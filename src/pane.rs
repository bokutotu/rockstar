//! A stable dock panel whose content can change without disturbing the layout.
use crate::{
    chat::{ChatPane, ChatSession},
    file_buffer::FileBuffer,
    file_view::FileView,
    mode::Mode,
    terminal::{TerminalSession, TerminalView},
};
use gpui_kit::{
    App, AppContext as _, Bounds, Context, Entity, EntityId, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Pixels, Render, Role,
    StatefulInteractiveElement as _, Styled as _, Window,
    base::TestSupportExt as _,
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
}

pub(crate) struct PaneActivated;

pub(crate) struct Pane {
    pub(crate) content: PaneContent,
    focus: FocusHandle,
    chat: Option<Entity<ChatPane>>,
    files: HashMap<EntityId, Entity<FileView>>,
    terminals: HashMap<EntityId, Entity<TerminalView>>,
    pub(crate) bounds: Rc<Cell<Bounds<Pixels>>>,
    pub(crate) show_title: bool,
    /// Reject late async loads/connections after the destination has changed.
    pub(crate) request: u64,
}

impl Pane {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        Self {
            content: PaneContent::Plain,
            focus: cx.focus_handle(),
            chat: None,
            files: HashMap::new(),
            terminals: HashMap::new(),
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
        let chat = if let Some(chat) = &self.chat {
            chat.update(cx, |chat, cx| chat.show_session(session, root, window, cx));
            chat.clone()
        } else {
            let chat = cx.new(|cx| ChatPane::new(session, root, window, cx));
            self.chat = Some(chat.clone());
            chat
        };
        self.content = PaneContent::Chat(chat);
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
        cx.emit(PanelEvent::LayoutChanged);
        cx.notify();
    }

    pub(crate) fn show_terminal(
        &mut self,
        session: Entity<TerminalSession>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    pub(crate) fn terminal(&self) -> Option<Entity<TerminalView>> {
        match &self.content {
            PaneContent::Terminal(view) => Some(view.clone()),
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
        self.show_title || matches!(self.content, PaneContent::File(_))
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
                .test_support()
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
                .child("\\ff    Find a file")
                .child("Ctrl-b  File tree")
                .child("ss / sv  Split pane")
                .into_any_element(),
            PaneContent::Chat(chat) => chat.clone().into_any_element(),
            PaneContent::File(view) => view.clone().into_any_element(),
            PaneContent::Terminal(view) => view.clone().into_any_element(),
        };
        div()
            .id("pane")
            .test_support()
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
