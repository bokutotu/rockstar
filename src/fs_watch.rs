//! Shared OS watch registration and recovery, independent of UI and document state.
//! All filesystem access and backend calls run on the worker, never the UI thread.
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher, event::ModifyKind};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

const EVENT_CAPACITY: usize = 256;
const RETRY_INTERVAL: Duration = Duration::from_secs(1);

pub(crate) struct WatchTargets {
    pub directories: HashSet<PathBuf>,
}

pub(crate) enum WatchUpdate {
    Changed(Event),
    Rescan,
    Error(String),
}

enum Command {
    Targets(WatchTargets),
    Recheck,
    Event(notify::Result<Event>),
    Stop,
}

pub(crate) struct FsWatch {
    commands: mpsc::Sender<Command>,
}

impl FsWatch {
    pub(crate) fn new() -> (Self, async_channel::Receiver<WatchUpdate>) {
        let (commands, requests) = mpsc::channel();
        let (updates, receiver) = async_channel::bounded(EVENT_CAPACITY);
        let worker_commands = commands.clone();
        std::thread::spawn(move || run_worker(requests, worker_commands, updates));
        (Self { commands }, receiver)
    }

    pub(crate) fn set_targets(&self, targets: WatchTargets) {
        let _ = self.commands.send(Command::Targets(targets));
    }

    pub(crate) fn recheck(&self) {
        let _ = self.commands.send(Command::Recheck);
    }
}

impl Drop for FsWatch {
    fn drop(&mut self) {
        // Explicit shutdown: the backend callback also holds a command sender.
        // Never join here, since backend shutdown can block the UI thread.
        let _ = self.commands.send(Command::Stop);
    }
}

struct Worker {
    commands: mpsc::Sender<Command>,
    queued_events: Arc<AtomicUsize>,
    overflow: Arc<AtomicBool>,
    updates: async_channel::Sender<WatchUpdate>,
    targets: HashSet<PathBuf>,
    registered: HashSet<PathBuf>,
    watcher: Option<RecommendedWatcher>,
    degraded: bool,
    rescan_pending: bool,
    error_pending: Option<String>,
}

fn run_worker(
    requests: mpsc::Receiver<Command>,
    commands: mpsc::Sender<Command>,
    updates: async_channel::Sender<WatchUpdate>,
) {
    let mut worker = Worker {
        commands,
        queued_events: Arc::new(AtomicUsize::new(0)),
        overflow: Arc::new(AtomicBool::new(false)),
        updates,
        targets: HashSet::new(),
        registered: HashSet::new(),
        watcher: None,
        degraded: false,
        rescan_pending: false,
        error_pending: None,
    };
    let mut retry_at = Instant::now() + RETRY_INTERVAL;
    loop {
        let first = match requests.recv_timeout(retry_at.saturating_duration_since(Instant::now()))
        {
            Ok(command) => Some(command),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if worker.updates.is_closed() {
            break;
        }
        let mut sync = false;
        let mut reset = false;
        // Coalesce target changes and resets, but keep progressing during event storms.
        for command in first
            .into_iter()
            .chain(requests.try_iter().take(EVENT_CAPACITY - 1))
        {
            match command {
                Command::Stop => return,
                Command::Targets(targets) => {
                    if worker.targets != targets.directories {
                        worker.targets = targets.directories;
                        sync = true;
                    }
                }
                Command::Recheck => {
                    // Rebuild even if paths are unchanged: a missed replacement can
                    // leave the backend watching the old inode at the same path.
                    sync = true;
                    reset = true;
                    worker.rescan_pending = true;
                }
                Command::Event(event) => {
                    worker.queued_events.fetch_sub(1, Ordering::Relaxed);
                    match event {
                        Ok(event) => {
                            let (needs_sync, invalidated) = worker.handle_event(event);
                            sync |= needs_sync;
                            reset |= invalidated;
                        }
                        Err(error) => {
                            worker.report_error(error.to_string());
                            sync = true;
                            reset = true;
                        }
                    }
                }
            }
        }
        if worker.overflow.swap(false, Ordering::Relaxed) {
            sync = true;
            reset = true;
            worker.rescan_pending = true;
        }
        if Instant::now() >= retry_at {
            // The healthy timer only checks queues; it never polls the filesystem.
            if worker.degraded {
                sync = true;
                reset = true;
                worker.rescan_pending = true;
            }
            retry_at = Instant::now() + RETRY_INTERVAL;
        }
        if sync {
            worker.sync(reset);
            if worker.degraded {
                retry_at = Instant::now() + RETRY_INTERVAL;
            }
        }
        worker.flush_updates();
    }
}

impl Worker {
    fn handle_event(&mut self, event: Event) -> (bool, bool) {
        if event.need_rescan()
            || event.paths.is_empty()
            || matches!(event.kind, EventKind::Any | EventKind::Other)
        {
            self.rescan_pending = true;
            return (true, true);
        }
        let removed = matches!(
            event.kind,
            EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_))
        );
        let topology = removed || matches!(event.kind, EventKind::Create(_));
        let invalidated = removed
            && event.paths.iter().any(|path| {
                self.registered
                    .iter()
                    .any(|directory| directory.starts_with(path))
            });
        // A watch on a surviving ancestor must move back down as missing
        // directories are recreated, without waiting for a consumer to resubscribe.
        let sync = invalidated
            || (topology
                && event.paths.iter().any(|path| {
                    self.targets
                        .iter()
                        .any(|directory| directory.starts_with(path))
                }));
        if !self.rescan_pending
            && let Err(async_channel::TrySendError::Full(_)) =
                self.updates.try_send(WatchUpdate::Changed(event))
        {
            // Backend events are still processed; only the consumer missed
            // changes. Retain a rescan until its queue has room again.
            self.rescan_pending = true;
        }
        (sync, invalidated)
    }

    fn sync(&mut self, reset: bool) {
        if self.targets.is_empty() {
            self.watcher = None;
            self.registered.clear();
            self.degraded = false;
            self.rescan_pending = false;
            self.error_pending = None;
            return;
        }
        if reset {
            self.watcher = None;
            self.registered.clear();
            self.rescan_pending = true;
        }
        let mut desired = HashSet::new();
        let mut error = None;
        for path in &self.targets {
            match existing_directory(path) {
                Ok(directory) => {
                    desired.insert(directory);
                }
                Err(message) => error = Some(message),
            }
        }
        if self.watcher.is_none() && !desired.is_empty() {
            let commands = self.commands.clone();
            let queued = self.queued_events.clone();
            let overflow = self.overflow.clone();
            match notify::recommended_watcher(move |event: notify::Result<Event>| {
                if event.as_ref().is_ok_and(|event| {
                    !event.need_rescan() && matches!(event.kind, EventKind::Access(_))
                }) {
                    return;
                }
                // Bound the event part of the queue without blocking the backend.
                // Control commands, especially shutdown, must always be accepted.
                if queued
                    .try_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                        (count < EVENT_CAPACITY).then_some(count + 1)
                    })
                    .is_err()
                {
                    overflow.store(true, Ordering::Relaxed);
                    return;
                }
                if commands.send(Command::Event(event)).is_err() {
                    queued.fetch_sub(1, Ordering::Relaxed);
                }
            }) {
                Ok(watcher) => self.watcher = Some(watcher),
                Err(failure) => error = Some(failure.to_string()),
            }
        }
        if let Some(watcher) = &mut self.watcher {
            for path in self.registered.difference(&desired) {
                if let Err(failure) = watcher.unwatch(path)
                    && !matches!(failure.kind, notify::ErrorKind::WatchNotFound)
                {
                    error = Some(format!("{}: {failure}", path.display()));
                }
                self.rescan_pending = true;
            }
            self.registered.retain(|path| desired.contains(path));
            for path in desired
                .difference(&self.registered)
                .cloned()
                .collect::<Vec<_>>()
            {
                match watcher.watch(&path, RecursiveMode::NonRecursive) {
                    Ok(()) => {
                        self.registered.insert(path);
                        // Close the read/traversal-to-registration gap.
                        self.rescan_pending = true;
                    }
                    Err(failure) => error = Some(format!("{}: {failure}", path.display())),
                }
            }
        }
        if let Some(error) = error {
            self.report_error(error);
        } else {
            self.degraded = false;
        }
    }

    fn report_error(&mut self, error: String) {
        if !self.degraded {
            self.error_pending = Some(error);
        }
        self.degraded = true;
        self.rescan_pending = true;
    }

    fn flush_updates(&mut self) {
        // Never block on the UI receiver, including when it is no longer polled.
        if let Some(error) = self.error_pending.take()
            && let Err(async_channel::TrySendError::Full(WatchUpdate::Error(error))) =
                self.updates.try_send(WatchUpdate::Error(error))
        {
            self.error_pending = Some(error);
        }
        if self.rescan_pending && self.updates.try_send(WatchUpdate::Rescan).is_ok() {
            self.rescan_pending = false;
        }
    }
}

fn existing_directory(path: &Path) -> Result<PathBuf, String> {
    for ancestor in path.ancestors() {
        match fs::metadata(ancestor) {
            Ok(metadata) if metadata.is_dir() => return Ok(ancestor.to_path_buf()),
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) => {}
            Err(error) => return Err(format!("{}: {error}", ancestor.display())),
        }
    }
    Err(format!(
        "No existing directory to watch for {}",
        path.display()
    ))
}
