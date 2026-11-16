//! Dedicated `haskell-language-server-wrapper` integration.
//!
//! HLS (`ghcide` + `hie-bios`) has no server-side OS file watcher fallback:
//! it caches `GetFileExists` in Shake and reloads `.cabal` / `package.yaml` /
//! `hie.yaml` cradles exclusively from `workspace/didChangeWatchedFiles`.
use super::{
    adapter::{ServerAdapter, lookup_section},
    language::{self, ServerKind},
    transport::{CommandSpec, queue_message},
};
use crate::fs_watch::{FsWatch, WatchTargets, WatchUpdate};
use anyhow::Result;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    env,
    ffi::OsStr,
    fs,
    os::unix::{ffi::OsStrExt as _, fs::MetadataExt as _},
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::Duration,
};

const MAX_WATCH_DIRS: usize = 20_000;
const MAX_WATCHED_FILES: usize = 50_000;
const WATCH_DEBOUNCE: Duration = Duration::from_millis(50);
const CHANGE_BATCH_SIZE: usize = 256;

const FILE_CREATED: u8 = 1;
const FILE_CHANGED: u8 = 2;
const FILE_DELETED: u8 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileStamp {
    dev: u64,
    ino: u64,
    mtime_sec: i64,
    mtime_nsec: i64,
    ctime_sec: i64,
    ctime_nsec: i64,
    len: u64,
}

struct WatcherHandle {
    nudge: async_channel::Sender<()>,
    _stop_async: async_channel::Sender<()>,
    _stop_sync: mpsc::Sender<()>,
}

pub(crate) struct HaskellAdapter {
    settings: Value,
    watcher: Option<WatcherHandle>,
}

impl HaskellAdapter {
    pub(crate) fn new() -> Self {
        Self {
            settings: json!({}),
            watcher: None,
        }
    }
}

impl ServerAdapter for HaskellAdapter {
    fn resolve_root(&self, workspace_root: &Path, file_path: &Path) -> PathBuf {
        let home = env::var_os("HOME").map(PathBuf::from);
        resolve_haskell_root(workspace_root, file_path, home.as_deref())
    }

    fn command(&self, _: &Path) -> CommandSpec {
        CommandSpec::from(ServerKind::Haskell)
    }

    fn customize_initialize(&mut self, _: &Path, params: &mut Value) {
        params["capabilities"]["workspace"]["didChangeWatchedFiles"] = json!({
            "dynamicRegistration": true,
            "relativePatternSupport": true
        });
    }

    fn on_initialized(
        &mut self,
        root: &Path,
        result: &Value,
        writer: &mpsc::SyncSender<Value>,
    ) -> Result<()> {
        if result
            .pointer("/serverInfo/version")
            .and_then(Value::as_str)
            .and_then(parse_hls_major_minor)
            .is_some_and(|version| version >= (2, 15))
        {
            self.settings = json!({
                "haskell": {
                    "componentsLoading": "multi: whole-project"
                }
            });
            queue_message(
                writer,
                &json!({
                    "jsonrpc": "2.0",
                    "method": "workspace/didChangeConfiguration",
                    "params": { "settings": self.settings }
                }),
            )?;
        }
        self.watcher = Some(spawn_watcher(root.to_path_buf(), writer.clone()));
        Ok(())
    }

    fn configuration(&self, section: Option<&str>) -> Value {
        lookup_section(&self.settings, section)
    }

    fn handle_client_request(&mut self, method: &str, _: &Value) -> Option<Value> {
        match method {
            "client/registerCapability" | "client/unregisterCapability" => Some(Value::Null),
            _ => None,
        }
    }

    fn on_client_message(&mut self, method: &str, _: &Value, _: &mpsc::SyncSender<Value>) {
        if method == "textDocument/didSave"
            && let Some(watcher) = &self.watcher
        {
            let _ = watcher.nudge.try_send(());
        }
    }
}

fn parse_hls_major_minor(raw: &str) -> Option<(u32, u32)> {
    let token = raw
        .trim()
        .split(|ch: char| !ch.is_ascii_digit() && ch != '.')
        .find(|part| !part.is_empty())?;
    let mut parts = token.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// Match `hie-bios` / `implicit-hie` cradle discovery up to the repository or `$HOME` boundary:
/// 1. Nearest `hie.yaml`
/// 2. Outermost multi-package root (`cabal.project`, `cabal.project.local`, `stack.yaml`)
/// 3. Nearest single-package manifest (`package.yaml` or `*.cabal`)
/// 4. Fallback to `workspace_root`
fn resolve_haskell_root(workspace_root: &Path, file_path: &Path, home: Option<&Path>) -> PathBuf {
    let start = file_path.parent().unwrap_or(workspace_root);
    let mut chain = Vec::new();
    for dir in start.ancestors() {
        if dir != workspace_root && dir != start && home == Some(dir) {
            break;
        }
        chain.push(dir);
        if dir.join(".git").exists() {
            break;
        }
    }

    if let Some(dir) = chain
        .iter()
        .copied()
        .find(|dir| dir.join("hie.yaml").is_file())
    {
        return dir.to_path_buf();
    }
    if let Some(dir) = chain.iter().rev().copied().find(|dir| {
        dir.join("cabal.project").is_file()
            || dir.join("cabal.project.local").is_file()
            || dir.join("stack.yaml").is_file()
    }) {
        return dir.to_path_buf();
    }
    if let Some(dir) = chain
        .iter()
        .copied()
        .find(|dir| dir.join("package.yaml").is_file() || has_cabal_file(dir))
    {
        return dir.to_path_buf();
    }
    workspace_root.to_path_buf()
}

fn has_cabal_file(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry.file_type().is_ok_and(|kind| kind.is_file())
            && Path::new(&entry.file_name()).extension() == Some(OsStr::new("cabal"))
    })
}

fn spawn_watcher(root: PathBuf, writer: mpsc::SyncSender<Value>) -> WatcherHandle {
    let (nudge, nudge_rx) = async_channel::bounded(1);
    let (stop_async, stop_async_rx) = async_channel::bounded(1);
    let (stop_sync, stop_sync_rx) = mpsc::channel();

    let _ = thread::Builder::new()
        .name("rockstar-hls-watch".into())
        .spawn(move || run_watcher(root, writer, nudge_rx, stop_async_rx, stop_sync_rx));

    WatcherHandle {
        nudge,
        _stop_async: stop_async,
        _stop_sync: stop_sync,
    }
}

fn run_watcher(
    root: PathBuf,
    writer: mpsc::SyncSender<Value>,
    nudge_rx: async_channel::Receiver<()>,
    stop_async_rx: async_channel::Receiver<()>,
    stop_sync_rx: mpsc::Receiver<()>,
) {
    enum Wake {
        Event(Option<WatchUpdate>),
        Nudge(Option<()>),
        Stop,
    }

    let (watch, updates_rx) = FsWatch::new();
    let (mut directories, mut known_files) = scan_workspace(&root);
    watch.set_targets(WatchTargets {
        directories: directories.clone(),
    });

    loop {
        let wake = futures_lite::future::block_on(futures_lite::future::or(
            async { Wake::Event(updates_rx.recv().await.ok()) },
            futures_lite::future::or(async { Wake::Nudge(nudge_rx.recv().await.ok()) }, async {
                let _ = stop_async_rx.recv().await;
                Wake::Stop
            }),
        ));
        match wake {
            Wake::Stop | Wake::Event(None) | Wake::Nudge(None) => break,
            Wake::Event(Some(WatchUpdate::Error(_))) => continue,
            Wake::Event(Some(WatchUpdate::Changed(_) | WatchUpdate::Rescan))
            | Wake::Nudge(Some(())) => {}
        }

        // Coalesce atomic-save temp renames and multi-file edits while waking immediately on drop.
        match stop_sync_rx.recv_timeout(WATCH_DEBOUNCE) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        while updates_rx.try_recv().is_ok() {}
        while nudge_rx.try_recv().is_ok() {}

        let (next_directories, next_files) = scan_workspace(&root);
        if next_directories != directories {
            directories = next_directories;
            watch.set_targets(WatchTargets {
                directories: directories.clone(),
            });
        }

        let changes = diff_files(&known_files, &next_files);
        known_files = next_files;
        if changes.is_empty() {
            continue;
        }
        for chunk in changes.chunks(CHANGE_BATCH_SIZE) {
            let notification = json!({
                "jsonrpc": "2.0",
                "method": "workspace/didChangeWatchedFiles",
                "params": { "changes": chunk }
            });
            if writer.send(notification).is_err() {
                return;
            }
        }
    }
}

fn scan_workspace(root: &Path) -> (HashSet<PathBuf>, HashMap<PathBuf, FileStamp>) {
    let mut directories = HashSet::from([root.to_path_buf()]);
    let mut files = HashMap::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_symlink() {
                continue;
            }
            let name = entry.file_name();
            let path = entry.path();
            if kind.is_dir() {
                if !is_ignored_dir(&name) && directories.len() < MAX_WATCH_DIRS {
                    directories.insert(path.clone());
                    stack.push(path);
                }
            } else if kind.is_file()
                && files.len() < MAX_WATCHED_FILES
                && is_watched_haskell_file(&name)
                && let Ok(meta) = entry.metadata()
            {
                files.insert(
                    path,
                    FileStamp {
                        dev: meta.dev(),
                        ino: meta.ino(),
                        mtime_sec: meta.mtime(),
                        mtime_nsec: meta.mtime_nsec(),
                        ctime_sec: meta.ctime(),
                        ctime_nsec: meta.ctime_nsec(),
                        len: meta.len(),
                    },
                );
            }
        }
    }
    (directories, files)
}

fn is_ignored_dir(name: &OsStr) -> bool {
    let bytes = name.as_bytes();
    bytes.starts_with(b".")
        || matches!(
            bytes,
            b"dist-newstyle" | b"dist" | b"target" | b"node_modules" | b"result"
        )
}

fn is_watched_haskell_file(name: &OsStr) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.starts_with(b".") || bytes.ends_with(b"~") {
        return false;
    }
    if matches!(
        bytes,
        b"hie.yaml"
            | b"cabal.project"
            | b"cabal.project.local"
            | b"cabal.project.freeze"
            | b"package.yaml"
            | b"stack.yaml"
            | b"stack.yaml.lock"
    ) {
        return true;
    }
    matches!(
        Path::new(name).extension().map(OsStr::as_bytes),
        Some(b"hs" | b"lhs" | b"hsc" | b"hs-boot" | b"lhs-boot" | b"chs" | b"cabal")
    )
}

fn diff_files(
    previous: &HashMap<PathBuf, FileStamp>,
    current: &HashMap<PathBuf, FileStamp>,
) -> Vec<Value> {
    let mut events: Vec<(&Path, u8)> = Vec::new();
    for (path, stamp) in current {
        match previous.get(path) {
            None => events.push((path, FILE_CREATED)),
            Some(old) if old != stamp => events.push((path, FILE_CHANGED)),
            Some(_) => {}
        }
    }
    for path in previous.keys() {
        if !current.contains_key(path) {
            events.push((path, FILE_DELETED));
        }
    }
    events.sort_by_key(|(a, _)| *a);
    events
        .into_iter()
        .filter_map(|(path, change_type)| {
            let uri = language::uri(path).ok()?;
            Some(json!({
                "uri": uri,
                "type": change_type
            }))
        })
        .collect()
}
