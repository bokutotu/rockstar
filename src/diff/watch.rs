//! One OS watch registration per directory, shared by all diff sessions in a window.
//! Filesystem discovery and backend registration never run on the UI thread.
use super::{DiffSession, git, model::RepositoryWatchTarget};
use anyhow::{Context as _, Result, ensure};
use gpui_kit::{
    Context, Entity, EntityId, Task, WeakEntity, Window,
    component::{WindowExt as _, notification::Notification},
};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher, event::ModifyKind};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

struct WatchedRepository {
    target: RepositoryWatchTarget,
    sessions: Vec<WeakEntity<DiffSession>>,
}

struct WatchCommand {
    targets: Vec<RepositoryWatchTarget>,
    invalidated: HashSet<PathBuf>,
    reset: bool,
}

enum Message {
    Event(notify::Result<Event>),
    Registered {
        status: Result<(), String>,
        changed: bool,
    },
}

pub(crate) struct DiffWatches {
    commands: mpsc::Sender<WatchCommand>,
    repositories: HashMap<PathBuf, WatchedRepository>,
    changed: HashSet<PathBuf>,
    invalidated: HashSet<PathBuf>,
    needs_sync: bool,
    debounce: Option<Task<()>>,
    overflow: Arc<AtomicBool>,
    degraded: bool,
    _retry: Task<()>,
}

impl DiffWatches {
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
                                this.report_error(error.to_string(), window, cx);
                                this.restart(cx);
                            }
                            Message::Registered { status, changed } => {
                                match status {
                                    Ok(()) => this.degraded = false,
                                    Err(error) => this.report_error(error, window, cx),
                                }
                                // A second discovery pass closes the traversal/registration
                                // gap: a new directory may have appeared before its parent
                                // was watched. Once registration is stable, later topology
                                // changes are covered by OS events.
                                if changed {
                                    this.sync_directories(false);
                                    this.invalidate_all(cx);
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
                        if this.overflow.swap(false, Ordering::Relaxed) {
                            this.restart(cx);
                        } else if this.degraded {
                            // No disk polling while the OS watcher is healthy.
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
            repositories: HashMap::new(),
            changed: HashSet::new(),
            invalidated: HashSet::new(),
            needs_sync: false,
            debounce: None,
            overflow,
            degraded: false,
            _retry: retry,
        }
    }

    pub(crate) fn attach(
        &mut self,
        target: RepositoryWatchTarget,
        session: &Entity<DiffSession>,
        cx: &mut Context<Self>,
    ) {
        let root = target.worktree.clone();
        let id = session.entity_id();
        let repository =
            self.repositories
                .entry(root.clone())
                .or_insert_with(|| WatchedRepository {
                    target: target.clone(),
                    sessions: Vec::new(),
                });
        if repository
            .sessions
            .iter()
            .any(|session| session.entity_id() == id)
        {
            return;
        }
        repository.target = target;
        repository.sessions.push(session.downgrade());
        let owner = cx.entity().downgrade();
        session.update(cx, |session, cx| {
            cx.on_release(move |_, cx| {
                let _ = owner.update(cx, |owner, _| owner.detach(&root, id));
            })
            .detach();
            // Also close the initial-load gap when this repository was already watched.
            session.invalidate(cx);
        });
        self.sync_directories(false);
    }

    fn detach(&mut self, root: &Path, id: EntityId) {
        if let Some(repository) = self.repositories.get_mut(root) {
            repository
                .sessions
                .retain(|session| session.entity_id() != id);
            if !repository.sessions.is_empty() {
                return;
            }
        }
        self.repositories.remove(root);
        self.changed.remove(root);
        self.sync_directories(false);
    }

    fn report_error(&mut self, error: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.repositories.is_empty() {
            self.degraded = false;
            return;
        }
        if !self.degraded {
            window.push_notification(Notification::error(format!("Diff watcher: {error}")), cx);
        }
        self.degraded = true;
    }

    fn handle_event(&mut self, event: Event, cx: &mut Context<Self>) {
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        if event.need_rescan() || event.paths.is_empty() || matches!(event.kind, EventKind::Other) {
            self.restart(cx);
            return;
        }
        let topology = matches!(
            event.kind,
            EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_))
        );
        let removed = matches!(
            event.kind,
            EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_))
        );
        let mut relevant = false;
        for (root, repository) in &self.repositories {
            for path in &event.paths {
                if !repository.target.affects(path) {
                    continue;
                }
                relevant = true;
                self.changed.insert(root.clone());
                self.needs_sync |= topology
                    || repository.target.metadata_changed(path)
                    || path
                        .file_name()
                        .is_some_and(|name| name == ".gitignore" || name == ".git");
                if removed {
                    self.invalidated.insert(path.clone());
                }
            }
        }
        if relevant && self.debounce.is_none() {
            // A fixed batching window also makes progress during a continuous stream
            // of saves, instead of postponing the refresh indefinitely.
            self.debounce = Some(cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.debounce = None;
                    if this.needs_sync {
                        this.sync_directories(false);
                    }
                    for root in std::mem::take(&mut this.changed) {
                        this.invalidate(&root, cx);
                    }
                });
            }));
        }
    }

    fn invalidate(&self, root: &Path, cx: &mut Context<Self>) {
        if let Some(repository) = self.repositories.get(root) {
            for session in &repository.sessions {
                if let Some(session) = session.upgrade() {
                    session.update(cx, |session, cx| session.invalidate(cx));
                }
            }
        }
    }

    fn invalidate_all(&self, cx: &mut Context<Self>) {
        for root in self.repositories.keys() {
            self.invalidate(root, cx);
        }
    }

    fn sync_directories(&mut self, reset: bool) {
        self.needs_sync = false;
        if self.repositories.is_empty() {
            self.degraded = false;
        }
        let _ = self.commands.send(WatchCommand {
            targets: self
                .repositories
                .values()
                .map(|repository| repository.target.clone())
                .collect(),
            invalidated: std::mem::take(&mut self.invalidated),
            reset,
        });
    }

    fn rescan(&mut self, cx: &mut Context<Self>) {
        self.sync_directories(false);
        self.invalidate_all(cx);
    }

    fn restart(&mut self, cx: &mut Context<Self>) {
        self.sync_directories(true);
        self.invalidate_all(cx);
    }
}

impl RepositoryWatchTarget {
    fn metadata_changed(&self, path: &Path) -> bool {
        [&self.git_dir, &self.common_dir]
            .into_iter()
            .any(|directory| {
                let Ok(relative) = path.strip_prefix(directory) else {
                    return false;
                };
                if relative
                    .file_name()
                    .is_some_and(|name| name.as_encoded_bytes().ends_with(b".lock"))
                {
                    return false;
                }
                relative.as_os_str().is_empty()
                    || relative.starts_with("refs")
                    || relative.starts_with("reftable")
                    || [
                        "HEAD",
                        "index",
                        "packed-refs",
                        "config",
                        "config.worktree",
                        "commondir",
                        "gitdir",
                        "shallow",
                        "info",
                        "info/exclude",
                    ]
                    .iter()
                    .any(|name| relative == Path::new(name))
            })
    }

    fn affects(&self, path: &Path) -> bool {
        if [&self.worktree, &self.git_dir, &self.common_dir]
            .into_iter()
            .any(|directory| directory.starts_with(path))
            || self.metadata_changed(path)
        {
            return true;
        }
        // Do not treat objects, logs, locks, or other worktrees' private metadata
        // as worktree edits even when the Git directory is inside the worktree.
        if path.starts_with(&self.git_dir) || path.starts_with(&self.common_dir) {
            return false;
        }
        let Ok(relative) = path.strip_prefix(&self.worktree) else {
            return false;
        };
        relative == Path::new(".git")
            || !relative.components().any(|part| part.as_os_str() == ".git")
    }
}

fn collect_directories(
    target: &RepositoryWatchTarget,
    directories: &mut HashSet<PathBuf>,
) -> Result<()> {
    // When a root disappears, creation events in its surviving ancestor let us
    // restore the watches. The degraded-mode rescan is a second recovery path.
    for root in [&target.worktree, &target.git_dir, &target.common_dir] {
        if let Some(parent) = root.ancestors().find(|path| path.is_dir()) {
            directories.insert(parent.to_path_buf());
        }
    }
    for root in [&target.git_dir, &target.common_dir] {
        ensure!(
            root.is_dir(),
            "Git directory unavailable: {}",
            root.display()
        );
        for name in ["refs", "info", "reftable"] {
            collect_metadata_directories(&root.join(name), directories)?;
        }
    }
    ensure!(
        fs::symlink_metadata(&target.worktree)?.is_dir(),
        "Working tree unavailable: {}",
        target.worktree.display()
    );
    let git_dir = target.git_dir.clone();
    let common_dir = target.common_dir.clone();
    for entry in ignore::WalkBuilder::new(&target.worktree)
        .hidden(false)
        .ignore(false)
        .follow_links(false)
        .filter_entry(move |entry| {
            entry.file_type().is_some_and(|kind| kind.is_dir())
                && entry.file_name() != ".git"
                && !entry.path().starts_with(&git_dir)
                && !entry.path().starts_with(&common_dir)
        })
        .build()
    {
        let entry = entry?;
        directories.insert(entry.into_path());
    }
    // Ignored directories may still contain tracked files. Watch their parents
    // too, without walking ignored build outputs or following replaced symlinks.
    let mut tracked_directories = HashSet::new();
    for file in git::tracked_paths(&target.worktree)? {
        let absolute = target.worktree.join(file);
        for parent in absolute.parent().into_iter().flat_map(Path::ancestors) {
            if parent == target.worktree {
                break;
            }
            tracked_directories.insert(parent.to_path_buf());
        }
    }
    let mut tracked_directories = tracked_directories.into_iter().collect::<Vec<_>>();
    tracked_directories.sort(); // parents precede children
    for directory in tracked_directories {
        if directory
            .parent()
            .is_some_and(|parent| directories.contains(parent))
            && fs::symlink_metadata(&directory).is_ok_and(|metadata| metadata.is_dir())
        {
            directories.insert(directory);
        }
    }
    Ok(())
}

fn collect_metadata_directories(root: &Path, directories: &mut HashSet<PathBuf>) -> Result<()> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        directories.insert(directory.clone());
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    Ok(())
}

fn watch_worker(
    requests: mpsc::Receiver<WatchCommand>,
    sender: async_channel::Sender<Message>,
    overflow: Arc<AtomicBool>,
) {
    let mut watcher: Option<RecommendedWatcher> = None;
    let mut registered = HashSet::<PathBuf>::new();
    while let Ok(mut command) = requests.recv() {
        // If discovery was slow, use the latest set of owners without losing
        // invalidations for directories that were removed and recreated.
        while let Ok(next) = requests.try_recv() {
            command.targets = next.targets;
            command.invalidated.extend(next.invalidated);
            command.reset |= next.reset;
        }
        let mut changed = command.reset;
        if command.reset {
            watcher = None;
            registered.clear();
        }
        let mut desired = HashSet::new();
        let mut status = Ok(());
        for target in &command.targets {
            if let Err(error) = collect_directories(target, &mut desired)
                .with_context(|| format!("Cannot watch {}", target.worktree.display()))
            {
                status = Err(format!("{error:#}"));
            }
        }
        if desired.is_empty() {
            watcher = None;
            registered.clear();
        } else {
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
                    Ok(created) => {
                        watcher = Some(created);
                        changed = true;
                    }
                    Err(error) => status = Err(error.to_string()),
                }
            }
            if let Some(watcher) = &mut watcher {
                let stale = registered
                    .iter()
                    .filter(|path| {
                        !desired.contains(*path)
                            || command
                                .invalidated
                                .iter()
                                .any(|removed| path.starts_with(removed))
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                for path in stale {
                    let _ = watcher.unwatch(&path);
                    registered.remove(&path);
                    changed = true;
                }
                for directory in desired.difference(&registered).cloned().collect::<Vec<_>>() {
                    match watcher.watch(&directory, RecursiveMode::NonRecursive) {
                        Ok(()) => {
                            registered.insert(directory);
                            changed = true;
                        }
                        Err(error) => {
                            status = Err(format!("{}: {error}", directory.display()));
                        }
                    }
                }
            }
        }
        if sender
            .send_blocking(Message::Registered { status, changed })
            .is_err()
        {
            break;
        }
    }
}
