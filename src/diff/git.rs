//! The only module that runs Git/gh or reads working-tree files.
use super::model::*;
use anyhow::{Context as _, Result, bail, ensure};
use std::{
    ffi::{OsStr, OsString},
    fs,
    io::Read,
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::PermissionsExt,
    },
    path::{Component, Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::Mutex,
};

const MAX_TEXT_BYTES: u64 = 16 * 1024 * 1024;
// Serialize index writes from separate diff sessions; Git also locks against
// other processes. Reads do not hold this lock.
static INDEX_WRITES: Mutex<()> = Mutex::new(());

fn git(root: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(root)
        .args(["--no-pager", "--literal-pathspecs", "-c", "color.ui=false"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(Stdio::null());
    // A workspace always addresses its own worktree, not an inherited Git
    // command's alternate index or repository.
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

fn checked(command: &mut Command) -> Result<Vec<u8>> {
    let name = command.get_program().to_string_lossy().into_owned();
    let output = command
        .output()
        .with_context(|| format!("Could not run {name}"))?;
    ensure_success(&name, output)
}

fn ensure_success(name: &str, output: Output) -> Result<Vec<u8>> {
    ensure!(
        output.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output.stdout)
}

fn text(bytes: Vec<u8>) -> Result<String> {
    Ok(String::from_utf8(bytes)?.trim().to_owned())
}

fn oid(value: &str) -> Result<()> {
    ensure!(
        (value.len() == 40 || value.len() == 64) && value.bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid Git object ID"
    );
    Ok(())
}

fn path(bytes: &[u8]) -> Result<PathBuf> {
    let path = PathBuf::from(OsString::from_vec(bytes.to_vec()));
    ensure!(
        !path.as_os_str().is_empty()
            && path.components().all(|c| matches!(c, Component::Normal(_))),
        "Invalid repository-relative path"
    );
    Ok(path)
}

pub(super) fn repository_root(directory: &Path) -> Result<PathBuf> {
    let bytes = checked(git(directory).args(["rev-parse", "--show-toplevel"]))
        .context("The workspace is not inside a Git working tree")?;
    // Remove the command's newline, not whitespace in the directory name.
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
    Ok(PathBuf::from(OsString::from_vec(bytes.to_vec())).canonicalize()?)
}

pub(super) fn load(directory: &Path, request: DiffRequest) -> Result<DiffSnapshot> {
    let root = repository_root(directory)?;
    match request {
        DiffRequest::Local(filter) => load_local_diff(&root, filter),
        DiffRequest::PullRequest => load_pr_diff(&root),
    }
}

fn changes(root: &Path, args: &[&str], section: DiffSection) -> Result<Vec<DiffEntry>> {
    let data = checked(
        git(root)
            .args([
                "diff",
                "--name-status",
                "-z",
                "--find-renames",
                "--no-relative",
                "--no-ext-diff",
                "--no-textconv",
                "--ignore-submodules=dirty",
            ])
            .args(args)
            .arg("--"),
    )?;
    let mut fields = data.split(|byte| *byte == 0);
    let mut entries = Vec::new();
    while let Some(status) = fields.next() {
        if status.is_empty() {
            continue;
        }
        let kind = match status[0] {
            b'A' => ChangeKind::Added,
            b'D' => ChangeKind::Deleted,
            b'M' => ChangeKind::Modified,
            b'R' => ChangeKind::Renamed,
            b'C' => ChangeKind::Copied,
            b'T' => ChangeKind::TypeChanged,
            b'U' => ChangeKind::Conflict,
            _ => bail!(
                "Unsupported Git change status: {}",
                String::from_utf8_lossy(status)
            ),
        };
        let first = path(fields.next().context("Missing Git change path")?)?;
        let (old_path, new_path) = match kind {
            ChangeKind::Added => (None, Some(first)),
            ChangeKind::Deleted => (Some(first), None),
            ChangeKind::Renamed | ChangeKind::Copied => (
                Some(first),
                Some(path(fields.next().context("Missing rename destination")?)?),
            ),
            _ => (Some(first.clone()), Some(first)),
        };
        entries.push(DiffEntry {
            section,
            old_path,
            new_path,
            kind,
        });
    }
    // Git may emit both U and M for an unresolved path. Never let the M entry
    // turn the conflict into an apparently stageable ordinary modification.
    let conflicts = entries
        .iter()
        .filter(|e| e.kind == ChangeKind::Conflict)
        .map(|e| e.path().to_path_buf())
        .collect::<Vec<_>>();
    entries.retain(|e| e.kind == ChangeKind::Conflict || !conflicts.iter().any(|p| p == e.path()));
    Ok(entries)
}

pub(super) fn load_local_diff(root: &Path, filter: LocalFilter) -> Result<DiffSnapshot> {
    let mut entries = Vec::new();
    if filter != LocalFilter::Unstaged {
        // With no explicit revision, --cached also supports an unborn HEAD.
        entries.extend(changes(root, &["--cached"], DiffSection::Staged)?);
    }
    if filter != LocalFilter::Staged {
        entries.extend(changes(root, &[], DiffSection::Unstaged)?);
        let data = checked(git(root).args(["ls-files", "--others", "--exclude-standard", "-z"]))?;
        for item in data
            .split(|byte| *byte == 0)
            .filter(|item| !item.is_empty())
        {
            // An untracked embedded repository is reported with a trailing '/'.
            // Keep it a file-level entry; Git itself decides how to add it.
            let item = item.strip_suffix(b"/").unwrap_or(item);
            entries.push(DiffEntry {
                section: DiffSection::Unstaged,
                old_path: None,
                new_path: Some(path(item)?),
                kind: ChangeKind::Untracked,
            });
        }
    }
    entries.sort_by(|a, b| {
        a.section
            .cmp(&b.section)
            .then_with(|| a.path().cmp(b.path()))
    });
    Ok(DiffSnapshot {
        root: root.to_path_buf(),
        source: DiffSource::Local(filter),
        entries,
    })
}

#[derive(serde::Deserialize)]
struct PrInfo {
    number: u64,
    #[serde(rename = "baseRefOid")]
    base: String,
    #[serde(rename = "headRefOid")]
    head: String,
    url: String,
}

fn load_pr_diff(root: &Path) -> Result<DiffSnapshot> {
    let data = checked(
        Command::new("gh")
            .current_dir(root)
            .env("GH_PROMPT_DISABLED", "1")
            .stdin(Stdio::null())
            .args(["pr", "view", "--json", "number,baseRefOid,headRefOid,url"]),
    )?;
    let pr: PrInfo = serde_json::from_slice(&data).context("Invalid PR information from gh")?;
    oid(&pr.base)?;
    oid(&pr.head)?;
    ensure!(pr.number > 0, "Invalid PR number");
    let mut repository = url::Url::parse(&pr.url)?;
    ensure!(
        repository.scheme() == "https"
            && repository.host_str().is_some()
            && repository.username().is_empty()
            && repository.password().is_none(),
        "Invalid PR URL"
    );
    let suffix = format!("/pull/{}", pr.number);
    let repository_path = repository
        .path()
        .strip_suffix(&suffix)
        .context("Invalid PR URL path")?;
    ensure!(
        repository_path.split('/').filter(|s| !s.is_empty()).count() == 2,
        "Invalid PR repository path"
    );
    let repository_path = format!("{repository_path}.git");
    repository.set_path(&repository_path);
    repository.set_query(None);
    repository.set_fragment(None);
    // Fetch the exact snapshot from the base repository (also exposes fork PR
    // heads). Do not update any branch, FETCH_HEAD, tag, submodule, or worktree.
    checked(git(root).args([
        "-c",
        "credential.helper=",
        "-c",
        "credential.helper=!gh auth git-credential",
        "fetch",
        "--no-tags",
        "--no-write-fetch-head",
        "--no-recurse-submodules",
        "--no-auto-maintenance",
        repository.as_str(),
        &pr.base,
        &pr.head,
    ]))?;
    let merge_base = text(
        checked(git(root).args(["merge-base", &pr.base, &pr.head])).context(
            "PR commits have no available common ancestor; a shallow clone may need deepening",
        )?,
    )?;
    oid(&merge_base)?;
    refresh_diff(&DiffSnapshot {
        root: root.to_path_buf(),
        source: DiffSource::PullRequest {
            number: pr.number,
            base_oid: pr.base,
            head_oid: pr.head,
            merge_base_oid: merge_base,
        },
        entries: Vec::new(),
    })
}

pub(super) fn refresh_diff(snapshot: &DiffSnapshot) -> Result<DiffSnapshot> {
    match &snapshot.source {
        DiffSource::Local(filter) => load_local_diff(&snapshot.root, *filter),
        DiffSource::PullRequest {
            head_oid,
            merge_base_oid,
            ..
        } => Ok(DiffSnapshot {
            root: snapshot.root.clone(),
            source: snapshot.source.clone(),
            entries: changes(
                &snapshot.root,
                &[merge_base_oid, head_oid],
                DiffSection::PullRequest,
            )?,
        }),
    }
}

fn head(root: &Path) -> Result<Option<String>> {
    let output = git(root)
        .args(["rev-parse", "--verify", "--quiet", "HEAD^{commit}"])
        .output()?;
    if output.status.code() == Some(1) {
        return Ok(None);
    }
    let value = text(ensure_success("git rev-parse HEAD", output)?)?;
    oid(&value)?;
    Ok(Some(value))
}

fn blob(root: &Path, mode: u32, object: &str) -> Result<FileVersion> {
    oid(object)?;
    if mode == 0o160000 {
        return Ok(FileVersion {
            mode: Some(mode),
            content: FileContent::Gitlink(object.to_owned()),
        });
    }
    let size: u64 = text(checked(git(root).args(["cat-file", "-s", object]))?)?.parse()?;
    ensure!(
        size <= MAX_TEXT_BYTES,
        "File exceeds the 16 MiB diff display limit (staging is still available)"
    );
    let bytes = checked(git(root).args(["cat-file", "blob", object]))?;
    Ok(file_version(mode, bytes))
}

fn file_version(mode: u32, bytes: Vec<u8>) -> FileVersion {
    let content = if bytes.contains(&0) {
        FileContent::Binary
    } else {
        String::from_utf8(bytes)
            .map(FileContent::Text)
            .unwrap_or(FileContent::Binary)
    };
    FileVersion {
        mode: Some(mode),
        content,
    }
}

fn tree_file(root: &Path, revision: &str, file: &Path) -> Result<FileVersion> {
    let data = checked(git(root).args(["ls-tree", "-z", revision, "--"]).arg(file))?;
    if data.is_empty() {
        return Ok(FileVersion::missing());
    }
    let metadata = data
        .split(|byte| *byte == b'\t')
        .next()
        .context("Invalid Git tree entry")?;
    let fields = std::str::from_utf8(metadata)?
        .split_whitespace()
        .collect::<Vec<_>>();
    ensure!(fields.len() == 3, "Invalid Git tree entry");
    blob(root, u32::from_str_radix(fields[0], 8)?, fields[2])
}

fn index_file(root: &Path, file: &Path) -> Result<FileVersion> {
    let data = checked(
        git(root)
            .args(["ls-files", "--stage", "-z", "--"])
            .arg(file),
    )?;
    // A literal pathspec can still match descendants of a directory. Only
    // accept the exact file, never the first child returned by ls-files.
    let metadata = data.split(|byte| *byte == 0).find_map(|record| {
        let tab = record.iter().position(|byte| *byte == b'\t')?;
        (&record[tab + 1..] == file.as_os_str().as_bytes()).then_some(&record[..tab])
    });
    let Some(metadata) = metadata else {
        return Ok(FileVersion::missing());
    };
    let fields = std::str::from_utf8(metadata)?
        .split_whitespace()
        .collect::<Vec<_>>();
    ensure!(
        fields.len() == 3 && fields[2] == "0",
        "Unresolved conflict; refresh the diff after resolving it externally"
    );
    blob(root, u32::from_str_radix(fields[0], 8)?, fields[1])
}

fn worktree_file(root: &Path, file: &Path) -> Result<FileVersion> {
    let absolute = root.join(file);
    let metadata = match fs::symlink_metadata(&absolute) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(FileVersion::missing());
        }
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() {
        return Ok(file_version(
            0o120000,
            fs::read_link(absolute)?.into_os_string().into_vec(),
        ));
    }
    if metadata.is_dir() {
        // Deinitialized submodules have no checkout: preserve the index OID.
        if !absolute.join(".git").exists() {
            let version = index_file(root, file)?;
            ensure!(
                matches!(version.content, FileContent::Gitlink(_)),
                "The selected path is now a directory; refresh the diff"
            );
            return Ok(version);
        }
        let commit = head(&absolute)?.context("Submodule has no commit")?;
        return Ok(FileVersion {
            mode: Some(0o160000),
            content: FileContent::Gitlink(commit),
        });
    }
    ensure!(
        metadata.is_file(),
        "Only regular files, symlinks, and submodules can be displayed"
    );
    ensure!(
        metadata.len() <= MAX_TEXT_BYTES,
        "File exceeds the 16 MiB diff display limit (staging is still available)"
    );
    let mode = if metadata.permissions().mode() & 0o111 != 0 {
        0o100755
    } else {
        0o100644
    };
    let mut bytes = Vec::new();
    fs::File::open(absolute)?
        .take(MAX_TEXT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_TEXT_BYTES,
        "File grew beyond the diff display limit"
    );
    Ok(file_version(mode, bytes))
}

pub(super) fn load_document(snapshot: &DiffSnapshot, entry: &DiffEntry) -> Result<DiffDocument> {
    ensure!(
        entry.kind != ChangeKind::Conflict,
        "Unresolved conflict. Three-way comparison and conflict staging are not supported; resolve it externally, then press r."
    );
    let root = &snapshot.root;
    let before = match (&snapshot.source, entry.section, &entry.old_path) {
        (_, _, None) => FileVersion::missing(),
        (DiffSource::Local(_), DiffSection::Staged, Some(file)) => match head(root)? {
            Some(head) => tree_file(root, &head, file)?,
            None => FileVersion::missing(),
        },
        (DiffSource::Local(_), DiffSection::Unstaged, Some(file)) => index_file(root, file)?,
        (DiffSource::PullRequest { merge_base_oid, .. }, DiffSection::PullRequest, Some(file)) => {
            tree_file(root, merge_base_oid, file)?
        }
        _ => bail!("Invalid diff comparison"),
    };
    let after = match (&snapshot.source, entry.section, &entry.new_path) {
        (_, _, None) => FileVersion::missing(),
        (DiffSource::Local(_), DiffSection::Staged, Some(file)) => index_file(root, file)?,
        (DiffSource::Local(_), DiffSection::Unstaged, Some(file)) => worktree_file(root, file)?,
        (DiffSource::PullRequest { head_oid, .. }, DiffSection::PullRequest, Some(file)) => {
            tree_file(root, head_oid, file)?
        }
        _ => bail!("Invalid diff comparison"),
    };
    Ok(DiffDocument::new(before, after))
}

pub(super) fn validate_action(
    snapshot: &DiffSnapshot,
    entry: &DiffEntry,
    action: IndexAction,
) -> Result<()> {
    ensure!(
        matches!(snapshot.source, DiffSource::Local(_)),
        "PR diffs are read-only; open :diff to stage local changes"
    );
    ensure!(
        entry.kind != ChangeKind::Conflict,
        "Resolve conflicts externally before staging or unstaging"
    );
    let section = match action {
        IndexAction::Stage => DiffSection::Unstaged,
        IndexAction::Unstage => DiffSection::Staged,
    };
    ensure!(
        entry.section == section,
        "Select a file in {} first",
        section.label()
    );
    Ok(())
}

pub(super) fn update_index(
    snapshot: &DiffSnapshot,
    entry: &DiffEntry,
    action: IndexAction,
) -> Result<()> {
    validate_action(snapshot, entry, action)?;
    let _guard = INDEX_WRITES
        .lock()
        .map_err(|_| anyhow::anyhow!("Git index writer unavailable"))?;
    let current = load_local_diff(&snapshot.root, LocalFilter::All)?;
    ensure!(
        current.entries.contains(entry),
        "The selected change has moved or disappeared; refresh and select it again"
    );
    let paths: Vec<&OsStr> = if entry.kind == ChangeKind::Renamed {
        entry
            .old_path
            .iter()
            .chain(entry.new_path.iter())
            .map(|p| p.as_os_str())
            .collect()
    } else {
        vec![entry.path().as_os_str()]
    };
    // Git pathspecs also recurse through directories, even with --literal-pathspecs.
    // Do not accidentally mutate neighbouring entries in a file/directory replacement.
    for file in &paths {
        let tracked = checked(git(&snapshot.root).args(["ls-files", "-z", "--"]).arg(file))?;
        ensure!(
            tracked
                .split(|b| *b == 0)
                .filter(|p| !p.is_empty())
                .all(|p| p == file.as_bytes()),
            "This path now contains other tracked files; handle the directory/file replacement with Git directly"
        );
    }
    match action {
        IndexAction::Stage if entry.kind == ChangeKind::Deleted => {
            checked(
                git(&snapshot.root)
                    .args(["update-index", "--force-remove", "--"])
                    .args(paths),
            )?;
        }
        IndexAction::Stage => {
            for file in &paths {
                let absolute = snapshot.root.join(file);
                if fs::symlink_metadata(&absolute).is_ok_and(|m| m.is_dir()) {
                    ensure!(
                        absolute.join(".git").exists()
                            || matches!(
                                index_file(&snapshot.root, Path::new(file))?.content,
                                FileContent::Gitlink(_)
                            ),
                        "The selected path is now a directory; refresh the diff"
                    );
                }
            }
            checked(git(&snapshot.root).args(["add", "--all", "--"]).args(paths))?;
        }
        IndexAction::Unstage => {
            if let Some(head) = head(&snapshot.root)? {
                for file in &paths {
                    let tree = checked(
                        git(&snapshot.root)
                            .args(["ls-tree", "-z", &head, "--"])
                            .arg(file),
                    )?;
                    ensure!(
                        !tree.starts_with(b"040000 "),
                        "HEAD contains a directory at this path; handle the directory/file replacement with Git directly"
                    );
                }
                checked(
                    git(&snapshot.root)
                        .args(["restore", "--staged", "--source"])
                        .arg(head)
                        .arg("--")
                        .args(paths),
                )?;
            } else {
                checked(
                    git(&snapshot.root)
                        .args(["rm", "--cached", "--force", "--ignore-unmatch", "--"])
                        .args(paths),
                )?;
            }
        }
    }
    Ok(())
}
