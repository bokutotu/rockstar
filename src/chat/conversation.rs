use std::path::{Path, PathBuf};
use gpui_kit::{
    Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    ScrollHandle, StatefulInteractiveElement as _, Styled as _, Window, div,
};

use crate::{agent::Message, mode::Mode};

use super::{
    message::ChatMessage,
    session::{ChatSession, ConversationChange},
};

/// Markdown rendering and scroll state belong to a pane, not to the session.
pub(super) struct ConversationView {
    messages: Vec<ChatMessage>,
    scroll: ScrollHandle,
    /// None follows the latest message; a cursor pauses automatic scrolling.
    current: Option<usize>,
    base: PathBuf,
    visible: bool,
}

impl ConversationView {
    pub(super) fn new(session: &Entity<ChatSession>, base: &Path, cx: &mut Context<Self>) -> Self {
        cx.on_release(|this, cx| {
            for message in &this.messages { message.shutdown(cx); }
        }).detach();
        cx.observe_global::<Mode>(|_, cx| cx.notify()).detach();
        let messages: Vec<_> = session
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
                    this.current = None;
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
                    if let Some(current) = this.current
                        && *index <= current
                    {
                        this.current = this.messages.len().checked_sub(1).map(|last| {
                            if current > *index { current - 1 } else { current.min(last) }
                        });
                        if let Some(current) = this.current {
                            this.scroll.scroll_to_top_of_item(current);
                        }
                    }
                }
            }
            if this.current.is_none() {
                this.scroll.scroll_to_top_of_item(this.messages.len());
            }
            cx.notify();
        })
        .detach();
        let scroll = ScrollHandle::new();
        scroll.scroll_to_top_of_item(messages.len());
        Self {
            messages,
            scroll,
            current: None,
            base: base.into(),
            visible: true,
        }
    }

    pub(super) fn current_message(&self) -> Option<usize> {
        self.current.or_else(|| self.messages.len().checked_sub(1))
    }

    pub(super) fn move_message(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(current) = self.current_message() else { return; };
        let next = if forward {
            (current + 1).min(self.messages.len() - 1)
        } else {
            current.saturating_sub(1)
        };
        self.current = Some(next);
        self.scroll.scroll_to_top_of_item(next);
        cx.notify();
    }

    pub(super) fn follow_latest(&mut self, cx: &mut Context<Self>) {
        self.current = None;
        self.scroll.scroll_to_top_of_item(self.messages.len());
        cx.notify();
    }

    pub(super) fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.visible = visible;
        for message in &self.messages { message.set_visible(visible, cx); }
    }
}

impl Render for ConversationView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let current = (*cx.global::<Mode>() == Mode::Normal)
            .then(|| self.current_message())
            .flatten();
        div()
            .id("conversation")
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_5()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .p_6()
            .children(
                self.messages
                    .iter()
                    .enumerate()
                    .map(|(index, message)| message.view(index, current == Some(index), cx)),
            )
            // A tail anchor lets following and navigation replace the same pending
            // scroll request, so a queued new-message scroll cannot override j/k.
            .child(div().h_0().flex_shrink_0())
    }
}
