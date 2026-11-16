//! Per-workspace `nix develop` environment resolution and caching.
//!
//! Rockstar runs as a single GUI process across workspaces (`:wsnew` and CLI
//! forwarding), so ambient process environment does not match newly opened Nix
//! projects. Each workspace resolves its devShell environment in the background
//! without blocking the UI or polluting child stdio with `shellHook` output.
use std::{
    collections::{HashMap, HashSet},
    env,
    ffi::{OsStr, OsString},
    fs,
    io::{self, Read},
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd},
        unix::{
            ffi::{OsStrExt, OsStringExt},
            net::UnixStream,
            process::CommandExt as _,
        },
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Condvar, Mutex},
    thread,
    time::SystemTime,
};

use anyhow::{Context as _, Result, bail, ensure};

const MAX_ENV_BYTES: usize = 16 * 1024 * 1024;
const MAX_STDERR_BYTES: usize = 16 * 1024;
const CAPTURE_SCRIPT: &str = r#"exec env -0 >&"$1""#;
const FALLBACK_NIX_PATHS: &[&str] = &[
    "/run/current-system/sw/bin/nix",
    "/nix/var/nix/profiles/default/bin/nix",
];

#[derive(Clone, Debug, PartialEq, Eq)]
enum Manifest {
    Flake { dir: PathBuf },
    Shell { dir: PathBuf, file: PathBuf },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileStamp {
    modified: Option<SystemTime>,
    len: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ManifestStamp {
    manifest: Manifest,
    primary: Option<FileStamp>,
    lockfile: Option<FileStamp>,
    git_index: Option<FileStamp>,
}

#[derive(Clone, Debug)]
pub(crate) struct EnvMap {
    vars: Vec<(OsString, OsString)>,
    path: Option<OsString>,
}

impl EnvMap {
    pub(crate) fn from_pairs(vars: Vec<(OsString, OsString)>) -> Self {
        let path = vars
            .iter()
            .find(|(key, _)| key.as_os_str() == OsStr::new("PATH"))
            .map(|(_, value)| value.clone());
        Self { vars, path }
    }

    #[cfg(test)]
    pub(crate) fn get(&self, key: impl AsRef<OsStr>) -> Option<&OsStr> {
        let key = key.as_ref();
        self.vars
            .iter()
            .find(|(k, _)| k.as_os_str() == key)
            .map(|(_, v)| v.as_os_str())
    }

    pub(crate) fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let resolved = resolve_program(program.as_ref(), self.path.as_deref());
        let mut command = Command::new(resolved);
        command.env_clear();
        command.envs(
            self.vars
                .iter()
                .map(|(k, v)| (k.as_os_str(), v.as_os_str())),
        );
        command
    }

    pub(crate) fn apply_pty(&self, command: &mut portable_pty::CommandBuilder) {
        if let Some(program) = command.get_argv().first().cloned() {
            let resolved = resolve_program(&program, self.path.as_deref());
            if let Some(slot) = command.get_argv_mut().first_mut() {
                *slot = resolved.into_os_string();
            }
        }
        command.env_clear();
        for (key, value) in &self.vars {
            command.env(key, value);
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum ResolutionOutcome {
    /// No Nix manifest applies to this workspace, or `nix` is not installed.
    Inherited,
    /// `nix develop` succeeded for this workspace.
    Activated(Arc<EnvMap>),
    /// A Nix manifest exists and `nix` ran, but `nix develop` failed.
    Failed(String),
}

impl ResolutionOutcome {
    pub(crate) fn command(&self, program: impl AsRef<OsStr>) -> Command {
        match self {
            Self::Activated(env) => env.command(program),
            Self::Inherited | Self::Failed(_) => Command::new(program),
        }
    }

    pub(crate) fn apply_pty(&self, command: &mut portable_pty::CommandBuilder) {
        if let Self::Activated(env) = self {
            env.apply_pty(command);
        }
    }

    pub(crate) fn failure(&self) -> Option<&str> {
        match self {
            Self::Failed(error) => Some(error.as_str()),
            Self::Inherited | Self::Activated(_) => None,
        }
    }
}

struct EnvState {
    computing: bool,
    cancelled: bool,
    generation: u64,
    active_pid: Option<u32>,
    cached: Option<(Option<ManifestStamp>, ResolutionOutcome)>,
}

pub(crate) struct WorkspaceEnv {
    root: PathBuf,
    nix_override: Option<PathBuf>,
    state: Mutex<EnvState>,
    ready: Condvar,
}

impl WorkspaceEnv {
    pub(crate) fn new(root: PathBuf) -> Arc<Self> {
        Self::with_nix_override(root, None)
    }

    fn with_nix_override(root: PathBuf, nix_override: Option<PathBuf>) -> Arc<Self> {
        Arc::new(Self {
            root,
            nix_override,
            state: Mutex::new(EnvState {
                computing: false,
                cancelled: false,
                generation: 0,
                active_pid: None,
                cached: None,
            }),
            ready: Condvar::new(),
        })
    }

    /// True if a previously failed consumer (such as a language server) has
    /// reason to retry because the workspace's Nix manifest state changed.
    pub(crate) fn should_retry_consumer(&self) -> bool {
        let current = current_stamp(&self.root);
        let state = self.state.lock().unwrap();
        match &state.cached {
            None => current.is_some(),
            Some((cached_stamp, outcome)) => {
                cached_stamp != &current
                    && (current.is_some() || matches!(outcome, ResolutionOutcome::Failed(_)))
            }
        }
    }

    pub(crate) fn cancel(&self) {
        let mut state = self.state.lock().unwrap();
        state.cancelled = true;
        if let Some(pid) = state.active_pid {
            kill_process_group(pid);
        }
        self.ready.notify_all();
    }

    pub(crate) fn resolve(&self) -> ResolutionOutcome {
        let current = current_stamp(&self.root);
        let mut state = self.state.lock().unwrap();
        if state.cancelled {
            return ResolutionOutcome::Inherited;
        }
        if state.computing {
            let start_gen = state.generation;
            while state.computing && state.generation == start_gen && !state.cancelled {
                state = self.ready.wait(state).unwrap();
            }
            if state.cancelled {
                return ResolutionOutcome::Inherited;
            }
            if let Some((_, outcome)) = &state.cached {
                return outcome.clone();
            }
        }
        if let Some((cached_stamp, outcome)) = &state.cached
            && cached_stamp == &current
            && !matches!(outcome, ResolutionOutcome::Failed(_))
        {
            return outcome.clone();
        }
        let Some(stamp) = current else {
            state.cached = Some((None, ResolutionOutcome::Inherited));
            return ResolutionOutcome::Inherited;
        };

        state.computing = true;
        drop(state);

        let outcome = match capture_nix_develop(
            &self.root,
            &stamp.manifest,
            self.nix_override.as_deref(),
            &self.state,
        ) {
            Ok(Some(env)) => ResolutionOutcome::Activated(Arc::new(env)),
            Ok(None) => ResolutionOutcome::Inherited,
            Err(error) => {
                if self.state.lock().unwrap().cancelled {
                    ResolutionOutcome::Inherited
                } else {
                    ResolutionOutcome::Failed(format!("{error:#}"))
                }
            }
        };

        // Re-read the manifest stamp on completion so any file changes made
        // while `nix develop` was running (or Nix updating flake.lock) match.
        let finished_stamp = current_stamp(&self.root).or(Some(stamp));
        let mut state = self.state.lock().unwrap();
        state.computing = false;
        state.active_pid = None;
        state.generation += 1;
        if !state.cancelled {
            state.cached = Some((finished_stamp, outcome.clone()));
        }
        self.ready.notify_all();
        outcome
    }
}

fn current_stamp(root: &Path) -> Option<ManifestStamp> {
    let home = env::var_os("HOME").map(PathBuf::from);
    let manifest = find_manifest(root, home.as_deref())?;
    let (dir, primary, lockfile) = match &manifest {
        Manifest::Flake { dir } => (
            dir.as_path(),
            file_stamp(&dir.join("flake.nix")),
            file_stamp(&dir.join("flake.lock")),
        ),
        Manifest::Shell { dir, file } => (dir.as_path(), file_stamp(file), None),
    };
    let git_index = find_git_index(root, dir).and_then(|index| file_stamp(&index));
    Some(ManifestStamp {
        manifest,
        primary,
        lockfile,
        git_index,
    })
}

fn find_manifest(root: &Path, home: Option<&Path>) -> Option<Manifest> {
    for dir in root.ancestors() {
        if dir != root && home == Some(dir) {
            break;
        }
        if dir.join("flake.nix").is_file() {
            return Some(Manifest::Flake {
                dir: dir.to_path_buf(),
            });
        }
        let shell = dir.join("shell.nix");
        if shell.is_file() {
            return Some(Manifest::Shell {
                dir: dir.to_path_buf(),
                file: shell,
            });
        }
        if dir.join(".git").exists() {
            break;
        }
    }
    None
}

fn find_git_index(root: &Path, manifest_dir: &Path) -> Option<PathBuf> {
    for dir in root.ancestors() {
        let dot_git = dir.join(".git");
        if dot_git.is_dir() {
            return Some(dot_git.join("index"));
        }
        if dot_git.is_file()
            && let Ok(contents) = fs::read_to_string(&dot_git)
            && let Some(raw) = contents
                .lines()
                .find_map(|line| line.strip_prefix("gitdir:"))
        {
            let git_dir = dir.join(raw.trim());
            return Some(git_dir.join("index"));
        }
        if dir == manifest_dir && dir.parent().is_none() {
            break;
        }
    }
    None
}

fn file_stamp(path: &Path) -> Option<FileStamp> {
    let meta = fs::metadata(path).ok()?;
    Some(FileStamp {
        modified: meta.modified().ok(),
        len: meta.len(),
    })
}

struct NixProcessGuard<'a> {
    child: Option<Child>,
    pid: u32,
    state: &'a Mutex<EnvState>,
}

impl Drop for NixProcessGuard<'_> {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            kill_process_group(self.pid);
            let _ = child.wait();
        }
        self.state.lock().unwrap().active_pid = None;
    }
}

fn kill_process_group(pid: u32) {
    if pid > 0 {
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
}

fn locate_nix(nix_override: Option<&Path>) -> PathBuf {
    if let Some(custom) = nix_override {
        return custom.to_path_buf();
    }
    let resolved = resolve_program(OsStr::new("nix"), env::var_os("PATH").as_deref());
    if resolved != Path::new("nix") {
        return resolved;
    }
    for candidate in FALLBACK_NIX_PATHS {
        let path = Path::new(candidate);
        if is_executable_file(path) {
            return path.to_path_buf();
        }
    }
    PathBuf::from("nix")
}

fn capture_nix_develop(
    root: &Path,
    manifest: &Manifest,
    nix_override: Option<&Path>,
    state: &Mutex<EnvState>,
) -> Result<Option<EnvMap>> {
    let ambient: Vec<(OsString, OsString)> = env::vars_os().collect();
    let nix_program = locate_nix(nix_override);

    let (mut env_reader, env_writer) = UnixStream::pair()?;
    let inherited_writer = duplicate_above_stdio(env_writer.as_fd())?;
    let env_fd = inherited_writer.as_raw_fd();
    let parent_pid = std::process::id() as libc::pid_t;

    let mut command = Command::new(&nix_program);
    command.args([
        "--extra-experimental-features",
        "nix-command flakes",
        "develop",
    ]);
    if let Manifest::Shell { file, .. } = manifest {
        command.args(["--impure", "--file"]).arg(file);
    }
    command
        .args(["--command", "sh", "-c", CAPTURE_SCRIPT, "rockstar-devshell"])
        .arg(env_fd.to_string())
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    // SAFETY: Only async-signal-safe syscalls run in the child before exec.
    unsafe {
        command.pre_exec(move || {
            if libc::setpgid(0, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            if libc::prctl(
                libc::PR_SET_PDEATHSIG,
                libc::SIGKILL as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
            ) == -1
            {
                return Err(io::Error::last_os_error());
            }
            if libc::getppid() != parent_pid {
                return Err(io::Error::from_raw_os_error(libc::ESRCH));
            }
            set_close_on_exec(env_fd, false)
        });
    }

    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("Cannot start nix develop"),
    };
    // Close parent copies of the write end so EOF is observed as soon as the child exits.
    drop((command, inherited_writer, env_writer));

    let pid = child.id();
    let mut guard = NixProcessGuard {
        child: Some(child),
        pid,
        state,
    };
    {
        let mut locked = state.lock().unwrap();
        if locked.cancelled {
            bail!("Workspace closed");
        }
        locked.active_pid = Some(pid);
    }

    let stderr = guard
        .child
        .as_mut()
        .and_then(|c| c.stderr.take())
        .expect("piped nix stderr");
    let stderr_reader = thread::spawn(move || read_stderr_tail(stderr));

    let mut raw_env = Vec::new();
    (&mut env_reader)
        .take((MAX_ENV_BYTES + 1) as u64)
        .read_to_end(&mut raw_env)
        .context("Cannot read nix develop environment")?;
    ensure!(
        raw_env.len() <= MAX_ENV_BYTES,
        "nix develop environment exceeds {} MiB",
        MAX_ENV_BYTES / (1024 * 1024)
    );

    let status = guard
        .child
        .take()
        .expect("live nix child")
        .wait()
        .context("Cannot wait for nix develop")?;
    let stderr_tail = stderr_reader.join().unwrap_or_default();

    if state.lock().unwrap().cancelled {
        bail!("Workspace closed");
    }
    if !status.success() {
        let detail = summarize_nix_error(&stderr_tail);
        if detail.is_empty() {
            bail!("exited with {status}");
        }
        bail!("{detail}");
    }
    ensure!(
        !raw_env.is_empty(),
        "nix develop exited without producing environment variables"
    );

    Ok(Some(sanitize_captured_env(&raw_env, &ambient)))
}

fn sanitize_captured_env(raw_env: &[u8], ambient: &[(OsString, OsString)]) -> EnvMap {
    let ambient_map: HashMap<&OsStr, &OsStr> = ambient
        .iter()
        .map(|(k, v)| (k.as_os_str(), v.as_os_str()))
        .collect();
    let mut parsed: Vec<(OsString, OsString)> = Vec::new();
    let mut index_by_key: HashMap<OsString, usize> = HashMap::new();

    for entry in raw_env.split(|&byte| byte == 0) {
        if entry.is_empty() {
            continue;
        }
        let Some(eq) = entry.iter().position(|&byte| byte == b'=') else {
            continue;
        };
        let key = OsString::from_vec(entry[..eq].to_vec());
        let value = OsString::from_vec(entry[eq + 1..].to_vec());
        if let Some(&slot) = index_by_key.get(&key) {
            parsed[slot].1 = value;
        } else {
            index_by_key.insert(key.clone(), parsed.len());
            parsed.push((key, value));
        }
    }

    // `nix develop` / `nix print-dev-env` creates an ephemeral `/tmp/nix-shell.*`
    // directory and assigns TMP/TMPDIR/TEMP/TEMPDIR to it. Remove it immediately
    // and restore the caller's temporary-directory settings so long-lived LSPs,
    // terminals, and agents never point at a directory tmpfiles may reap.
    let ephemeral_build_top = index_by_key
        .get(OsStr::new("NIX_BUILD_TOP"))
        .map(|&slot| parsed[slot].1.clone())
        .filter(|value| {
            ambient_map.get(OsStr::new("NIX_BUILD_TOP")).copied() != Some(value.as_os_str())
                && is_ephemeral_nix_shell_dir(Path::new(value))
        });
    if let Some(dir) = &ephemeral_build_top {
        let _ = fs::remove_dir_all(Path::new(dir));
    }

    // Preserve the user's login shell (e.g. fish/zsh) instead of Nix's stdenv bash.
    restore_or_unset(
        &mut parsed,
        &mut index_by_key,
        &ambient_map,
        "NIX_BUILD_TOP",
    );
    restore_or_unset(&mut parsed, &mut index_by_key, &ambient_map, "SHELL");
    restore_or_unset(&mut parsed, &mut index_by_key, &ambient_map, "terminfo");

    for temp_var in ["TMP", "TMPDIR", "TEMP", "TEMPDIR"] {
        let matches_ephemeral = ephemeral_build_top.as_ref().is_some_and(|ephemeral| {
            index_by_key
                .get(OsStr::new(temp_var))
                .is_some_and(|&slot| &parsed[slot].1 == ephemeral)
        });
        if matches_ephemeral {
            restore_or_unset(&mut parsed, &mut index_by_key, &ambient_map, temp_var);
        }
    }

    for transient in ["SHLVL", "PWD", "OLDPWD", "_"] {
        remove_var(&mut parsed, &mut index_by_key, OsStr::new(transient));
    }

    for list_var in ["PATH", "XDG_DATA_DIRS"] {
        if let Some(&slot) = index_by_key.get(OsStr::new(list_var)) {
            parsed[slot].1 = dedup_colon_list(&parsed[slot].1);
        }
    }

    parsed.retain(|(key, _)| !key.is_empty());
    EnvMap::from_pairs(parsed)
}

fn is_ephemeral_nix_shell_dir(path: &Path) -> bool {
    path.is_absolute()
        && path
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.starts_with("nix-shell."))
}

fn restore_or_unset(
    vars: &mut Vec<(OsString, OsString)>,
    index: &mut HashMap<OsString, usize>,
    ambient: &HashMap<&OsStr, &OsStr>,
    name: &str,
) {
    let key = OsStr::new(name);
    match ambient.get(key).copied() {
        Some(original) => set_var(vars, index, key, original),
        None => remove_var(vars, index, key),
    }
}

fn set_var(
    vars: &mut Vec<(OsString, OsString)>,
    index: &mut HashMap<OsString, usize>,
    key: &OsStr,
    value: &OsStr,
) {
    if let Some(&slot) = index.get(key) {
        vars[slot].1 = value.to_os_string();
    } else {
        index.insert(key.to_os_string(), vars.len());
        vars.push((key.to_os_string(), value.to_os_string()));
    }
}

fn remove_var(
    vars: &mut [(OsString, OsString)],
    index: &mut HashMap<OsString, usize>,
    key: &OsStr,
) {
    if let Some(slot) = index.remove(key) {
        vars[slot].0.clear();
        vars[slot].1.clear();
    }
}

fn dedup_colon_list(value: &OsStr) -> OsString {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for part in value.as_bytes().split(|&b| b == b':') {
        if part.is_empty() {
            continue;
        }
        let normalized = if part.len() > 1 && part.ends_with(b"/") {
            &part[..part.len() - 1]
        } else {
            part
        };
        if seen.insert(normalized.to_vec()) {
            if !out.is_empty() {
                out.push(b':');
            }
            out.extend_from_slice(normalized);
        }
    }
    OsString::from_vec(out)
}

pub(crate) fn resolve_program(program: &OsStr, path_env: Option<&OsStr>) -> PathBuf {
    if program.as_bytes().contains(&b'/') {
        return PathBuf::from(program);
    }
    if let Some(path_env) = path_env {
        for dir in env::split_paths(path_env) {
            if dir.as_os_str().is_empty() {
                continue;
            }
            let candidate = dir.join(program);
            if is_executable_file(&candidate) {
                return candidate;
            }
        }
    }
    PathBuf::from(program)
}

fn is_executable_file(path: &Path) -> bool {
    if !fs::metadata(path).is_ok_and(|meta| meta.is_file()) {
        return false;
    }
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    unsafe { libc::access(c_path.as_ptr(), libc::X_OK) == 0 }
}

fn read_stderr_tail(mut stderr: impl Read) -> String {
    let mut buf = [0u8; 1024];
    let mut tail = Vec::new();
    while let Ok(n) = stderr.read(&mut buf) {
        if n == 0 {
            break;
        }
        tail.extend_from_slice(&buf[..n]);
        if tail.len() > MAX_STDERR_BYTES {
            let excess = tail.len() - MAX_STDERR_BYTES;
            tail.drain(..excess);
        }
    }
    String::from_utf8_lossy(&tail).into_owned()
}

fn summarize_nix_error(stderr: &str) -> String {
    let stripped = strip_ansi(stderr);
    let focus = stripped
        .rfind("error:")
        .map(|idx| &stripped[idx + "error:".len()..])
        .unwrap_or(&stripped);
    let collapsed = focus.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = collapsed.chars();
    let summary: String = chars.by_ref().take(240).collect();
    if chars.next().is_some() {
        format!("{summary}…")
    } else {
        summary
    }
}

fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn duplicate_above_stdio(fd: BorrowedFd<'_>) -> io::Result<OwnedFd> {
    let duplicate = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
    if duplicate == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
}

fn set_close_on_exec(fd: RawFd, enabled: bool) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    let flags = if enabled {
        flags | libc::FD_CLOEXEC
    } else {
        flags & !libc::FD_CLOEXEC
    };
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(prefix: &str) -> Self {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp");
            fs::create_dir_all(&base).unwrap();
            let dir = base.join(format!(
                "{prefix}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            // Bound ancestor walks inside `target/test-tmp` so Rockstar's own flake.nix isn't matched.
            fs::create_dir_all(dir.join(".git")).unwrap();
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_executable(path: &Path, script: &str) {
        fs::write(path, script).unwrap();
        let mut perms = fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).unwrap();
    }

    #[test]
    fn manifest_discovery_respects_git_and_home_boundaries() {
        let repo = TempDir::new("manifest");
        let sub = repo.path().join("packages/app");
        fs::create_dir_all(&sub).unwrap();

        // Plain git repository without flake.nix or shell.nix does not escape above `.git`.
        assert_eq!(find_manifest(&sub, None), None);

        // Monorepo root with flake.nix is found from nested directories.
        fs::write(repo.path().join("flake.nix"), "{}").unwrap();
        assert_eq!(
            find_manifest(&sub, None),
            Some(Manifest::Flake {
                dir: repo.path().to_path_buf(),
            })
        );

        // Home boundary stops parent search when opening a non-Nix child under $HOME,
        // while still allowing opening $HOME itself if $HOME/flake.nix exists.
        fs::remove_dir_all(repo.path().join(".git")).unwrap();
        assert_eq!(find_manifest(&sub, Some(repo.path())), None);
        assert_eq!(
            find_manifest(repo.path(), Some(repo.path())),
            Some(Manifest::Flake {
                dir: repo.path().to_path_buf(),
            })
        );
    }

    #[test]
    fn sanitize_env_cleans_ephemeral_tmpdir_and_preserves_login_shell() {
        let scratch = TempDir::new("sanitize");
        let fake_nix_tmp = scratch.path().join("nix-shell.abc123");
        fs::create_dir_all(&fake_nix_tmp).unwrap();

        let mut raw = Vec::new();
        for (k, v) in [
            (
                "PATH",
                "/nix/store/hls/bin:/usr/bin/:/nix/store/hls/bin:/bin",
            ),
            ("SHELL", "/nix/store/bash/bin/bash"),
            ("NIX_BUILD_TOP", fake_nix_tmp.to_str().unwrap()),
            ("TMPDIR", fake_nix_tmp.to_str().unwrap()),
            ("TMP", fake_nix_tmp.to_str().unwrap()),
            ("TEMP", "/custom/preserved"),
            ("NIX_GHC_LIBDIR", "/nix/store/ghc/lib"),
            ("PWD", "/some/dir"),
            ("SHLVL", "2"),
        ] {
            raw.extend_from_slice(k.as_bytes());
            raw.push(b'=');
            raw.extend_from_slice(v.as_bytes());
            raw.push(0);
        }

        let ambient = vec![
            (OsString::from("SHELL"), OsString::from("/run/sw/bin/fish")),
            (OsString::from("TMPDIR"), OsString::from("/run/user/1000")),
        ];
        let env = sanitize_captured_env(&raw, &ambient);

        assert!(
            !fake_nix_tmp.exists(),
            "ephemeral nix-shell dir must be deleted"
        );
        assert_eq!(env.get("SHELL"), Some(OsStr::new("/run/sw/bin/fish")));
        assert_eq!(env.get("TMPDIR"), Some(OsStr::new("/run/user/1000")));
        assert_eq!(env.get("TMP"), None);
        assert_eq!(env.get("TEMP"), Some(OsStr::new("/custom/preserved")));
        assert_eq!(env.get("NIX_BUILD_TOP"), None);
        assert_eq!(env.get("PWD"), None);
        assert_eq!(env.get("SHLVL"), None);
        assert_eq!(
            env.get("PATH"),
            Some(OsStr::new("/nix/store/hls/bin:/usr/bin:/bin"))
        );
        assert_eq!(
            env.get("NIX_GHC_LIBDIR"),
            Some(OsStr::new("/nix/store/ghc/lib"))
        );
    }

    #[test]
    fn workspace_env_ignores_shellhook_stdout_and_resolves_lsp_binaries() {
        let workspace = TempDir::new("devshell");
        fs::write(workspace.path().join("flake.nix"), "{}").unwrap();

        let bin_dir = workspace.path().join("devshell-bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let hls = bin_dir.join("haskell-language-server-wrapper");
        write_executable(&hls, "#!/bin/sh\nprintf 'hls-ok:%s' \"$HLS_EXTRA\"\n");

        let count_file = workspace.path().join("invocations");
        let mock_nix = workspace.path().join("mock-nix");
        write_executable(
            &mock_nix,
            &format!(
                "#!/bin/sh\n\
                 printf 'x' >> '{}'\n\
                 while [ $# -gt 0 ]; do\n\
                   if [ \"$1\" = \"--command\" ]; then shift; break; fi\n\
                   shift\n\
                 done\n\
                 printf 'noisy shellHook banner=123\\n'\n\
                 export PATH='{}:'\"$PATH\"\n\
                 export HLS_EXTRA='from-flake'\n\
                 exec \"$@\"\n",
                count_file.display(),
                bin_dir.display(),
            ),
        );

        let devshell =
            WorkspaceEnv::with_nix_override(workspace.path().to_path_buf(), Some(mock_nix));
        let first = devshell.resolve();
        let second = devshell.resolve();
        assert!(matches!(first, ResolutionOutcome::Activated(_)));
        assert!(matches!(second, ResolutionOutcome::Activated(_)));
        assert_eq!(
            fs::read_to_string(&count_file).unwrap().len(),
            1,
            "second resolve with unchanged flake.nix must use cache"
        );

        let output = first
            .command("haskell-language-server-wrapper")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout), "hls-ok:from-flake");
    }

    #[test]
    fn concurrent_resolvers_share_single_inflight_nix_develop_and_retry_after_failure() {
        let workspace = TempDir::new("concurrent");
        let flake = workspace.path().join("flake.nix");
        fs::write(&flake, "{ broken }").unwrap();

        let count_file = workspace.path().join("count");
        let gate_file = workspace.path().join("ok");
        let mock_nix = workspace.path().join("mock-nix");
        write_executable(
            &mock_nix,
            &format!(
                "#!/bin/sh\n\
                 printf '1' >> '{}'\n\
                 sleep 0.05\n\
                 if [ ! -f '{}' ]; then\n\
                   printf 'error: flake evaluation failed\\n' >&2\n\
                   exit 1\n\
                 fi\n\
                 while [ $# -gt 0 ]; do\n\
                   if [ \"$1\" = \"--command\" ]; then shift; break; fi\n\
                   shift\n\
                 done\n\
                 export DEVSHELL_READY=yes\n\
                 exec \"$@\"\n",
                count_file.display(),
                gate_file.display(),
            ),
        );

        let devshell =
            WorkspaceEnv::with_nix_override(workspace.path().to_path_buf(), Some(mock_nix));

        // Two threads resolving concurrently share a single `nix develop` run.
        let d1 = devshell.clone();
        let d2 = devshell.clone();
        let t1 = thread::spawn(move || d1.resolve());
        let t2 = thread::spawn(move || d2.resolve());
        let r1 = t1.join().unwrap();
        let r2 = t2.join().unwrap();
        assert_eq!(r1.failure(), Some("flake evaluation failed"));
        assert_eq!(r2.failure(), Some("flake evaluation failed"));
        assert_eq!(fs::read_to_string(&count_file).unwrap().len(), 1);

        // Editing flake.nix marks failed consumers eligible for retry and succeeds.
        fs::write(&gate_file, "ok").unwrap();
        fs::write(&flake, "{ fixed = true; }").unwrap();
        assert!(devshell.should_retry_consumer());

        let recovered = devshell.resolve();
        match recovered {
            ResolutionOutcome::Activated(env) => {
                assert_eq!(env.get("DEVSHELL_READY"), Some(OsStr::new("yes")));
            }
            other => panic!("expected Activated, got {other:?}"),
        }
        assert!(!devshell.should_retry_consumer());
    }

    #[test]
    fn summarize_nix_error_extracts_final_error_clause() {
        let stderr = "\x1b[31;1merror:\x1b[0m\n       … while evaluating 'devShells'\n\n       error: Path 'flake.nix' is not tracked by Git.\n";
        assert_eq!(
            summarize_nix_error(stderr),
            "Path 'flake.nix' is not tracked by Git."
        );
    }
}
