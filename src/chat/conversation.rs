use std::path::{Path, PathBuf};
use gpui_kit::{
    Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    ScrollHandle, StatefulInteractiveElement as _, Styled as _, Window, div,
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
    base: PathBuf,
    visible: bool,
}

impl ConversationView {
    pub(super) fn new(session: &Entity<ChatSession>, base: &Path, cx: &mut Context<Self>) -> Self {
        cx.on_release(|this, cx| {
            for message in &this.messages { message.shutdown(cx); }
        }).detach();
        let messages = session
            .read(cx)
            .messages
            .clone()
            .into_iter()
            .map(|message| ChatMessage::new(message, base, cx))
            .collect();
        cx.subscribe(session, |this, _, change, cx| {
            match change {
                ConversationChange::Reset(messages) => {
                    for message in &this.messages { message.shutdown(cx); }
                    this.messages = messages.iter().cloned().map(|message| {
                        let message = ChatMessage::new(message, &this.base, cx);
                        message.set_visible(this.visible, cx);
                        message
                    }).collect();
                    this.scroll = ScrollHandle::new();
                }
                ConversationChange::Append(message) => {
                    let message = ChatMessage::new(message.clone(), &this.base, cx);
                    message.set_visible(this.visible, cx);
                    this.messages.push(message)
                }
                ConversationChange::AppendText(index, text) => {
                    this.messages[*index].append_text(text, cx)
                }
                ConversationChange::Replace(index, message) => {
                    let (Message::User(text) | Message::Assistant(text)) = message;
                    this.messages[*index].set_text(text, cx)
                }
                ConversationChange::Remove(index) => {
                    this.messages.remove(*index).shutdown(cx);
                }
            }
            this.scroll.scroll_to_bottom();
            cx.notify();
        })
        .detach();
        let scroll = ScrollHandle::new();
        scroll.scroll_to_bottom();
        Self {
            messages,
            scroll,
            base: base.into(),
            visible: true,
        }
    }

    pub(super) fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.visible = visible;
        for message in &self.messages { message.set_visible(visible, cx); }
    }
}

impl Render for ConversationView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("conversation")
            .size_full()
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
