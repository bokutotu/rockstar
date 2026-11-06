use std::path::Path;

use gpui_kit::{
    App, AppContext as _, ClipboardItem, Context, Entity, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render, Role,
    StatefulInteractiveElement as _, StyleRefinement, Styled as _, Window,
    base::TestSupportExt as _, component::ActiveTheme as _, div,
};

use crate::agent::Message;

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
        // Live status changes must reach every pane without rebuilding its conversation.
        cx.observe(&session, |_, _, cx| cx.notify()).detach();
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

    pub(crate) fn move_message(&self, forward: bool, cx: &mut App) {
        self.conversation.update(cx, |view, cx| view.move_message(forward, cx));
    }

    pub(crate) fn yank_message(&self, cx: &mut App) {
        let Some(index) = self.conversation.read(cx).current_message() else { return; };
        let text = match self.session.read(cx).messages.get(index) {
            Some(Message::User(text) | Message::Assistant(text)) => text.clone(),
            None => return,
        };
        // Copy the original Markdown, not the rendered text or the role label.
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    pub(crate) fn focus_input(&self, window: &mut Window, cx: &mut App) {
        self.conversation.update(cx, |view, cx| view.follow_latest(cx));
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
            .min_w_0()
            .relative()
            .flex()
            .flex_col()
            // Typing in the composer must not rebuild the unchanged conversation.
            .child(
                self.conversation
                    .clone()
                    .cached(StyleRefinement::default().flex_1().min_h_0().w_full()),
            )
            .children(
                self.session.read(cx).activity.as_deref().map(|text| activity_view(text, cx)),
            )
            .child(Composer::new(&self.composer))
    }
}

fn activity_view(text: &str, cx: &App) -> impl IntoElement + use<> {
    div()
        .id("chat-activity")
        .role(Role::Status)
        .aria_label(text.to_owned())
        .w_full()
        .min_w_0()
        .flex_shrink_0()
        .px_4()
        .py_1()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .truncate()
        .child(text.to_owned())
        .test_support()
}
