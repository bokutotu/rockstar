use crate::{
    file_search,
    mode::{Mode, text_key},
    workspace::Tab,
};
use gpui_kit::{
    App, AppContext as _, Bounds, Context, Entity, EntityId, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyBinding, Keystroke, MouseButton, ParentElement as _,
    Pixels, Render, Role, ScrollStrategy, StatefulInteractiveElement as _, Styled as _, Task,
    UniformListScrollHandle, WeakEntity, Window,
    base::ElementExt as _,
    component::{
        ActiveTheme as _, WindowExt as _,
        dialog::DialogEntrance,
        input::{Enter, Input, InputEvent, InputState, MoveDown, MoveUp},
        notification::Notification,
    },
    div,
    prelude::FluentBuilder as _,
    px, uniform_list,
};
use std::{cell::Cell, collections::HashSet, path::PathBuf, rc::Rc};

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
                        Notification::error(format!("{error:#}")).autohide(false),
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
    paths: Vec<String>,
    visible: Vec<String>,
    expanded: HashSet<String>,
    selected: usize,
    focus: FocusHandle,
    pub(crate) bounds: Rc<Cell<Bounds<Pixels>>>,
    scroll: UniformListScrollHandle,
    loading: bool,
    scan: Option<Task<()>>,
}
impl EventEmitter<OpenFile> for FileTree {}
impl Focusable for FileTree {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl FileTree {
    pub(crate) fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut tree = Self {
            root,
            paths: Vec::new(),
            visible: Vec::new(),
            expanded: HashSet::new(),
            selected: 0,
            focus: cx.focus_handle(),
            bounds: Rc::default(),
            scroll: UniformListScrollHandle::default(),
            loading: true,
            scan: None,
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
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let scan = cx.background_spawn(async move { file_search::scan(&root) });
        self.scan = Some(cx.spawn(async move |this, cx| {
            let mut paths = scan.await;
            paths.retain(|p| p.split('/').all(|part| !part.starts_with('.')));
            // Lexicographic component ordering, folders before files at each depth.
            paths.sort_by_cached_key(|path| {
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
            let _ = this.update(cx, |this, cx| {
                this.paths = paths;
                this.loading = false;
                this.rebuild(cx);
            });
        }));
    }
    fn rebuild(&mut self, cx: &mut Context<Self>) {
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
        self.selected = self.selected.min(self.visible.len().saturating_sub(1));
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
    pub(crate) fn key(&mut self, key: &Keystroke, cx: &mut Context<Self>) -> bool {
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
            .child(div().px_2().py_2().child(if self.loading {
                "Files · Scanning…"
            } else {
                "Files"
            }))
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
                                div()
                                    .id(("tree-entry", index))
                                    .role(Role::TreeItem)
                                    .aria_label(path.clone())
                                    .aria_selected(index == this.selected)
                                    .h(px(24.))
                                    .pl(px(8. + depth as f32 * 14.))
                                    .pr_2()
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
                                    .child(format!("{icon}{label}"))
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
