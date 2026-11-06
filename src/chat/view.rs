use std::{collections::HashMap, path::Path};

use gpui_kit::{
    App, AppContext as _, Context, Entity, EntityId, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render, Role,
    StatefulInteractiveElement as _, StyleRefinement, Styled as _, Window,
    base::TestSupportExt as _, div,
};

use super::{
    composer::{Composer, ComposerView},
    conversation::ConversationView,
    session::ChatSession,
};

struct SessionViews {
    conversation: Entity<ConversationView>,
    composer: Entity<ComposerView>,
}

/// One window onto a session. Each pane retains its own scroll and cursor
/// positions when cycling away from a session and back again.
pub(crate) struct ChatPane {
    focus: FocusHandle,
    pub(crate) session: Entity<ChatSession>,
    views: HashMap<EntityId, SessionViews>,
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
            views: HashMap::new(),
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
        cx.notify();
    }

    pub(crate) fn focus_input(&self, window: &mut Window, cx: &mut App) {
        self.views[&self.session.entity_id()]
            .composer
            .update(cx, |composer, cx| composer.focus(window, cx));
    }
}

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
                }),
            )
            .size_full()
            .relative()
            .flex()
            .flex_col()
            // Typing in the composer must not rebuild the unchanged conversation.
            .child(
                views
                    .conversation
                    .clone()
                    .cached(StyleRefinement::default().flex_1().min_h_0().w_full()),
            )
            .child(Composer::new(&views.composer))
    }
}
