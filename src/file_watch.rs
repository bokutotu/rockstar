//! Shared filesystem notifications fan out to independent tab buffers.
//! Document reads are coalesced per path; OS watch recovery lives in `fs_watch`.
use crate::{
    file_buffer::{self, FileBuffer},
    fs_watch::{FsWatch, WatchTargets, WatchUpdate},
};
use gpui_kit::{
    AppContext as _, Context, Entity, EntityId, Task, WeakEntity, Window,
    component::{WindowExt as _, notification::Notification},
};
use notify::Event;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::Duration,
};

struct WatchedFile {
    buffers: Vec<WeakEntity<FileBuffer>>,
    reading: Option<Task<()>>,
}

pub(crate) struct FileWatches {
    watch: FsWatch,
    files: HashMap<PathBuf, WatchedFile>,
}

impl FileWatches {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (watch, receiver) = FsWatch::new();
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(update) = receiver.recv().await {
                if this
                    .update_in(cx, |this, window, cx| {
                        this.handle_watch_update(update, window, cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        cx.observe_window_activation(window, |this, window, _| {
            if window.is_window_active() {
                this.watch.recheck();
            }
        })
        .detach();
        Self {
            watch,
            files: HashMap::new(),
        }
    }

    fn handle_watch_update(
        &mut self,
        update: WatchUpdate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match update {
            WatchUpdate::Changed(event) => self.handle_event(event, cx),
            WatchUpdate::Rescan => self.refresh_all(cx),
            WatchUpdate::Error(error) => {
                window.push_notification(Notification::error(format!("File watcher: {error}")), cx)
            }
        }
    }

    pub(crate) fn attach(&mut self, buffer: &Entity<FileBuffer>, cx: &mut Context<Self>) {
        let path = buffer.read(cx).path.clone();
        let id = buffer.entity_id();
        let file = self
            .files
            .entry(path.clone())
            .or_insert_with(|| WatchedFile {
                buffers: Vec::new(),
                reading: None,
            });
        if file.buffers.iter().any(|buffer| buffer.entity_id() == id) {
            return;
        }
        file.buffers.push(buffer.downgrade());
        let owner = cx.entity().downgrade();
        let released_path = path.clone();
        buffer.update(cx, |_, cx| {
            cx.on_release(move |_, cx| {
                let _ = owner.update(cx, |owner, cx| owner.detach(&released_path, id, cx));
            })
            .detach();
        });
        let watched_path = path.clone();
        let mut saving = buffer.read(cx).saving;
        let mut dirty = buffer.read(cx).dirty();
        cx.observe(buffer, move |this, buffer, cx| {
            let state = buffer.read(cx);
            let now = state.saving;
            let now_dirty = state.dirty();
            let needs_reload =
                dirty && !now_dirty && state.disk_state != file_buffer::DiskState::Current;
            if (saving && !now) || needs_reload {
                this.refresh(&watched_path, cx);
            }
            saving = now;
            dirty = now_dirty;
        })
        .detach();
        self.sync_directories();
        self.refresh(&path, cx);
    }

    fn detach(&mut self, path: &Path, id: EntityId, _: &mut Context<Self>) {
        if let Some(file) = self.files.get_mut(path) {
            file.buffers.retain(|buffer| buffer.entity_id() != id);
            if file.buffers.is_empty() {
                self.files.remove(path);
            }
        }
        self.sync_directories();
    }

    fn sync_directories(&self) {
        self.watch.set_targets(WatchTargets {
            directories: self
                .files
                .keys()
                .filter_map(|path| path.parent().map(Path::to_path_buf))
                .collect(),
        });
    }

    fn handle_event(&mut self, event: Event, cx: &mut Context<Self>) {
        let paths = self
            .files
            .keys()
            .filter(|path| event.paths.iter().any(|changed| path.starts_with(changed)))
            .cloned()
            .collect::<Vec<_>>();
        for path in paths {
            self.refresh(&path, cx);
        }
    }

    fn refresh_all(&mut self, cx: &mut Context<Self>) {
        for path in self.files.keys().cloned().collect::<Vec<_>>() {
            self.refresh(&path, cx);
        }
    }

    fn refresh(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(file) = self.files.get_mut(path) else {
            return;
        };
        let path = path.to_path_buf();
        // Replacing the task coalesces notifications and invalidates older reads.
        file.reading = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(40))
                .await;
            let Ok(buffers) = this.read_with(cx, |this, cx| {
                this.files
                    .get(&path)
                    .map(|file| {
                        file.buffers
                            .iter()
                            .filter_map(|weak| {
                                let buffer = weak.upgrade()?;
                                let state = buffer.read(cx);
                                (!state.saving)
                                    .then(|| (weak.clone(), state.version, state.disk_epoch))
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            }) else {
                return;
            };
            let read_path = path.clone();
            let contents = cx
                .background_spawn(async move {
                    file_buffer::read_disk(&read_path).map_err(|error| format!("{error:#}"))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let mut again = false;
                for (weak, version, epoch) in buffers {
                    let Some(buffer) = weak.upgrade() else {
                        continue;
                    };
                    let state = buffer.read(cx);
                    if state.saving {
                        continue;
                    }
                    if (state.version, state.disk_epoch) != (version, epoch) {
                        again = true;
                        continue;
                    }
                    buffer.update(cx, |buffer, cx| {
                        buffer.apply_disk_contents(contents.clone().map_err(anyhow::Error::msg), cx)
                    });
                }
                if again {
                    this.refresh(&path, cx);
                }
            });
        }));
    }
}
