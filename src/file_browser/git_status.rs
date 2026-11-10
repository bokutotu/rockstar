//! Git decorations are computed off the UI thread, relative to the tree root.
use anyhow::{Context as _, Result, ensure};
use notify::{Event, EventKind};
use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    os::unix::ffi::OsStringExt as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Changes {
    pub branch: bool,
    pub uncommitted: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct GitStatus {
    pub paths: HashMap<String, Changes>,
    pub base: Option<String>,
    pub watch_directories: HashSet<PathBuf>,
    pub error: Option<String>,
}

impl GitStatus {
    pub(super) fn affects(&self, root: &Path, event: &Event) -> bool {
        !matches!(event.kind, EventKind::Access(_))
            && (event.need_rescan()
                || event.paths.is_empty()
                || event.paths.iter().any(|path| {
                    path.starts_with(root)
                        || self
                            .watch_directories
                            .iter()
                            .any(|dir| path.starts_with(dir))
                }))
    }

    fn mark(&mut self, root: &Path, repository: &Path, path: PathBuf, branch: bool) {
        let absolute = repository.join(path);
        let Ok(relative) = absolute.strip_prefix(root) else {
            return;
        };
        let path = relative.to_string_lossy().into_owned();
        // Aggregate both flags independently so a folder may contain both kinds.
        for end in path
            .match_indices('/')
            .map(|(i, _)| i + 1)
            .chain(std::iter::once(path.len()))
        {
            let status = self.paths.entry(path[..end].to_owned()).or_default();
            if branch {
                status.branch = true;
            } else {
                status.uncommitted = true;
            }
        }
    }
}

fn git(root: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(root)
        .args(["--no-pager", "--literal-pathspecs", "-c", "color.ui=false"])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    for name in [
        "GIT_DIR",
        "GIT_COMMON_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_PREFIX",
    ] {
        command.env_remove(name);
    }
    command
}

fn checked(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = git(root).args(args).output()?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output.stdout)
}

fn optional(root: &Path, args: &[&str]) -> Option<String> {
    let output = git(root).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn nul_paths(data: &[u8]) -> impl Iterator<Item = PathBuf> + '_ {
    data.split(|b| *b == 0)
        .filter(|path| !path.is_empty())
        .map(|path| PathBuf::from(OsString::from_vec(path.to_vec())))
}

fn status_paths(data: &[u8]) -> Result<Vec<PathBuf>> {
    let mut fields = data
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty());
    let mut paths = Vec::new();
    while let Some(record) = fields.next() {
        ensure!(
            record.len() > 3 && record[2] == b' ',
            "Invalid git status record"
        );
        paths.push(PathBuf::from(OsString::from_vec(record[3..].to_vec())));
        if record[..2]
            .iter()
            .any(|status| matches!(status, b'R' | b'C'))
        {
            let source = fields.next().context("Missing git rename source")?;
            paths.push(PathBuf::from(OsString::from_vec(source.to_vec())));
        }
    }
    Ok(paths)
}

fn watch_refs(directory: PathBuf, targets: &mut HashSet<PathBuf>) {
    if !targets.insert(directory.clone()) {
        return;
    }
    if let Ok(entries) = std::fs::read_dir(directory) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                watch_refs(entry.path(), targets);
            }
        }
    }
}

pub(super) fn load(root: &Path) -> Result<GitStatus> {
    let output = git(root).args(["rev-parse", "--show-toplevel"]).output()?;
    if !output.status.success() {
        return Ok(GitStatus::default());
    }
    let repository = PathBuf::from(OsString::from_vec(
        output
            .stdout
            .strip_suffix(b"\n")
            .unwrap_or(&output.stdout)
            .to_vec(),
    ));
    let mut status = GitStatus::default();
    for argument in ["--git-dir", "--git-common-dir"] {
        let data = checked(root, &["rev-parse", "--path-format=absolute", argument])?;
        let dir = PathBuf::from(OsString::from_vec(
            data.strip_suffix(b"\n").unwrap_or(&data).to_vec(),
        ));
        status.watch_directories.insert(dir.clone());
        watch_refs(dir.join("refs"), &mut status.watch_directories);
    }
    let data = checked(
        &repository,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ],
    )?;
    for path in status_paths(&data)? {
        status.mark(root, &repository, path, false);
    }
    if optional(&repository, &["rev-parse", "--verify", "HEAD"]).is_none() {
        return Ok(status); // A new repository still has staged/untracked decorations.
    }
    let configured = optional(&repository, &["config", "--get", "rockstar.baseBranch"]);
    let candidates = if let Some(base) = configured.as_ref() {
        vec![base.clone()]
    } else {
        let mut candidates = Vec::new();
        for remote in ["origin", "upstream"] {
            if let Some(base) = optional(
                &repository,
                &[
                    "symbolic-ref",
                    "--quiet",
                    &format!("refs/remotes/{remote}/HEAD"),
                ],
            ) {
                candidates.push(base);
            }
        }
        candidates.extend(
            [
                "refs/heads/main",
                "refs/heads/master",
                "refs/heads/trunk",
                "refs/remotes/origin/main",
                "refs/remotes/origin/master",
            ]
            .map(str::to_owned),
        );
        candidates
    };
    for base in candidates {
        let Some(oid) = optional(
            &repository,
            &[
                "rev-parse",
                "--verify",
                "--end-of-options",
                &format!("{base}^{{commit}}"),
            ],
        ) else {
            continue;
        };
        let Some(merge_base) = optional(&repository, &["merge-base", "HEAD", &oid]) else {
            continue;
        };
        let data = checked(
            &repository,
            &[
                "diff",
                "--name-only",
                "-z",
                "--no-renames",
                "--no-ext-diff",
                "--no-textconv",
                &merge_base,
                "HEAD",
                "--",
            ],
        )?;
        for path in nul_paths(&data) {
            status.mark(root, &repository, path, true);
        }
        status.base = Some(
            base.strip_prefix("refs/heads/")
                .or_else(|| base.strip_prefix("refs/remotes/"))
                .unwrap_or(&base)
                .to_owned(),
        );
        return Ok(status);
    }
    if let Some(base) = configured {
        status.error = Some(format!("Cannot resolve base branch: {base}"));
    }
    Ok(status)
}
