use crate::{
    file_search,
    fs_watch::{FsWatch, WatchTargets, WatchUpdate},
    mode::{Mode, text_key},
    workspace::Tab,
};
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, Entity, EntityId, EventEmitter,
    FocusHandle, Focusable, InteractiveElement as _, IntoElement, KeyBinding, Keystroke,
    MouseButton, ParentElement as _, Pixels, Render, Role, ScrollStrategy,
    StatefulInteractiveElement as _, Styled as _, Task, UniformListScrollHandle, WeakEntity,
    Window,
    base::{Disableable as _, ElementExt as _},
    component::{
        ActiveTheme as _, Sizable as _, WindowExt as _,
        button::Button,
        dialog::DialogEntrance,
        input::{Enter, Input, InputEvent, InputState, MoveDown, MoveUp},
        notification::Notification,
    },
    div,
    prelude::FluentBuilder as _,
    px, uniform_list,
};
use std::{cell::Cell, collections::HashSet, path::PathBuf, rc::Rc, time::Duration};

mod creation;
mod creation_dialog;
mod git_status;
mod icons;
mod tree_watch;
use creation::{EntryKind, initial_creation_path};
use tree_watch::TreeSnapshot;

pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-n", MoveDown, Some("FilePicker > Input")),
        KeyBinding::new("ctrl-p", MoveUp, Some("FilePicker > Input")),
    ]);
}

pub(crate) fn open_finder(
    workspace: WeakEntity<Tab>,
    target: EntityId,
    root: PathBuf,
    window: &mut Window,
    cx: &mut App,
) {
    let finder = cx.new(|cx| FileFinder::new(workspace, target, root, window, cx));
    let focus = finder.read(cx).input.focus_handle(cx);
    window.open_dialog(cx, move |dialog, _, cx| {
        dialog
            .centered()
            .w(px(640.))
            .min_h_0()
            .p_3()
            .rounded_lg()
            .bg(cx.theme().popover)
            .text_color(cx.theme().popover_foreground)
            .close_button(false)
            .entrance(DialogEntrance::None)
            .on_ok(|_, _, _| false)
            .on_close(|_, _, cx| cx.set_global(Mode::Normal))
            .child(finder.clone())
    });
    focus.focus(window, cx);
}

struct FileFinder {
    workspace: WeakEntity<Tab>,
    target: EntityId,
    root: PathBuf,
    input: Entity<InputState>,
    paths: Vec<String>,
    matches: Vec<String>,
    selected: usize,
    loading: bool,
    _scan: Task<()>,
}
impl FileFinder {
    fn new(
        workspace: WeakEntity<Tab>,
        target: EntityId,
        root: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find files…"));
        cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.filter(cx);
            }
        })
        .detach();
        let directory = root.clone();
        let scan = cx.background_spawn(async move { file_search::project_files(&directory) });
        let task = cx.spawn_in(window, async move |this, cx| {
            let result = scan.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.loading = false;
                match result {
                    Ok(paths) => {
                        this.paths = paths;
                        this.filter(cx);
                    }
                    Err(error) => window.push_notification(
                        Notification::error(format!("{error:#}")),
                        cx,
                    ),
                }
                cx.notify();
            });
        });
        Self {
            workspace,
            target,
            root,
            input,
            paths: Vec::new(),
            matches: Vec::new(),
            selected: 0,
            loading: true,
            _scan: task,
        }
    }
    fn filter(&mut self, cx: &mut Context<Self>) {
        self.matches = file_search::fuzzy_match(&self.paths, &self.input.read(cx).value(), 200);
        self.selected = 0;
        cx.notify();
    }
    fn move_selection(&mut self, down: bool, cx: &mut Context<Self>) {
        if !self.matches.is_empty() {
            self.selected = if down {
                (self.selected + 1) % self.matches.len()
            } else {
                (self.selected + self.matches.len() - 1) % self.matches.len()
            };
            cx.notify();
        }
    }
    fn accept(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.matches.get(index) {
            let path = self.root.join(path);
            cx.set_global(Mode::Normal);
            let _ = self.workspace.update(cx, |workspace, cx| {
                workspace.open_file(self.target, path, window, cx)
            });
        }
    }
}
impl Render for FileFinder {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let first = self.selected.saturating_sub(9);
        div()
            .id("file-finder")
            .role(Role::Group)
            .aria_label("Find files")
            .key_context("FilePicker")
            .w_full()
            .flex()
            .flex_col()
            .gap_1()
            .capture_action(cx.listener(|this, _: &MoveDown, _, cx| {
                this.move_selection(true, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &MoveUp, _, cx| {
                this.move_selection(false, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &Enter, window, cx| {
                this.accept(this.selected, window, cx);
                cx.stop_propagation();
            }))
            .child(Input::new(&self.input).aria_label("File query"))
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(if self.loading {
                        "Scanning…".into()
                    } else {
                        format!(
                            "{} matches · Enter to open · Esc to cancel",
                            self.matches.len()
                        )
                    }),
            )
            .when(!self.loading && self.matches.is_empty(), |this| {
                this.child(div().child("No matching files"))
            })
            .children(
                self.matches
                    .iter()
                    .enumerate()
                    .skip(first)
                    .take(10)
                    .map(|(index, path)| {
                        div()
                            .id(("file-candidate", index))
                            .role(Role::ListBoxOption)
                            .aria_label(path.clone())
                            .aria_selected(index == self.selected)
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .cursor_pointer()
                            .when(index == self.selected, |this| {
                                this.bg(cx.theme().accent)
                                    .text_color(cx.theme().accent_foreground)
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.accept(index, window, cx)
                            }))
                            .child(path.clone())
                    }),
            )
    }
}

pub(crate) struct OpenFile(pub PathBuf);
pub(crate) struct FileTree {
    root: PathBuf,
    git: git_status::GitStatus,
    paths: Vec<String>,
    visible: Vec<String>,
    expanded: HashSet<String>,
    selected: usize,
    focus: FocusHandle,
    pub(crate) bounds: Rc<Cell<Bounds<Pixels>>>,
    scroll: UniformListScrollHandle,
    loading: bool,
    scan: Option<Task<()>>,
    refresh_pending: bool,
    reveal_pending: Option<String>,
    debounce: Option<Task<()>>,
    scan_failed: bool,
    watch: FsWatch,
    window: AnyWindowHandle,
    _scan_retry: Task<()>,
}
impl EventEmitter<OpenFile> for FileTree {}
impl Focusable for FileTree {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl FileTree {
    pub(crate) fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (watch, receiver) = FsWatch::new();
        watch.set_targets(WatchTargets {
            directories: HashSet::from([
                root.clone(),
                root.parent().unwrap_or(&root).to_path_buf(),
            ]),
        });
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
        let scan_retry = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this
                    .update(cx, |this, cx| {
                        // Traversal failures are separate from OS watch failures.
                        if this.scan_failed {
                            this.request_refresh(cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let mut tree = Self {
            root,
            git: Default::default(),
            paths: Vec::new(),
            visible: Vec::new(),
            expanded: HashSet::new(),
            selected: 0,
            focus: cx.focus_handle(),
            bounds: Rc::default(),
            scroll: UniformListScrollHandle::default(),
            loading: true,
            scan: None,
            refresh_pending: false,
            reveal_pending: None,
            debounce: None,
            scan_failed: false,
            watch,
            window: window.window_handle(),
            _scan_retry: scan_retry,
        };
        tree.refresh(cx);
        tree
    }
    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
        if cx.global::<Mode>().is_overlay() {
            return;
        }
        if *cx.global::<Mode>() != Mode::Normal {
            cx.set_global(Mode::Normal);
        }
        self.focus.focus(window, cx);
    }
    fn handle_watch_update(
        &mut self,
        update: WatchUpdate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match update {
            WatchUpdate::Changed(event) => {
                if tree_watch::affects_tree(&self.root, &event) || self.git.affects(&self.root, &event) {
                    self.request_refresh(cx);
                }
            }
            WatchUpdate::Rescan => self.request_refresh(cx),
            WatchUpdate::Error(error) => window.push_notification(
                Notification::error(format!("File tree watcher: {error}")),
                cx,
            ),
        }
    }
    fn request_refresh(&mut self, cx: &mut Context<Self>) {
        self.refresh_pending = true;
        if self.scan.is_some() || self.debounce.is_some() {
            return;
        }
        // A fixed batching window keeps making progress even during continuous changes.
        self.debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.debounce = None;
                this.refresh(cx);
            });
        }));
    }
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        self.refresh_pending = true;
        if self.scan.is_some() {
            return;
        }
        self.refresh_pending = false;
        self.debounce = None;
        // Only a scan started after creation may consume the reveal request.
        let reveal = self.reveal_pending.take();
        let root = self.root.clone();
        let window = self.window;
        let scan = cx.background_spawn(async move { tree_watch::scan_tree(&root) });
        self.scan = Some(cx.spawn(async move |this, cx| {
            let result = scan.await;
            let _ = cx.update_window(window, |_, window, cx| {
                this.update(cx, |this, cx| {
                    this.scan = None;
                    match result {
                        Ok(snapshot) => {
                            this.scan_failed = snapshot.git.error.is_some();
                            this.apply_snapshot(snapshot, reveal.as_deref(), cx);
                        }
                        Err(error) => {
                            if !this.scan_failed {
                                window.push_notification(
                                    Notification::error(format!("File tree: {error:#}")),
                                    cx,
                                );
                            }
                            // Keep the snapshot and retry revealing unless a newer
                            // creation already requested its own selection.
                            if this.reveal_pending.is_none() {
                                this.reveal_pending = reveal;
                            }
                            this.scan_failed = true;
                            this.loading = false;
                            cx.notify();
                        }
                    }
                    if this.refresh_pending {
                        this.request_refresh(cx);
                    }
                })
            });
        }));
    }
    fn apply_snapshot(
        &mut self,
        snapshot: TreeSnapshot,
        reveal: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        self.watch.set_targets(WatchTargets {
            directories: snapshot.watch_directories,
        });
        if !self.loading && self.paths == snapshot.paths && self.git == snapshot.git && reveal.is_none() {
            return;
        }
        self.loading = false;
        self.git = snapshot.git;
        self.paths = snapshot.paths;
        let directories = self
            .paths
            .iter()
            .filter(|path| path.ends_with('/'))
            .map(String::as_str)
            .collect::<HashSet<_>>();
        self.expanded
            .retain(|path| directories.contains(path.as_str()));
        // Hidden/ignored entries stay hidden, even when created from the tree.
        let reveal = reveal.filter(|path| self.paths.iter().any(|entry| entry == path));
        if let Some(path) = reveal {
            for (index, _) in path.trim_end_matches('/').match_indices('/') {
                self.expanded.insert(path[..=index].to_owned());
            }
        }
        // Automatic updates keep the viewport; explicit creation reveals its entry.
        self.rebuild(cx);
        if let Some(path) = reveal
            && let Some(index) = self.visible.iter().position(|entry| entry == path)
        {
            self.selected = index;
            self.scroll.scroll_to_item(index, ScrollStrategy::Nearest);
        }
    }
    fn begin_creation(
        &mut self,
        kind: EntryKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if cx.global::<Mode>().is_overlay() || window.has_active_dialog(cx) {
            return;
        }
        self.focus(window, cx);
        let initial_path =
            initial_creation_path(self.visible.get(self.selected).map(String::as_str));
        creation_dialog::open(
            cx.entity().downgrade(),
            self.root.clone(),
            kind,
            initial_path,
            window,
            cx,
        );
    }
    fn finish_creation(&mut self, kind: EntryKind, path: PathBuf, cx: &mut Context<Self>) {
        if let Ok(relative) = path.strip_prefix(&self.root) {
            let mut relative = relative.to_string_lossy().into_owned();
            if kind == EntryKind::Directory {
                relative.push('/');
            }
            self.reveal_pending = Some(relative);
        }
        self.refresh(cx);
        if kind == EntryKind::File {
            cx.emit(OpenFile(path));
        }
    }
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let selected = self.visible.get(self.selected).cloned();
        self.visible = self
            .paths
            .iter()
            .filter(|path| {
                let trimmed = path.trim_end_matches('/');
                trimmed
                    .match_indices('/')
                    .all(|(i, _)| self.expanded.contains(&trimmed[..=i]))
            })
            .cloned()
            .collect();
        self.selected = selected
            .and_then(|path| self.visible.iter().position(|visible| visible == &path))
            .unwrap_or_else(|| self.selected.min(self.visible.len().saturating_sub(1)));
        cx.notify();
    }
    fn open(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(path) = self.visible.get(index).cloned() {
            self.selected = index;
            if path.ends_with('/') {
                if !self.expanded.remove(&path) {
                    self.expanded.insert(path);
                }
                self.rebuild(cx);
            } else {
                cx.emit(OpenFile(self.root.join(path)));
            }
        }
    }
    pub(crate) fn key(
        &mut self,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let k = text_key(key);
        match k {
            "j" | "down" => {
                self.selected = (self.selected + 1).min(self.visible.len().saturating_sub(1))
            }
            "k" | "up" => self.selected = self.selected.saturating_sub(1),
            "enter" => self.open(self.selected, cx),
            "l" | "right" => {
                if let Some(path) = self.visible.get(self.selected).cloned() {
                    if path.ends_with('/') {
                        if self.expanded.insert(path) {
                            self.rebuild(cx);
                        } else {
                            self.selected =
                                (self.selected + 1).min(self.visible.len().saturating_sub(1));
                        }
                    } else {
                        self.open(self.selected, cx);
                    }
                }
            }
            "h" | "left" => {
                if let Some(path) = self.visible.get(self.selected).cloned() {
                    if self.expanded.remove(&path) {
                        self.rebuild(cx);
                    } else if let Some((parent, _)) = path.trim_end_matches('/').rsplit_once('/')
                        && let Some(index) =
                            self.visible.iter().position(|p| p == &format!("{parent}/"))
                    {
                        self.selected = index;
                    }
                }
            }
            "r" => self.refresh(cx),
            "a" | "A"
                if !key.modifiers.control && !key.modifiers.alt && !key.modifiers.platform =>
            {
                let kind = if k == "A" || key.modifiers.shift {
                    EntryKind::Directory
                } else {
                    EntryKind::File
                };
                self.begin_creation(kind, window, cx);
                return true;
            }
            _ => return false,
        }
        self.scroll
            .scroll_to_item(self.selected, ScrollStrategy::Nearest);
        cx.notify();
        true
    }
}
impl Render for FileTree {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bounds = self.bounds.clone();
        div()
            .id("file-tree")
            .role(Role::Group)
            .aria_label("File tree")
            .track_focus(&self.focus)
            .on_prepaint(move |rect, _, _| bounds.set(rect))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.focus(window, cx);
                    cx.notify();
                }),
            )
            .w(px(260.))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(div().px_2().py_2().child(if self.loading { "Files · Scanning…" } else { "Files" }))
            .child(div().px_2().pb_1().text_xs().text_color(cx.theme().muted_foreground)
                .child(self.git.error.clone().unwrap_or_else(|| self.git.base.as_ref().map_or_else(
                    || "W: uncommitted".to_owned(),
                    |base| format!("B: vs {base} · W: uncommitted")
                ))))
            .child(
                div()
                    .flex()
                    .gap_1()
                    .px_2()
                    .pb_2()
                    .child(
                        Button::new("new-file")
                            .small()
                            .label("New File")
                            .tooltip("New file (a)")
                            .disabled(cx.global::<Mode>().is_overlay())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.begin_creation(EntryKind::File, window, cx)
                            })),
                    )
                    .child(
                        Button::new("new-folder")
                            .small()
                            .label("New Folder")
                            .tooltip("New folder (Shift+A)")
                            .disabled(cx.global::<Mode>().is_overlay())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.begin_creation(EntryKind::Directory, window, cx)
                            })),
                    ),
            )
            .child(
                uniform_list(
                    "tree-entries",
                    self.visible.len(),
                    cx.processor(|this, range: Range<usize>, _, cx| {
                        range
                            .map(|index| {
                                let path = &this.visible[index];
                                let depth = path.trim_end_matches('/').matches('/').count();
                                let label = path
                                    .trim_end_matches('/')
                                    .rsplit('/')
                                    .next()
                                    .unwrap_or(path);
                                let icon = if path.ends_with('/') {
                                    if this.expanded.contains(path) {
                                        "▾ "
                                    } else {
                                        "▸ "
                                    }
                                } else {
                                    "  "
                                };
                                let changes = this.git.paths.get(path).copied().unwrap_or_default();
                                div()
                                    .id(("tree-entry", index))
                                    .role(Role::TreeItem)
                                    .aria_label(path.clone())
                                    .aria_description(format!("{}{}", if changes.branch { "Changed from base branch. " } else { "" }, if changes.uncommitted { "Uncommitted changes." } else { "" }))
                                    .aria_selected(index == this.selected)
                                    .h(px(24.))
                                    .pl(px(8. + depth as f32 * 14.))
                                    .pr_2()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .cursor_pointer()
                                    .when(index == this.selected, |row| {
                                        row.bg(cx.theme().accent)
                                            .text_color(cx.theme().accent_foreground)
                                    })
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        if cx.global::<Mode>().is_overlay() {
                                            return;
                                        }
                                        this.focus(window, cx);
                                        this.open(index, cx);
                                    }))
                                    .child(div().w(px(10.)).flex_shrink_0().child(icon))
                                    .child(icons::view(path, this.expanded.contains(path)))
                                    .child(div().flex_1().min_w_0().truncate()
                                        .when(changes.uncommitted, |name| name.text_color(cx.theme().warning))
                                        .when(changes.branch && !changes.uncommitted, |name| name.text_color(cx.theme().info))
                                        .child(label.to_owned()))
                                    .when(changes.branch, |row| row.child(div().text_xs().text_color(cx.theme().info).child("B")))
                                    .when(changes.uncommitted, |row| row.child(div().text_xs().text_color(cx.theme().warning).child("W")))
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.scroll)
                .flex_1()
                .min_h_0(),
            )
    }
}
use std::ops::Range;
