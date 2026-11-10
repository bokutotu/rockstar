//! Validation and filesystem operations for new file-tree entries.
use anyhow::{Context as _, Result, ensure};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum EntryKind {
    File,
    Directory,
}

pub(super) struct CreateEntryRequest {
    pub kind: EntryKind,
    pub relative_path: PathBuf,
}

pub(super) fn initial_creation_path(selected: Option<&str>) -> PathBuf {
    selected
        .and_then(|path| path.rfind('/').map(|index| &path[..=index]))
        .map(PathBuf::from)
        .unwrap_or_default()
}

pub(super) fn resolve_creation_path(root: &Path, request: &CreateEntryRequest) -> Result<PathBuf> {
    let relative = &request.relative_path;
    ensure!(!relative.as_os_str().is_empty(), "Enter a relative path");
    ensure!(
        relative
            .components()
            .all(|part| matches!(part, Component::Normal(_) | Component::CurDir)),
        "Use a relative path without '..' components"
    );
    let bytes = relative.as_os_str().as_encoded_bytes();
    let last_part = bytes
        .rsplit(|byte| *byte == b'/')
        .find(|part| !part.is_empty());
    ensure!(
        last_part != Some(b".".as_slice())
            && (request.kind != EntryKind::File || !bytes.ends_with(b"/")),
        "Enter a name for the new entry"
    );
    let name = relative
        .file_name()
        .context("Enter a name for the new entry")?;
    let root = root
        .canonicalize()
        .with_context(|| format!("Cannot access workspace {}", root.display()))?;
    let parent = root.join(relative.parent().unwrap_or(Path::new("")));
    // Resolve the existing parent, not the new leaf. This also rejects missing
    // intermediate directories and links that point outside the workspace.
    let parent = parent
        .canonicalize()
        .with_context(|| format!("Cannot access parent directory {}", parent.display()))?;
    ensure!(
        parent.starts_with(&root),
        "The parent directory is outside the workspace"
    );
    ensure!(parent.is_dir(), "Not a directory: {}", parent.display());
    Ok(parent.join(name))
}

pub(super) fn create_entry(root: &Path, request: &CreateEntryRequest) -> Result<PathBuf> {
    let path = resolve_creation_path(root, request)?;
    match request.kind {
        EntryKind::File => fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map(drop),
        EntryKind::Directory => fs::create_dir(&path),
    }
    .with_context(|| format!("Cannot create {}", path.display()))?;
    Ok(path)
}
