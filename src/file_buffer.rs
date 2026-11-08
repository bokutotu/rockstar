//! One editable document per canonical path within a tab. Other tabs sync only through disk.
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

mod history;
use history::UndoTree;
pub(crate) use history::{BufferPositions, map_position};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DiskState {
    Current,
    Changed,
    Unavailable(String),
}

pub(crate) struct FileBuffer {
    pub(crate) path: PathBuf,
    pub(crate) text: String,
    pub(crate) version: i32,
    pub(crate) lsp: Option<Entity<crate::lsp::DocumentBinding>>,
    /// The exact bytes decoded from disk, including BOM and line endings.
    saved_text: String,
    undo: UndoTree,
    insert_group: Option<EntityId>,
    replay_group: Option<u64>,
    pub(crate) positions: BufferPositions,
    pub(crate) saving: bool,
    pub(crate) disk_state: DiskState,
    /// Invalidates disk reads across saves/reloads even if the editor text did not change.
    pub(crate) disk_epoch: u64,
}

impl FileBuffer {
    pub(crate) fn load(path: &Path) -> Result<Self> {
        let path = path
            .canonicalize()
            .with_context(|| format!("Cannot open {}", path.display()))?;
        let saved_text = read_disk(&path)?;
        let text = saved_text
            .strip_prefix('\u{feff}')
            .unwrap_or(&saved_text)
            .replace("\r\n", "\n");
        Ok(Self {
            undo: UndoTree::new(&text),
            path,
            text,
            version: 0,
            lsp: None,
            saved_text,
            insert_group: None,
            replay_group: None,
            positions: BufferPositions::default(),
            saving: false,
            disk_state: DiskState::Current,
            disk_epoch: 0,
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
        let saved = self
            .saved_text
            .strip_prefix('\u{feff}')
            .unwrap_or(&self.saved_text);
        if saved.contains("\r\n") {
            self.text != saved.replace("\r\n", "\n")
        } else {
            // LF files need no normalized copy on every title/status paint.
            self.text != saved
        }
    }

    pub(crate) fn saved_text(&self) -> String {
        self.saved_text.clone()
    }

    pub(crate) fn finish_save(&mut self, written: String, cx: &mut Context<Self>) {
        let normalized = written
            .strip_prefix('\u{feff}')
            .unwrap_or(&written)
            .replace("\r\n", "\n");
        self.saved_text = written;
        self.saving = false;
        self.disk_epoch += 1;
        self.disk_state = DiskState::Current;
        cx.emit(BufferEvent::Saved(normalized));
        cx.notify();
    }

    pub(crate) fn apply_disk_contents(&mut self, contents: Result<String>, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let state = match contents {
            Ok(contents) if contents == self.saved_text => DiskState::Current,
            Ok(contents) if !self.dirty() => {
                self.replace_from_disk(contents, cx);
                return;
            }
            Ok(_) => DiskState::Changed,
            Err(error) => DiskState::Unavailable(format!("{error:#}")),
        };
        // A newer external snapshot must also invalidate a pending explicit
        // reload when local edits prevented us from replacing the text.
        if state != DiskState::Current {
            self.disk_epoch += 1;
        }
        if self.disk_state != state {
            self.disk_state = state;
            cx.notify();
        }
    }

    pub(crate) fn replace_from_disk(&mut self, contents: String, cx: &mut Context<Self>) {
        let before = std::mem::take(&mut self.text);
        self.text = contents
            .strip_prefix('\u{feff}')
            .unwrap_or(&contents)
            .replace("\r\n", "\n");
        self.saved_text = contents;
        self.undo = UndoTree::new(&self.text);
        self.positions.remap(&before, &self.text);
        self.insert_group = None;
        self.replay_group = None;
        self.version = self
            .version
            .checked_add(1)
            .expect("document version exhausted");
        self.disk_epoch += 1;
        self.disk_state = DiskState::Current;
        cx.notify();
    }

    pub(crate) fn finish_insert(&mut self, source: EntityId, cursor: usize) {
        if self.insert_group == Some(source) {
            self.undo.finish_cursor(cursor);
        }
        self.insert_group = None;
    }
    pub(crate) fn end_insert(&mut self) {
        self.insert_group = None;
    }

    pub(crate) fn edit(
        &mut self,
        text: String,
        cursor: usize,
        after: usize,
        source: EntityId,
        insert: bool,
        replay: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        if self.text == text {
            return;
        }
        let join = replay.is_some_and(|id| self.replay_group == Some(id))
            || (replay.is_none() && insert && self.insert_group == Some(source));
        self.undo.record(&text, cursor, after, join);
        let removed = self
            .positions
            .changed(&self.text, &text, cursor, after, join);
        self.undo.remember_removed_marks(removed);
        self.insert_group = insert.then_some(source);
        self.replay_group = replay;
        self.text = text;
        self.version = self
            .version
            .checked_add(1)
            .expect("document version exhausted");
        cx.notify();
    }

    pub(crate) fn undo(
        &mut self,
        _cursor: usize,
        redo: bool,
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        let state = self.undo.step(redo)?;
        Some(self.restore(state, cx))
    }
    pub(crate) fn undo_time(
        &mut self,
        newer: bool,
        count: usize,
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        let state = self.undo.time(newer, count)?;
        Some(self.restore(state, cx))
    }
    fn restore(&mut self, (text, cursor): (String, usize), cx: &mut Context<Self>) -> usize {
        self.insert_group = None;
        self.replay_group = None;
        self.positions.remap(&self.text, &text);
        for (name, at) in self.undo.take_restored_marks() {
            self.positions.marks.entry(name).or_insert(at);
        }
        self.text = text;
        self.version = self
            .version
            .checked_add(1)
            .expect("document version exhausted");
        cx.notify();
        cursor
    }
}

pub(crate) fn read_disk(path: &Path) -> Result<String> {
    if !path.is_file() {
        bail!("Not a regular file: {}", path.display());
    }
    let contents = fs::read_to_string(path)
        .with_context(|| format!("Cannot read {} as UTF-8 text", path.display()))?;
    if contents.contains('\0') {
        bail!("Cannot open binary file: {}", path.display());
    }
    Ok(contents)
}

/// Check the last read version before replacing it. A failed write leaves the old file intact.
/// Canonical paths ensure saving through a symlink updates its target, not the link itself.
pub(crate) fn save(path: &Path, expected: &str, contents: &str) -> Result<()> {
    // All app writes are short, background operations. Serializing them also closes
    // the compare/rename race between independent tabs writing the same path.
    static WRITES: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _write = WRITES.lock().unwrap_or_else(|poison| poison.into_inner());
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
