use gpui_kit::{
    Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    ScrollHandle, StatefulInteractiveElement as _, Styled as _, Window, base::TestSupportExt as _,
    div,
};

use crate::agent::Message;

use super::{
    message::ChatMessage,
    session::{ChatSession, ConversationChange},
};

/// Markdown rendering and scroll state belong to a pane, not to the session.
pub(super) struct ConversationView {
    messages: Vec<ChatMessage>,
    scroll: ScrollHandle,
}

impl ConversationView {
    pub(super) fn new(session: &Entity<ChatSession>, cx: &mut Context<Self>) -> Self {
        let messages = session
            .read(cx)
            .messages
            .clone()
            .into_iter()
            .map(|message| ChatMessage::new(message, cx))
            .collect();
        cx.subscribe(session, |this, _, change, cx| {
            match change {
                ConversationChange::Append(message) => {
                    this.messages.push(ChatMessage::new(message.clone(), cx))
                }
                ConversationChange::AppendText(index, text) => {
                    this.messages[*index].append_text(text, cx)
                }
                ConversationChange::Replace(index, message) => {
                    let (Message::User(text) | Message::Assistant(text)) = message;
                    this.messages[*index].set_text(text, cx)
                }
                ConversationChange::Remove(index) => {
                    this.messages.remove(*index);
                }
            }
            this.scroll.scroll_to_bottom();
            cx.notify();
        })
        .detach();
        Self {
            messages,
            scroll: ScrollHandle::new(),
        }
    }
}

impl Render for ConversationView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("conversation")
            .test_support()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .p_6()
            .child(
                div().flex().flex_col().gap_5().children(
                    self.messages
                        .iter()
                        .enumerate()
                        .map(|(index, message)| message.view(index)),
                ),
            )
    }
}
