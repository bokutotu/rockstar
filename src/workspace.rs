use crate::{
    agent::{Agent, AgentError},
    chat::ChatSession,
    file_browser::{self, FileTree, OpenFile},
    file_buffer::FileBuffer,
    mode::{Mode, text_key},
    pane::{Pane, PaneActivated, PaneContent},
};
use gpui_kit::{
    App, AppContext as _, Bounds, Context, Entity, EntityId, Focusable as _,
    InteractiveElement as _, IntoElement, Keystroke, ParentElement as _, Pixels, Render, Role,
    StatefulInteractiveElement as _, Styled as _, Window,
    base::{Placement, TestSupportExt as _},
    canvas,
    component::{
        ActiveTheme as _, WindowExt as _,
        button::Button,
        dialog::DialogEntrance,
        dock::{
            DockArea, DockLayout, DockPlacement, InsertTarget, PanelEvent, PanelId, panel_handle,
        },
        notification::Notification,
    },
    div, outline,
    prelude::FluentBuilder as _,
    px,
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
mod dock;
pub(crate) type AgentConnector = Arc<dyn Fn(&Path) -> Result<Agent, AgentError> + Send + Sync>;

pub(crate) struct Workspace {
    root: PathBuf,
    lsp: Entity<crate::lsp::LspWorkspace>,
    sessions: Vec<Entity<ChatSession>>,
    buffers: Vec<Entity<FileBuffer>>,
    panes: Vec<Entity<Pane>>,
    active: usize,
    dock: Entity<DockArea>,
    connect: AgentConnector,
    tree: Option<Entity<FileTree>>,
    tree_visible: bool,
    pending: String,
}

impl Workspace {
    pub(crate) fn new(
        root: PathBuf,
        connect: AgentConnector,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let dock = dock::new(window, cx);
        let lsp = cx.new(|cx| crate::lsp::LspWorkspace::new(root.clone(), window, cx));
        let mut workspace = Self {
            root,
            lsp,
            sessions: Vec::new(),
            buffers: Vec::new(),
            panes: Vec::new(),
            active: 0,
            dock,
            connect,
            tree: None,
            tree_visible: false,
            pending: String::new(),
        };
        let pane = workspace.add_pane(window, cx);
        workspace.dock.update(cx, |dock, cx| {
            dock.set_center(
                DockLayout::tabs().panel_view(panel_handle(pane), cx),
                window,
                cx,
            );
        });
        workspace
    }

    fn add_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<Pane> {
        let pane = cx.new(Pane::new);
        let id = pane.entity_id();
        cx.on_focus_in(&pane.focus_handle(cx), window, move |this, _, cx| {
            this.activate_pane(id, cx)
        })
        .detach();
        cx.subscribe(&pane, |this, pane, _: &PaneActivated, cx| {
            this.activate_pane(pane.entity_id(), cx)
        })
        .detach();
        cx.subscribe_in(
            &pane,
            window,
            |this, pane, event: &crate::file_view::Navigate, window, cx| {
                if this.panes.get(this.active) != Some(pane) {
                    return;
                }
                match crate::lsp::language::path(&event.uri) {
                    Ok(path) => this.open_file_at(
                        pane.entity_id(),
                        path,
                        Some((event.position, event.encoding)),
                        window,
                        cx,
                    ),
                    Err(error) => {
                        window.push_notification(Notification::error(format!("LSP: {error:#}")), cx)
                    }
                }
            },
        )
        .detach();
        self.panes.push(pane.clone());
        pane
    }

    fn activate_pane(&mut self, id: EntityId, cx: &mut Context<Self>) {
        if let Some(index) = self.panes.iter().position(|pane| pane.entity_id() == id) {
            if self.active != index {
                self.active = index;
                self.pending.clear();
                if !cx.global::<Mode>().is_overlay() {
                    let mode = match &self.panes[index].read(cx).content {
                        PaneContent::File(view) => view.read(cx).mode(),
                        _ => Mode::Normal,
                    };
                    cx.set_global(mode);
                }
            }
            cx.notify();
        }
    }

    pub(crate) fn focus_active(&self, window: &mut Window, cx: &mut App) {
        self.panes[self.active].update(cx, |pane, cx| pane.focus_content(window, cx));
    }

    pub(crate) fn close_active_pane(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        if self.panes.len() == 1 {
            anyhow::bail!("Cannot close the last pane.");
        }
        let pane = self.panes.remove(self.active);
        self.active = self.active.min(self.panes.len() - 1);
        self.dock
            .update(cx, |dock, cx| dock.remove_panel(pane, window, cx));
        self.focus_active(window, cx);
        cx.notify();
        Ok(())
    }

    fn show_titles(&self, cx: &mut App) {
        for pane in &self.panes {
            pane.update(cx, |pane, cx| {
                pane.show_title = true;
                cx.emit(PanelEvent::LayoutChanged);
                cx.notify();
            });
        }
    }

    fn split(&mut self, placement: Placement, window: &mut Window, cx: &mut Context<Self>) {
        let source = &self.panes[self.active];
        let node = self
            .dock
            .read(cx)
            .layout(DockPlacement::Center)
            .and_then(|tree| tree.find_panel_node(PanelId::from(source.entity_id())));
        let Some(node) = node else {
            return;
        };
        let pane = self.add_pane(window, cx);
        self.dock.update(cx, |dock, cx| {
            dock.add_panel_view(
                panel_handle(pane.clone()),
                DockPlacement::Center,
                None,
                window,
                cx,
            );
            dock.move_panel(
                PanelId::from(pane.entity_id()),
                InsertTarget::Split {
                    node,
                    placement,
                    size: None,
                },
                window,
                cx,
            );
        });
        self.active = self.panes.len() - 1;
        self.show_titles(cx);
        cx.set_global(Mode::Normal);
        self.focus_active(window, cx);
        cx.notify();
    }

    fn move_focus(&mut self, direction: Placement, window: &mut Window, cx: &mut Context<Self>) {
        let from = self.panes[self.active].read(cx).bounds.get();
        let next = self
            .panes
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != self.active)
            .filter_map(|(index, pane)| {
                let to = pane.read(cx).bounds.get();
                let (gap, overlap, distance) = match direction {
                    Placement::Left => (
                        from.left() - to.right(),
                        from.bottom().min(to.bottom()) - from.top().max(to.top()),
                        (from.center().y - to.center().y).abs(),
                    ),
                    Placement::Right => (
                        to.left() - from.right(),
                        from.bottom().min(to.bottom()) - from.top().max(to.top()),
                        (from.center().y - to.center().y).abs(),
                    ),
                    Placement::Top => (
                        from.top() - to.bottom(),
                        from.right().min(to.right()) - from.left().max(to.left()),
                        (from.center().x - to.center().x).abs(),
                    ),
                    Placement::Bottom => (
                        to.top() - from.bottom(),
                        from.right().min(to.right()) - from.left().max(to.left()),
                        (from.center().x - to.center().x).abs(),
                    ),
                };
                (f32::from(gap) >= -0.5 && f32::from(overlap) > 0.0).then_some((
                    index,
                    f32::from(gap),
                    f32::from(distance),
                ))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.2.total_cmp(&b.2)))
            .map(|(index, _, _)| index);
        if let Some(index) = next {
            self.active = index;
            cx.set_global(Mode::Normal);
            self.focus_active(window, cx);
            cx.notify();
        }
    }

    fn cycle(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.panes[self.active].clone();
        if let Some(view) = pane.read(cx).file() {
            let buffer = &view.read(cx).buffer;
            let current = self
                .buffers
                .iter()
                .position(|entry| entry == buffer)
                .unwrap_or(0);
            let next = cycle_index(current, self.buffers.len(), forward);
            let buffer = self.buffers[next].clone();
            pane.update(cx, |pane, cx| {
                pane.request += 1;
                pane.show_file(buffer, window, cx);
            });
        } else if !self.sessions.is_empty() {
            let session = pane.read(cx).session(cx);
            let current = self
                .sessions
                .iter()
                .position(|entry| Some(entry) == session.as_ref());
            let next = current.map_or(0, |current| {
                cycle_index(current, self.sessions.len(), forward)
            });
            let session = self.sessions[next].clone();
            pane.update(cx, |pane, cx| {
                pane.request += 1;
                pane.show_chat(session, &self.root, window, cx);
            });
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    pub(crate) fn show_chat(&mut self, fresh: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !fresh && self.panes[self.active].read(cx).session(cx).is_some() {
            return;
        }
        let pane = self.panes[self.active].clone();
        let pane_id = pane.entity_id();
        let request = pane.update(cx, |pane, _| {
            pane.request += 1;
            pane.request
        });
        let root = self.root.clone();
        let connect = self.connect.clone();
        let connecting = cx.background_spawn(async move { connect(&root) });
        cx.spawn_in(window, async move |this, cx| {
            let result = connecting.await;
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(pane) = this
                    .panes
                    .iter()
                    .find(|pane| pane.entity_id() == pane_id && pane.read(cx).request == request)
                    .cloned()
                else {
                    return;
                };
                match result {
                    Ok(agent) => {
                        let number = this.sessions.len() + 1;
                        let session = cx.new(|cx| ChatSession::new(agent, number, window, cx));
                        this.sessions.push(session.clone());
                        pane.update(cx, |pane, cx| {
                            pane.show_chat(session, &this.root, window, cx)
                        });
                        if this.sessions.len() > 1 {
                            this.show_titles(cx);
                        }
                        if this.panes[this.active] == pane && !cx.global::<Mode>().is_overlay() {
                            this.focus_active(window, cx);
                        }
                        cx.notify();
                    }
                    Err(error) => window.push_notification(
                        Notification::error(error.to_string()).autohide(false),
                        cx,
                    ),
                }
            });
        })
        .detach();
    }

    pub(crate) fn open_file(
        &mut self,
        pane_id: EntityId,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_file_at(pane_id, path, None, window, cx);
    }

    fn open_file_at(
        &mut self,
        pane_id: EntityId,
        path: PathBuf,
        location: Option<(lsp_types::Position, crate::lsp::language::Encoding)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pane) = self
            .panes
            .iter()
            .find(|pane| pane.entity_id() == pane_id)
            .cloned()
        else {
            return;
        };
        let request = pane.update(cx, |pane, _| {
            pane.request += 1;
            pane.request
        });
        // Resolve aliases before looking up unsaved documents. Never reload an existing buffer.
        let canonical = match path.canonicalize() {
            Ok(path) => path,
            Err(error) => {
                window.push_notification(
                    Notification::error(format!("Cannot open {}: {error}", path.display()))
                        .autohide(false),
                    cx,
                );
                return;
            }
        };
        if let Some(buffer) = self
            .buffers
            .iter()
            .find(|buffer| buffer.read(cx).path == canonical)
            .cloned()
        {
            pane.update(cx, |pane, cx| {
                pane.show_file(buffer, window, cx);
                if let Some((position, encoding)) = location
                    && let Some(file) = pane.file()
                {
                    file.update(cx, |file, cx| file.jump_to(position, encoding, window, cx));
                }
            });
            if self.panes[self.active] == pane {
                self.focus_active(window, cx);
            }
            cx.notify();
            return;
        }
        let loading = cx.background_spawn(async move { FileBuffer::load(&canonical) });
        cx.spawn_in(window, async move |this, cx| {
            let result = loading.await;
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(pane) = this
                    .panes
                    .iter()
                    .find(|pane| pane.entity_id() == pane_id && pane.read(cx).request == request)
                    .cloned()
                else {
                    return;
                };
                match result {
                    Ok(document) => {
                        let buffer = this
                            .buffers
                            .iter()
                            .find(|buffer| buffer.read(cx).path == document.path)
                            .cloned()
                            .unwrap_or_else(|| {
                                let buffer = cx.new(|_| document);
                                match this.lsp.update(cx, |lsp, cx| lsp.bind(&buffer, cx)) {
                                    Ok(binding) => {
                                        buffer.update(cx, |buffer, _| buffer.lsp = binding)
                                    }
                                    Err(error) => window.push_notification(
                                        Notification::error(format!("{error:#}")),
                                        cx,
                                    ),
                                }
                                this.buffers.push(buffer.clone());
                                buffer
                            });
                        pane.update(cx, |pane, cx| {
                            pane.show_file(buffer, window, cx);
                            if let Some((position, encoding)) = location
                                && let Some(file) = pane.file()
                            {
                                file.update(cx, |file, cx| {
                                    file.jump_to(position, encoding, window, cx)
                                });
                            }
                        });
                        if this.panes[this.active] == pane && !cx.global::<Mode>().is_overlay() {
                            this.focus_active(window, cx);
                        }
                        cx.notify();
                    }
                    Err(error) => window.push_notification(
                        Notification::error(format!("{error:#}")).autohide(false),
                        cx,
                    ),
                }
            });
        })
        .detach();
    }

    pub(crate) fn save_active(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        let Some(file) = self.panes[self.active].read(cx).file() else {
            anyhow::bail!("This pane has no file to save.");
        };
        file.update(cx, |file, cx| file.save(window, cx));
        Ok(())
    }

    pub(crate) fn open_finder(&self, window: &mut Window, cx: &mut Context<Self>) {
        file_browser::open_finder(
            cx.entity().downgrade(),
            self.panes[self.active].entity_id(),
            self.root.clone(),
            window,
            cx,
        );
    }

    fn toggle_tree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.tree_visible = !self.tree_visible;
        if self.tree_visible {
            if self.tree.is_none() {
                let tree = cx.new(|cx| FileTree::new(self.root.clone(), cx));
                cx.subscribe_in(&tree, window, |this, _, event: &OpenFile, window, cx| {
                    let pane = this.panes[this.active].entity_id();
                    this.open_file(pane, event.0.clone(), window, cx);
                })
                .detach();
                self.tree = Some(tree);
            } else if let Some(tree) = &self.tree {
                tree.update(cx, |tree, cx| tree.refresh(cx));
            }
            self.tree
                .as_ref()
                .unwrap()
                .focus_handle(cx)
                .focus(window, cx);
        } else {
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    pub(crate) fn key(
        &mut self,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let mode = *cx.global::<Mode>();
        if mode == Mode::Confirm {
            return false;
        }
        if matches!(mode, Mode::CommandPalette | Mode::FileFinder) {
            if key.key == "escape" {
                cx.set_global(Mode::Normal);
                return true;
            }
            return false;
        }
        let file = self.panes[self.active].read(cx).file();
        if matches!(mode, Mode::Search | Mode::LspPopup) {
            return file.is_some_and(|file| file.update(cx, |file, cx| file.key(key, window, cx)));
        }
        if mode == Mode::Normal && key.modifiers.control && key.key == "b" {
            self.pending.clear();
            self.toggle_tree(window, cx);
            return true;
        }
        if self.tree_visible
            && let Some(tree) = &self.tree
            && tree.focus_handle(cx).contains_focused(window, cx)
        {
            if key.key == "escape" {
                self.focus_active(window, cx);
                return true;
            }
            return tree.update(cx, |tree, cx| tree.key(key, cx));
        }
        if mode == Mode::Normal && key.modifiers.control && matches!(key.key.as_str(), "n" | "p") {
            self.pending.clear();
            self.cycle(key.key == "n", window, cx);
            return true;
        }
        let k = text_key(key);
        if mode == Mode::Normal
            && !key.modifiers.control
            && !key.modifiers.alt
            && !key.modifiers.platform
        {
            if !self.pending.is_empty() {
                self.pending.push_str(k);
                let pending = std::mem::take(&mut self.pending);
                match pending.as_str() {
                    "ss" => self.split(Placement::Bottom, window, cx),
                    "sv" => self.split(Placement::Right, window, cx),
                    "sh" => self.move_focus(Placement::Left, window, cx),
                    "sj" => self.move_focus(Placement::Bottom, window, cx),
                    "sk" => self.move_focus(Placement::Top, window, cx),
                    "sl" => self.move_focus(Placement::Right, window, cx),
                    "\\f" => self.pending = pending,
                    "\\ff" => cx.set_global(Mode::FileFinder),
                    _ => {}
                }
                return true;
            }
            if matches!(k, "s" | "\\") && file.as_ref().is_none_or(|file| !file.read(cx).pending())
            {
                self.pending.push_str(k);
                return true;
            }
            if k == ":" {
                cx.set_global(Mode::CommandPalette);
                return true;
            }
        }
        if let Some(file) = file {
            return file.update(cx, |file, cx| file.key(key, window, cx));
        }
        if key.key == "escape" {
            self.pending.clear();
            cx.set_global(Mode::Normal);
            return true;
        }
        if mode == Mode::Normal
            && k == "i"
            && !key.modifiers.control
            && !key.modifiers.alt
            && !key.modifiers.platform
        {
            if self.panes[self.active].read(cx).session(cx).is_some() {
                cx.set_global(Mode::Insert);
            }
            return true;
        }
        false
    }

    /// Used both by the native window close gesture and :quit.
    pub(crate) fn request_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.buffers.iter().any(|buffer| buffer.read(cx).saving) {
            window.push_notification(
                Notification::info("Saving files. Please wait before closing."),
                cx,
            );
            return false;
        }
        let dirty = self
            .buffers
            .iter()
            .filter(|buffer| buffer.read(cx).dirty())
            .map(|buffer| buffer.read(cx).path.display().to_string())
            .collect::<Vec<_>>();
        if dirty.is_empty() {
            return true;
        }
        if *cx.global::<Mode>() == Mode::Confirm {
            return false;
        }
        window.close_dialog(cx);
        cx.set_global(Mode::Confirm);
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .centered()
                .w(px(520.))
                .p_4()
                .entrance(DialogEntrance::None)
                .title("Unsaved files")
                .on_ok(|_, _, _| false)
                .on_close(|_, _, cx| cx.set_global(Mode::Normal))
                .child(
                    div()
                        .id("unsaved-files")
                        .test_support()
                        .role(Role::Group)
                        .aria_label("Unsaved files")
                        .child("Save these files before closing, or discard their changes:")
                        .children(dirty.iter().map(|path| div().child(path.clone())))
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .mt_3()
                                .child(Button::new("cancel-close").label("Keep editing").on_click(
                                    |_, window, cx| {
                                        window.close_dialog(cx);
                                        cx.set_global(Mode::Normal);
                                    },
                                ))
                                .child(
                                    Button::new("discard-close")
                                        .label("Discard and close")
                                        .on_click(|_, window, _| window.remove_window()),
                                ),
                        ),
                )
        });
        false
    }
}

fn cycle_index(current: usize, len: usize, forward: bool) -> usize {
    if forward {
        (current + 1) % len
    } else {
        (current + len - 1) % len
    }
}
fn paint_active_border(bounds: Bounds<Pixels>, window: &mut Window, cx: &App) {
    window.paint_quad(outline(bounds, cx.theme().border, Default::default()).border_widths(px(2.)));
}
impl Render for Workspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active_bounds = self.panes[self.active].read(cx).bounds.clone();
        div()
            .size_full()
            .relative()
            .flex()
            .when(self.tree_visible, |this| this.children(self.tree.clone()))
            .child(div().flex_1().min_w_0().h_full().child(self.dock.clone()))
            .child(
                canvas(
                    |_, _, _| {},
                    move |_, _, window, cx| paint_active_border(active_bounds.get(), window, cx),
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
    }
}
