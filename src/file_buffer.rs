//! One editable document per canonical path. Views own positions; documents own history.
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context as _, Result, bail};
use gpui_kit::{Context, Entity, EntityId, EventEmitter};

pub(crate) enum BufferEvent {
    Saved(String),
}

impl EventEmitter<BufferEvent> for FileBuffer {}

#[derive(Clone)]
struct Snapshot {
    text: String,
    cursor: usize,
}

pub(crate) struct FileBuffer {
    pub(crate) path: PathBuf,
    pub(crate) text: String,
    pub(crate) version: i32,
    pub(crate) lsp: Option<Entity<crate::lsp::DocumentBinding>>,
    /// The exact bytes decoded from disk, including BOM and line endings.
    saved_text: String,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    insert_group: Option<EntityId>,
    pub(crate) saving: bool,
}

impl FileBuffer {
    pub(crate) fn load(path: &Path) -> Result<Self> {
        let path = path
            .canonicalize()
            .with_context(|| format!("Cannot open {}", path.display()))?;
        if !path.is_file() {
            bail!("Not a regular file: {}", path.display());
        }
        let saved_text = fs::read_to_string(&path)
            .with_context(|| format!("Cannot read {} as UTF-8 text", path.display()))?;
        if saved_text.contains('\0') {
            bail!("Cannot open binary file: {}", path.display());
        }
        let text = saved_text
            .strip_prefix('\u{feff}')
            .unwrap_or(&saved_text)
            .replace("\r\n", "\n");
        Ok(Self {
            path,
            text,
            version: 0,
            lsp: None,
            saved_text,
            undo: Vec::new(),
            redo: Vec::new(),
            insert_group: None,
            saving: false,
        })
    }

    pub(crate) fn serialized(&self) -> String {
        let mut text = if self.saved_text.contains("\r\n") {
            self.text.replace('\n', "\r\n")
        } else {
            self.text.clone()
        };
        if self.saved_text.starts_with('\u{feff}') {
            text.insert(0, '\u{feff}');
        }
        text
    }

    pub(crate) fn dirty(&self) -> bool {
        self.text
            != self
                .saved_text
                .strip_prefix('\u{feff}')
                .unwrap_or(&self.saved_text)
                .replace("\r\n", "\n")
    }

    pub(crate) fn saved_text(&self) -> String {
        self.saved_text.clone()
    }

    pub(crate) fn finish_save(&mut self, written: String, cx: &mut Context<Self>) {
        let normalized = written.strip_prefix('\u{feff}').unwrap_or(&written).replace("\r\n", "\n");
        self.saved_text = written;
        self.saving = false;
        cx.emit(BufferEvent::Saved(normalized));
        cx.notify();
    }

    pub(crate) fn end_insert(&mut self) {
        self.insert_group = None;
    }

    pub(crate) fn edit(
        &mut self,
        text: String,
        cursor: usize,
        source: EntityId,
        insert: bool,
        cx: &mut Context<Self>,
    ) {
        if self.text == text {
            return;
        }
        if !insert || self.insert_group != Some(source) {
            self.undo.push(Snapshot {
                text: self.text.clone(),
                cursor,
            });
            if self.undo.len() > 256 {
                self.undo.remove(0);
            }
        }
        self.insert_group = insert.then_some(source);
        self.redo.clear();
        self.text = text;
        self.version = self.version.checked_add(1).expect("document version exhausted");
        cx.notify();
    }

    pub(crate) fn undo(
        &mut self,
        cursor: usize,
        redo: bool,
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        self.insert_group = None;
        let (from, to) = if redo {
            (&mut self.redo, &mut self.undo)
        } else {
            (&mut self.undo, &mut self.redo)
        };
        let previous = from.pop()?;
        to.push(Snapshot {
            text: std::mem::replace(&mut self.text, previous.text),
            cursor,
        });
        self.version = self.version.checked_add(1).expect("document version exhausted");
        cx.notify();
        Some(previous.cursor)
    }
}

/// Check the last read version before replacing it. A failed write leaves the old file intact.
/// Canonical paths ensure saving through a symlink updates its target, not the link itself.
pub(crate) fn save(path: &Path, expected: &str, contents: &str) -> Result<()> {
    let actual = fs::read_to_string(path)
        .with_context(|| format!("Cannot read {} before saving", path.display()))?;
    if actual != expected {
        bail!("File changed on disk; save cancelled: {}", path.display());
    }
    let metadata = fs::metadata(path)?;
    if metadata.permissions().readonly() {
        bail!("File is read-only: {}", path.display());
    }
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let temporary = path.with_file_name(format!(
        ".rockstar-{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    // Only clean up a temporary file that this save actually created.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .with_context(|| format!("Cannot create temporary file for {}", path.display()))?;
    let result = (|| -> Result<()> {
        file.set_permissions(metadata.permissions())?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        // Recheck after the potentially slow write, before replacing the destination.
        if fs::read_to_string(path)? != expected {
            bail!("File changed on disk; save cancelled: {}", path.display());
        }
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.with_context(|| format!("Cannot save {}", path.display()))
}
