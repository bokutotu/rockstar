//! Tree snapshots and tree-specific change filtering. OS watches live in `fs_watch`.
use anyhow::{Context as _, Result};
use notify::{Event, EventKind, event::ModifyKind};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

pub(super) struct TreeSnapshot {
    pub paths: Vec<String>,
    pub watch_directories: HashSet<PathBuf>,
    pub git: super::git_status::GitStatus,
}

pub(super) fn scan_tree(root: &Path) -> Result<TreeSnapshot> {
    let mut snapshot = TreeSnapshot {
        paths: Vec::new(),
        git: Default::default(),
        // The parent covers removal/replacement of the root. FsWatch resolves
        // missing directories to their nearest surviving ancestor.
        watch_directories: HashSet::from([root.parent().unwrap_or(root).to_path_buf()]),
    };
    match fs::metadata(root) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Ok(snapshot),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(snapshot),
        Err(error) => return Err(error).with_context(|| format!("Cannot scan {}", root.display())),
    }
    for entry in ignore::WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .follow_links(false)
        .filter_entry(|entry| {
            entry.depth() == 0 || !entry.file_name().as_encoded_bytes().starts_with(b".")
        })
        .build()
    {
        let entry = entry.with_context(|| format!("Cannot scan {}", root.display()))?;
        let directory = entry.file_type().is_some_and(|kind| kind.is_dir());
        if directory {
            snapshot
                .watch_directories
                .insert(entry.path().to_path_buf());
        }
        if entry.depth() == 0 {
            continue;
        }
        let relative = entry.path().strip_prefix(root)?;
        let mut path = relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if directory {
            path.push('/');
        }
        snapshot.paths.push(path);
        // Match the existing file browser's bound on large workspaces.
        if snapshot.paths.len() >= 50_000 {
            break;
        }
    }
    // Lexicographic component ordering, folders before files at each depth.
    snapshot.paths.sort_by_cached_key(|path| {
        let pieces = path.trim_end_matches('/').split('/').collect::<Vec<_>>();
        pieces
            .iter()
            .enumerate()
            .map(|(i, part)| {
                (
                    i + 1 == pieces.len() && !path.ends_with('/'),
                    part.to_string(),
                )
            })
            .collect::<Vec<_>>()
    });
    snapshot.git = super::git_status::load(root).unwrap_or_else(|error| super::git_status::GitStatus {
        error: Some(format!("Git status: {error:#}")),
        ..Default::default()
    });
    snapshot.watch_directories.extend(snapshot.git.watch_directories.iter().cloned());
    Ok(snapshot)
}

pub(super) fn affects_tree(root: &Path, event: &Event) -> bool {
    if event.need_rescan() {
        return true;
    }
    if matches!(event.kind, EventKind::Access(_)) {
        return false;
    }
    if event.paths.is_empty() {
        return true;
    }
    let topology = matches!(
        event.kind,
        EventKind::Any
            | EventKind::Other
            | EventKind::Create(_)
            | EventKind::Remove(_)
            | EventKind::Modify(ModifyKind::Name(_) | ModifyKind::Any | ModifyKind::Other)
    );
    event.paths.iter().any(|path| {
        if topology && root.starts_with(path) {
            return true;
        }
        let Ok(relative) = path.strip_prefix(root) else {
            return false;
        };
        // Ignore files are hidden, but their contents determine visible entries.
        let ignore_file = relative
            .file_name()
            .is_some_and(|name| name == ".gitignore" || name == ".ignore");
        let visible = if ignore_file {
            relative.parent().unwrap_or(Path::new(""))
        } else {
            relative
        };
        (topology || ignore_file)
            && !visible
                .components()
                .any(|part| part.as_os_str().as_encoded_bytes().starts_with(b"."))
    })
}
