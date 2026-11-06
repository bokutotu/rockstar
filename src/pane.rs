//! A stable dock panel whose content can change without disturbing the layout.
use crate::{
    chat::{ChatPane, ChatSession},
    diff::{DiffSession, DiffView},
    file_buffer::FileBuffer,
    file_view::FileView,
    mode::Mode,
    media::{MediaSession, MediaView},
    terminal::{TerminalSession, TerminalView},
};
use gpui_kit::{
    App, AppContext as _, Bounds, Context, Entity, EntityId, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Pixels, Render, Role,
    StatefulInteractiveElement as _, Styled as _, Window,
    component::{
        ActiveTheme as _,
        dock::{BasePanel, Panel, PanelEvent},
    },
    div,
};
use std::{cell::Cell, collections::HashMap, path::Path, rc::Rc};

pub(crate) enum PaneContent {
    Plain,
    Chat(Entity<ChatPane>),
    File(Entity<FileView>),
    Terminal(Entity<TerminalView>),
    Media(Entity<MediaView>),
    Diff(Entity<DiffView>),
}

pub(crate) struct PaneActivated;

pub(crate) struct Pane {
    pub(crate) content: PaneContent,
    focus: FocusHandle,
    chats: HashMap<EntityId, Entity<ChatPane>>,
    files: HashMap<EntityId, Entity<FileView>>,
    terminals: HashMap<EntityId, Entity<TerminalView>>,
    media: HashMap<EntityId, Entity<MediaView>>,
    diffs: HashMap<EntityId, Entity<DiffView>>,
    pub(crate) bounds: Rc<Cell<Bounds<Pixels>>>,
    pub(crate) show_title: bool,
    /// Reject late async loads/connections after the destination has changed.
    pub(crate) request: u64,
}

impl Pane {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        cx.on_release(|this, cx| this.set_content_visible(false, cx)).detach();
        Self {
            content: PaneContent::Plain,
            focus: cx.focus_handle(),
            chats: HashMap::new(),
            files: HashMap::new(),
            terminals: HashMap::new(),
            media: HashMap::new(),
            diffs: HashMap::new(),
            bounds: Rc::default(),
            show_title: false,
            request: 0,
        }
    }

    pub(crate) fn show_chat(
        &mut self,
        session: Entity<ChatSession>,
        root: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.item_id(cx) != Some(session.entity_id()) { self.set_content_visible(false, cx); }
        let chat = self
            .chats
            .entry(session.entity_id())
            .or_insert_with(|| cx.new(|cx| ChatPane::new(session, root, window, cx)))
            .clone();
        self.content = PaneContent::Chat(chat);
        self.set_content_visible(true, cx);
        cx.emit(PanelEvent::LayoutChanged);
        cx.notify();
    }

    pub(crate) fn show_file(
        &mut self,
        buffer: Entity<FileBuffer>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = buffer.entity_id();
        if self.item_id(cx) != Some(id) { self.set_content_visible(false, cx); }
        let view = self
            .files
            .entry(id)
            .or_insert_with(|| {
                let view = cx.new(|cx| FileView::new(buffer.clone(), window, cx));
                cx.subscribe(&view, |_, _, event: &crate::file_view::Navigate, cx| {
                    cx.emit(event.clone())
                })
                .detach();
                cx.observe(&buffer, |_, _, cx| {
                    cx.emit(PanelEvent::LayoutChanged);
                    cx.notify();
                })
                .detach();
                view
            })
            .clone();
        self.content = PaneContent::File(view);
        self.set_content_visible(true, cx);
        cx.emit(PanelEvent::LayoutChanged);
        cx.notify();
    }

    pub(crate) fn show_terminal(
        &mut self,
        session: Entity<TerminalSession>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.item_id(cx) != Some(session.entity_id()) { self.set_content_visible(false, cx); }
        let view = self.terminals.entry(session.entity_id()).or_insert_with(|| {
            let mut label = session.read(cx).label();
            cx.observe(&session, move |_, session, cx| {
                let next = session.read(cx).label();
                if next != label {
                    label = next;
                    cx.emit(PanelEvent::LayoutChanged);
                    cx.notify();
                }
            }).detach();
            cx.new(|cx| TerminalView::new(session, window, cx))
        }).clone();
        self.content = PaneContent::Terminal(view);
        cx.emit(PanelEvent::LayoutChanged);
        cx.notify();
    }

    pub(crate) fn show_diff(&mut self, session: Entity<DiffSession>, cx: &mut Context<Self>) {
        let id = session.entity_id();
        if self.item_id(cx) != Some(id) { self.set_content_visible(false, cx); }
        let view = self.diffs.entry(id).or_insert_with(|| cx.new(|cx| DiffView::new(session, cx))).clone();
        self.content = PaneContent::Diff(view);
        self.set_content_visible(true, cx);
        cx.emit(PanelEvent::LayoutChanged);
        cx.notify();
    }

    pub(crate) fn show_media(&mut self, session: Entity<MediaSession>, window: &mut Window, cx: &mut Context<Self>) {
        let id = session.entity_id();
        if self.item_id(cx) != Some(id) { self.set_content_visible(false, cx); }
        let view = self.media.entry(id).or_insert_with(|| cx.new(|cx| MediaView::new(session, false, window, cx))).clone();
        self.content = PaneContent::Media(view);
        self.set_content_visible(true, cx);
        cx.emit(PanelEvent::LayoutChanged);
        cx.notify();
    }

    pub(crate) fn set_content_visible(&self, visible: bool, cx: &mut App) {
        match &self.content {
            PaneContent::Media(view) => view.update(cx, |view, cx| view.set_visible(visible, cx)),
            PaneContent::Chat(chat) => chat.update(cx, |chat, cx| chat.set_visible(visible, cx)),
            PaneContent::File(view) => view.update(cx, |view, cx| view.set_media_visible(visible, cx)),
            PaneContent::Diff(view) => view.update(cx, |view, cx| view.set_visible(visible, cx)),
            _ => {}
        }
    }

    /// Cancel UI-only requests before hiding a tab, without stopping its sessions.
    pub(crate) fn suspend(&mut self, cx: &mut Context<Self>) {
        for view in self.files.values() {
            view.update(cx, |view, cx| view.suspend(cx));
        }
    }

    /// Forget both visible and cached views of an item. The layout stays intact.
    pub(crate) fn forget_item(&mut self, id: EntityId, cx: &mut Context<Self>) -> bool {
        let displayed = self.item_id(cx) == Some(id);
        if displayed { self.set_content_visible(false, cx); }
        self.media.remove(&id);
        self.chats.remove(&id);
        self.files.remove(&id);
        self.terminals.remove(&id);
        self.diffs.remove(&id);
        if displayed {
            self.content = PaneContent::Plain;
            self.request += 1;
            cx.emit(PanelEvent::LayoutChanged);
            cx.notify();
        }
        displayed
    }

    pub(crate) fn chat(&self) -> Option<Entity<ChatPane>> {
        match &self.content {
            PaneContent::Chat(chat) => Some(chat.clone()),
            _ => None,
        }
    }

    pub(crate) fn terminal(&self) -> Option<Entity<TerminalView>> {
        match &self.content {
            PaneContent::Terminal(view) => Some(view.clone()),
            _ => None,
        }
    }

    pub(crate) fn diff(&self) -> Option<Entity<DiffView>> {
        match &self.content {
            PaneContent::Diff(view) => Some(view.clone()),
            _ => None,
        }
    }

    /// Identity of the retained content, independent of this pane's view entities.
    pub(crate) fn item_id(&self, cx: &App) -> Option<EntityId> {
        match &self.content {
            PaneContent::Plain => None,
            PaneContent::Chat(chat) => Some(chat.read(cx).session.entity_id()),
            PaneContent::File(view) => Some(view.read(cx).buffer.entity_id()),
            PaneContent::Terminal(view) => Some(view.read(cx).session.entity_id()),
            PaneContent::Media(view) => Some(view.read(cx).session.entity_id()),
            PaneContent::Diff(view) => Some(view.read(cx).session.entity_id()),
        }
    }

    pub(crate) fn session(&self, cx: &App) -> Option<Entity<ChatSession>> {
        match &self.content {
            PaneContent::Chat(chat) => Some(chat.read(cx).session.clone()),
            _ => None,
        }
    }

    pub(crate) fn file(&self) -> Option<Entity<FileView>> {
        match &self.content {
            PaneContent::File(view) => Some(view.clone()),
            _ => None,
        }
    }

    pub(crate) fn focus_content(&self, window: &mut Window, cx: &mut App) {
        match &self.content {
            PaneContent::Plain => {
                if *cx.global::<Mode>() != Mode::Normal {
                    cx.set_global(Mode::Normal);
                }
                self.focus.focus(window, cx);
            }
            PaneContent::Chat(chat) => {
                if *cx.global::<Mode>() == Mode::Insert {
                    chat.update(cx, |chat, cx| chat.focus_input(window, cx));
                } else {
                    chat.focus_handle(cx).focus(window, cx);
                }
            }
            PaneContent::File(view) => view.update(cx, |view, cx| view.focus(window, cx)),
            PaneContent::Terminal(view) => view.update(cx, |view, cx| view.focus(window, cx)),
            PaneContent::Media(view) => view.update(cx, |view, cx| view.focus(window, cx)),
            PaneContent::Diff(view) => view.update(cx, |view, cx| view.focus(window, cx)),
        }
    }
}

impl BasePanel for Pane {
    fn panel_name(&self) -> &'static str {
        "Pane"
    }
    fn closable(&self, _: &App) -> bool {
        false
    }
}
impl Panel for Pane {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match &self.content {
            PaneContent::Plain => "Plain".to_string(),
            PaneContent::Chat(chat) => format!("Chat {}", chat.read(cx).session.read(cx).number),
            PaneContent::Terminal(view) => view.read(cx).session.read(cx).label(),
            PaneContent::Diff(view) => view.read(cx).session.read(cx).label(),
            PaneContent::Media(view) => match &view.read(cx).session.read(cx).source {
                crate::media::MediaSource::File(path) => path.file_name().unwrap_or_default().to_string_lossy().into_owned(),
                source => source.label(),
            },
            PaneContent::File(view) => {
                let buffer = view.read(cx).buffer.read(cx);
                format!(
                    "{}{}",
                    buffer
                        .path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy(),
                    if buffer.dirty() { " ●" } else { "" }
                )
            }
        }
    }
    fn title_bar(&self, _: &App) -> bool {
        self.show_title || matches!(self.content, PaneContent::File(_) | PaneContent::Media(_))
    }
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}
impl EventEmitter<PanelEvent> for Pane {}
impl EventEmitter<PaneActivated> for Pane {}
impl EventEmitter<crate::file_view::Navigate> for Pane {}
impl Focusable for Pane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Pane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match &self.content {
            PaneContent::Plain => div()
                .id("plain")
                .role(Role::Group)
                .aria_label("Plain")
                .size_full()
                .flex()
                .flex_col()
                .justify_center()
                .items_center()
                .gap_2()
                .text_color(cx.theme().muted_foreground)
                .child("Rockstar")
                .child(":chat  Start a chat")
                .child(":term  Open a terminal")
                .child(":diff / :prdiff  Review changes")
                .child("\\ff    Find a file")
                .child("Ctrl-b  File tree")
                .child("ss / sv  Split pane")
                .child("\\ww / \\tt  Workspaces / tabs")
                .into_any_element(),
            PaneContent::Chat(chat) => chat.clone().into_any_element(),
            PaneContent::File(view) => view.clone().into_any_element(),
            PaneContent::Terminal(view) => view.clone().into_any_element(),
            PaneContent::Media(view) => view.clone().into_any_element(),
            PaneContent::Diff(view) => view.clone().into_any_element(),
        };
        div()
            .id("pane")
            .role(Role::Group)
            .aria_label("Workspace pane")
            .track_focus(&self.focus)
            .size_full()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    if matches!(this.content, PaneContent::Plain) {
                        this.focus.focus(window, cx);
                    }
                    cx.emit(PaneActivated);
                }),
            )
            .child(content)
    }
}
