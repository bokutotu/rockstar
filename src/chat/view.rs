use std::{cell::Cell, collections::HashMap, path::Path, rc::Rc};

use gpui_kit::{
    App, AppContext as _, Bounds, Context, Entity, EntityId, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Pixels, Render, Role,
    StatefulInteractiveElement as _, Styled as _, Window,
    base::TestSupportExt as _,
    component::dock::{BasePanel, Panel, PanelEvent},
    div,
};

use super::{
    composer::{Composer, ComposerView},
    conversation::ConversationView,
    session::ChatSession,
};

pub(crate) struct PaneActivated;

struct SessionViews {
    conversation: Entity<ConversationView>,
    composer: Entity<ComposerView>,
}

/// One window onto a session. Each pane retains its own scroll and cursor
/// positions when cycling away from a session and back again.
pub(crate) struct ChatPane {
    focus: FocusHandle,
    pub(crate) session: Entity<ChatSession>,
    /// The outer dock frame, including the title bar when visible.
    pub(crate) bounds: Rc<Cell<Bounds<Pixels>>>,
    views: HashMap<EntityId, SessionViews>,
    pub(crate) show_title: bool,
}

impl ChatPane {
    pub(crate) fn new(
        session: Entity<ChatSession>,
        root: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut pane = Self {
            focus: cx.focus_handle(),
            session: session.clone(),
            bounds: Rc::default(),
            views: HashMap::new(),
            show_title: false,
        };
        pane.show_session(session, root, window, cx);
        pane
    }

    pub(crate) fn show_session(
        &mut self,
        session: Entity<ChatSession>,
        root: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.views
            .entry(session.entity_id())
            .or_insert_with(|| SessionViews {
                conversation: cx.new(|cx| ConversationView::new(&session, cx)),
                composer: cx
                    .new(|cx| ComposerView::new(session.clone(), root.to_path_buf(), window, cx)),
            });
        self.session = session;
        cx.emit(PanelEvent::LayoutChanged);
        cx.notify();
    }

    pub(crate) fn focus_input(&self, window: &mut Window, cx: &mut App) {
        self.views[&self.session.entity_id()]
            .composer
            .update(cx, |composer, cx| composer.focus(window, cx));
    }
}

impl BasePanel for ChatPane {
    fn panel_name(&self) -> &'static str {
        "Chat"
    }
    fn closable(&self, _: &App) -> bool {
        false
    }
}

impl Panel for ChatPane {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        format!("Chat {}", self.session.read(cx).number)
    }
    fn title_bar(&self, _: &App) -> bool {
        self.show_title
    }
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl EventEmitter<PanelEvent> for ChatPane {}
impl EventEmitter<PaneActivated> for ChatPane {}

impl Focusable for ChatPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ChatPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let views = &self.views[&self.session.entity_id()];
        div()
            .id("chat")
            .test_support()
            .role(Role::Group)
            .aria_label(format!("Chat {}", self.session.read(cx).number))
            .track_focus(&self.focus)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    if !this.focus.contains_focused(window, cx) {
                        this.focus.focus(window, cx);
                    }
                    cx.emit(PaneActivated);
                }),
            )
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .child(views.conversation.clone())
            .child(Composer::new(&views.composer))
    }
}
