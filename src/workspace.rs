use crate::{
    agent::{Agent, AgentError},
    chat::ChatSession,
    diff::{DiffRequest, IndexAction, LocalFilter},
    file_browser::{self, FileTree, OpenFile},
    file_buffer::FileBuffer,
    mode::{Mode, text_key},
    pane::{Pane, PaneActivated, PaneContent},
};
use gpui_kit::{
    App, AppContext as _, Bounds, Context, Entity, EntityId, Focusable as _,
    EventEmitter, InteractiveElement as _, IntoElement, Keystroke, ParentElement as _, Pixels, Render, Role,
    ScrollHandle, StatefulInteractiveElement as _, Styled as _, Window,
    base::Placement,
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
mod items;
mod terminal;
mod window;
mod picker;
pub(crate) use window::WorkspaceWindow;
use items::OpenItem;
pub(crate) use items::WipeMode;

pub(crate) type AgentConnector = Arc<dyn Fn(&Path) -> Result<Agent, AgentError> + Send + Sync>;

/// One independent editing context inside a directory-scoped workspace.
pub(crate) struct Tab {
    root: PathBuf,
    visible: bool,
    watches: Entity<crate::file_watch::FileWatches>,
    lsp: Entity<crate::lsp::LspWorkspace>,
    items: Vec<OpenItem>,
    item_scroll: ScrollHandle,
    panes: Vec<Entity<Pane>>,
    active: usize,
    dock: Entity<DockArea>,
    connect: AgentConnector,
    tree: Option<Entity<FileTree>>,
    tree_visible: bool,
    pending: String,
}

#[derive(Clone, Copy)]
enum TabAction {
    Workspaces,
    Tabs,
    CloseWorkspace,
    CloseTab,
    OpenDiff(DiffRequest),
}
impl EventEmitter<TabAction> for Tab {}

impl Tab {
    pub(crate) fn new(
        root: PathBuf,
        connect: AgentConnector,
        watches: Entity<crate::file_watch::FileWatches>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let dock = dock::new(window, cx);
        let lsp = cx.new(|cx| crate::lsp::LspWorkspace::new(root.clone(), window, cx));
        let mut workspace = Self {
            root,
            visible: false,
            watches,
            lsp,
            items: Vec::new(),
            item_scroll: ScrollHandle::new(),
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
                if !this.visible || this.panes.get(this.active) != Some(pane) {
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
        if !self.visible {
            return;
        }
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
                self.reveal_active_item(cx);
            }
            cx.notify();
        }
    }

    pub(crate) fn focus_active(&self, window: &mut Window, cx: &mut App) {
        if !self.visible {
            return;
        }
        self.reveal_active_item(cx);
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
        pane.update(cx, |pane, cx| pane.set_content_visible(false, cx));
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

    pub(crate) fn show_chat(&mut self, fresh: bool, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.panes[self.active].read(cx).session(cx);
        if !fresh && session.is_some() {
            return;
        }
        // A pending reset must not keep a wiped session and its connection alive.
        let session = session.map(|session| session.downgrade());
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
                match (result, session) {
                    (Ok(agent), Some(session)) => {
                        if let Some(session) = session.upgrade() {
                            session.update(cx, |session, cx| session.reset(agent, window, cx));
                        }
                    }
                    (Ok(agent), None) => {
                        let number = this
                            .items
                            .iter()
                            .filter_map(|item| match item {
                                OpenItem::Chat(session) => Some(session.read(cx).number),
                                _ => None,
                            })
                            .max()
                            .unwrap_or(0)
                            + 1;
                        let session = cx.new(|cx| ChatSession::new(agent, number, window, cx));
                        let id = this.register_item(OpenItem::Chat(session), cx);
                        this.show_item(pane.entity_id(), id, window, cx);
                        if number > 1 {
                            this.show_titles(cx);
                        }
                    }
                    (Err(error), _) => window.push_notification(
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
        if let Some(buffer) = self.find_buffer(&canonical, cx) {
            self.show_item(pane_id, buffer.entity_id(), window, cx);
            let activate = self.visible && self.panes[self.active] == pane && !cx.global::<Mode>().is_overlay();
            pane.update(cx, |pane, cx| {
                if let Some((position, encoding)) = location
                    && let Some(file) = pane.file()
                {
                    file.update(cx, |file, cx| file.jump_to(position, encoding, activate, window, cx));
                }
            });
            return;
        }
        if location.is_none()
            && let Some(kind) = crate::media::classify_file(&path).or_else(|| crate::media::classify_file(&canonical))
        {
            let source = crate::media::MediaSource::File(canonical);
            let existing = self.items.iter().find_map(|item| match item {
                OpenItem::Media(session) if session.read(cx).source == source => Some(session.clone()),
                _ => None,
            });
            let session = existing.unwrap_or_else(|| {
                let session = cx.new(|cx| crate::media::MediaSession::new(kind, source, cx));
                self.register_item(OpenItem::Media(session.clone()), cx);
                session
            });
            self.show_item(pane_id, session.entity_id(), window, cx);
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
                        let buffer = this.find_buffer(&document.path, cx).unwrap_or_else(|| {
                            let buffer = cx.new(|_| document);
                            match this.lsp.update(cx, |lsp, cx| lsp.bind(&buffer, cx)) {
                                Ok(binding) => buffer.update(cx, |buffer, _| buffer.lsp = binding),
                                Err(error) => window.push_notification(
                                    Notification::error(format!("{error:#}")),
                                    cx,
                                ),
                            }
                            this.register_item(OpenItem::File(buffer.clone()), cx);
                            buffer
                        });
                        this.show_item(pane_id, buffer.entity_id(), window, cx);
                        let activate = this.visible && this.panes[this.active] == pane && !cx.global::<Mode>().is_overlay();
                        pane.update(cx, |pane, cx| {
                            if let Some((position, encoding)) = location
                                && let Some(file) = pane.file()
                            {
                                file.update(cx, |file, cx| {
                                    file.jump_to(position, encoding, activate, window, cx)
                                });
                            }
                        });
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

    pub(crate) fn update_diff_index(&mut self, action: IndexAction, cx: &mut Context<Self>) -> anyhow::Result<()> {
        let view = self.panes[self.active].read(cx).diff()
            .ok_or_else(|| anyhow::anyhow!("This pane has no diff; open :diff and select a file first"))?;
        view.update(cx, |view, cx| view.update_index(action, cx))
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
        if matches!(mode, Mode::CommandPalette | Mode::FileFinder | Mode::Switcher) {
            if key.key == "escape" {
                cx.set_global(Mode::Normal);
                return true;
            }
            return false;
        }
        let terminal = self.panes[self.active].read(cx).terminal();
        if mode == Mode::Insert
            && let Some(terminal) = &terminal
            && terminal.focus_handle(cx).contains_focused(window, cx)
        {
            return terminal.update(cx, |terminal, cx| terminal.key(key, window, cx));
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
            && !(mode == Mode::Normal && (self.pending.starts_with('\\') || text_key(key) == "\\"))
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
                    "\\f" | "\\w" | "\\t" | "\\g" => self.pending = pending,
                    "\\ff" => cx.set_global(Mode::FileFinder),
                    "\\ww" => cx.emit(TabAction::Workspaces),
                    "\\tt" => cx.emit(TabAction::Tabs),
                    "\\wd" => cx.emit(TabAction::CloseWorkspace),
                    "\\td" => cx.emit(TabAction::CloseTab),
                    "\\gd" => cx.emit(TabAction::OpenDiff(DiffRequest::Local(LocalFilter::All))),
                    "\\gp" => cx.emit(TabAction::OpenDiff(DiffRequest::PullRequest)),
                    "\\gi" => cx.emit(TabAction::OpenDiff(DiffRequest::Local(LocalFilter::Staged))),
                    "\\gw" => cx.emit(TabAction::OpenDiff(DiffRequest::Local(LocalFilter::Unstaged))),
                    "\\gs" | "\\gu" => {
                        let action = if pending == "\\gs" { IndexAction::Stage } else { IndexAction::Unstage };
                        if let Err(error) = self.update_diff_index(action, cx) {
                            window.push_notification(Notification::error(format!("{error:#}")), cx);
                        }
                    }
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
        if let Some(diff) = self.panes[self.active].read(cx).diff()
            && diff.update(cx, |diff, cx| diff.key(key, cx))
        {
            return true;
        }
        if let Some(terminal) = &terminal
            && terminal.update(cx, |terminal, cx| terminal.key(key, window, cx))
        {
            return true;
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
            if self.panes[self.active].read(cx).session(cx).is_some() || terminal.is_some() {
                cx.set_global(Mode::Insert);
            }
            return true;
        }
        false
    }

    fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.visible = visible;
        self.pending.clear();
        for pane in &self.panes {
            pane.update(cx, |pane, cx| pane.set_content_visible(visible, cx));
        }
        if !visible {
            for pane in &self.panes {
                pane.update(cx, |pane, cx| pane.suspend(cx));
            }
        }
        cx.notify();
    }

    fn shutdown(&mut self, cx: &mut Context<Self>) {
        self.set_visible(false, cx);
        for pane in &self.panes {
            pane.update(cx, |pane, _| pane.request += 1);
        }
        let ids = self.items.iter().map(OpenItem::id).collect::<Vec<_>>();
        for id in ids {
            for pane in &self.panes {
                pane.update(cx, |pane, cx| { pane.forget_item(id, cx); });
            }
        }
        for item in &self.items {
            if let OpenItem::Media(session) = item {
                session.update(cx, |session, cx| session.shutdown(cx));
            }
        }
        self.items.clear();
        self.lsp.update(cx, |lsp, cx| lsp.stop(cx));
    }

    pub(crate) fn reload_active_file(&self, window: &mut Window, cx: &mut Context<Self>) -> anyhow::Result<()> {
        let file = self.panes[self.active].read(cx).file().ok_or_else(|| anyhow::anyhow!("This pane has no file to reload."))?;
        let buffer = file.read(cx).buffer.clone();
        let state = buffer.read(cx);
        anyhow::ensure!(!state.saving, "File is being saved. Please wait before reloading.");
        let stamp = (state.version, state.disk_epoch);
        let path = state.path.clone();
        let buffer = buffer.downgrade();
        let reading = cx.background_spawn(async move { crate::file_buffer::read_disk(&path) });
        cx.spawn_in(window, async move |_, cx| {
            let result = reading.await;
            let _ = cx.update(|window, cx| {
                let Some(buffer) = buffer.upgrade() else { return; };
                let state = buffer.read(cx);
                if state.saving || (state.version, state.disk_epoch) != stamp {
                    window.push_notification(Notification::error("File changed while reloading; reload cancelled."), cx);
                    return;
                }
                match result {
                    Ok(contents) => buffer.update(cx, |buffer, cx| buffer.replace_from_disk(contents, cx)),
                    Err(error) => window.push_notification(Notification::error(format!("{error:#}")), cx),
                }
            });
        }).detach();
        Ok(())
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
impl Render for Tab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active_bounds = self.panes[self.active].read(cx).bounds.clone();
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .child(self.render_item_bar(cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .when(self.tree_visible, |this| this.children(self.tree.clone()))
                    .child(div().flex_1().min_w_0().h_full().child(self.dock.clone())),
            )
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
