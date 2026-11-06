//! OS notifications are coalesced per path and fan out to independent tab buffers.
//! Watch registration also runs off the UI thread: some backends synchronously
//! wait for their event thread, which may itself be waking the UI executor.
use crate::file_buffer::{self, FileBuffer};
use gpui_kit::{
    AppContext as _, Context, Entity, EntityId, Task, WeakEntity, Window,
    component::{WindowExt as _, notification::Notification},
};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

enum Message {
    Event(notify::Result<Event>),
    BackendStatus(Result<(), String>),
}
enum WatchCommand {
    Directories(HashSet<PathBuf>),
    Reset,
}
struct WatchedFile {
    buffers: Vec<WeakEntity<FileBuffer>>,
    reading: Option<Task<()>>,
}

pub(crate) struct FileWatches {
    commands: mpsc::Sender<WatchCommand>,
    directories: HashSet<PathBuf>,
    files: HashMap<PathBuf, WatchedFile>,
    overflow: Arc<AtomicBool>,
    degraded: bool,
    _retry: Task<()>,
}

impl FileWatches {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (sender, receiver) = async_channel::bounded(256);
        let (commands, requests) = mpsc::channel();
        let overflow = Arc::new(AtomicBool::new(false));
        let worker_overflow = overflow.clone();
        std::thread::spawn(move || watch_worker(requests, sender, worker_overflow));
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(message) = receiver.recv().await {
                if this
                    .update_in(cx, |this, window, cx| {
                        match message {
                            Message::Event(Ok(event)) => this.handle_event(event, cx),
                            Message::Event(Err(error)) => {
                                window.push_notification(
                                    Notification::error(format!("File watcher: {error}")),
                                    cx,
                                );
                                this.restart(cx);
                            }
                            Message::BackendStatus(status) => {
                                if let Err(error) = &status
                                    && !this.degraded
                                {
                                    window.push_notification(
                                        Notification::error(format!("File watcher: {error}")),
                                        cx,
                                    );
                                }
                                this.degraded = status.is_err();
                                if status.is_ok() {
                                    this.refresh_all(cx);
                                }
                            }
                        }
                        if this.overflow.swap(false, Ordering::Relaxed) {
                            this.restart(cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.rescan(cx);
            }
        })
        .detach();
        let retry = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this
                    .update(cx, |this, cx| {
                        // No filesystem polling while the OS watcher is healthy.
                        if this.degraded {
                            this.rescan(cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            commands,
            directories: HashSet::new(),
            files: HashMap::new(),
            overflow,
            degraded: false,
            _retry: retry,
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

    fn sync_directories(&mut self) {
        self.directories = self
            .files
            .keys()
            .filter_map(|path| {
                // A removed parent is recovered through creation events in its nearest
                // surviving ancestor; no recursive project-wide watch is required.
                path.parent()?
                    .ancestors()
                    .find(|parent| parent.is_dir())
                    .map(Path::to_path_buf)
            })
            .collect();
        if self.directories.is_empty() {
            self.degraded = false;
        }
        let _ = self
            .commands
            .send(WatchCommand::Directories(self.directories.clone()));
    }

    fn handle_event(&mut self, event: Event, cx: &mut Context<Self>) {
        if event.need_rescan() || event.paths.is_empty() || matches!(event.kind, EventKind::Other) {
            self.restart(cx);
            return;
        }
        if matches!(
            event.kind,
            EventKind::Remove(_) | EventKind::Modify(notify::event::ModifyKind::Name(_))
        ) && event
            .paths
            .iter()
            .any(|path| self.directories.contains(path))
        {
            let _ = self.commands.send(WatchCommand::Reset);
        }
        let paths = self
            .files
            .keys()
            .filter(|path| event.paths.iter().any(|changed| path.starts_with(changed)))
            .cloned()
            .collect::<Vec<_>>();
        if !paths.is_empty() {
            self.sync_directories();
            for path in paths {
                self.refresh(&path, cx);
            }
        }
    }

    fn restart(&mut self, cx: &mut Context<Self>) {
        let _ = self.commands.send(WatchCommand::Reset);
        self.rescan(cx);
    }

    fn rescan(&mut self, cx: &mut Context<Self>) {
        self.sync_directories();
        self.refresh_all(cx);
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

fn watch_worker(
    requests: mpsc::Receiver<WatchCommand>,
    sender: async_channel::Sender<Message>,
    overflow: Arc<AtomicBool>,
) {
    let mut watcher: Option<RecommendedWatcher> = None;
    let mut registered = HashSet::new();
    while let Ok(command) = requests.recv() {
        let desired = match command {
            WatchCommand::Reset => {
                watcher = None;
                registered.clear();
                continue;
            }
            WatchCommand::Directories(desired) => desired,
        };
        if desired.is_empty() {
            watcher = None;
            registered.clear();
            continue;
        }
        if watcher.is_some() && desired == registered {
            continue;
        }
        if watcher.is_none() {
            let events = sender.clone();
            let overflow = overflow.clone();
            match notify::recommended_watcher(move |event: notify::Result<Event>| {
                if event
                    .as_ref()
                    .is_ok_and(|event| matches!(event.kind, EventKind::Access(_)))
                {
                    return;
                }
                if events.try_send(Message::Event(event)).is_err() {
                    overflow.store(true, Ordering::Relaxed);
                }
            }) {
                Ok(created) => watcher = Some(created),
                Err(error) => {
                    let _ = sender.send_blocking(Message::BackendStatus(Err(error.to_string())));
                    continue;
                }
            }
        }
        let watcher = watcher.as_mut().unwrap();
        for path in registered.difference(&desired) {
            let _ = watcher.unwatch(path);
        }
        registered.retain(|path| desired.contains(path));
        let mut status = Ok(());
        for path in desired.difference(&registered).cloned().collect::<Vec<_>>() {
            match watcher.watch(&path, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    registered.insert(path);
                }
                Err(error) => status = Err(format!("{}: {error}", path.display())),
            }
        }
        // A registration acknowledgement also rechecks disk, closing the gap
        // between the initial file load and asynchronous watch registration.
        if sender
            .send_blocking(Message::BackendStatus(status))
            .is_err()
        {
            break;
        }
    }
}
