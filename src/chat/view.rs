use std::path::Path;

use gpui_kit::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render, Role,
    StatefulInteractiveElement as _, StyleRefinement, Styled as _, Window,
    div,
};

use super::{
    composer::{Composer, ComposerView},
    conversation::ConversationView,
    session::ChatSession,
};

/// One window onto a session. Each pane retains its own scroll and cursor
/// positions when cycling away from a session and back again.
pub(crate) struct ChatPane {
    focus: FocusHandle,
    pub(crate) session: Entity<ChatSession>,
    conversation: Entity<ConversationView>,
    composer: Entity<ComposerView>,
}

impl ChatPane {
    pub(crate) fn new(
        session: Entity<ChatSession>,
        root: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            focus: cx.focus_handle(),
            conversation: cx.new(|cx| ConversationView::new(&session, root, cx)),
            composer: cx
                .new(|cx| ComposerView::new(session.clone(), root.to_path_buf(), window, cx)),
            session,
        }
    }

    pub(crate) fn set_visible(&self, visible: bool, cx: &mut App) {
        self.conversation.update(cx, |view, cx| view.set_visible(visible, cx));
    }

    pub(crate) fn focus_input(&self, window: &mut Window, cx: &mut App) {
        self.composer
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
        div()
            .id("chat")
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
                self.conversation
                    .clone()
                    .cached(StyleRefinement::default().flex_1().min_h_0().w_full()),
            )
            .child(Composer::new(&self.composer))
    }
}
