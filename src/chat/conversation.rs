use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, Render, ScrollHandle,
    StatefulInteractiveElement as _, Styled as _, Window, base::TestSupportExt as _, div, point,
    px,
};

use super::message::ChatMessage;
use crate::agent::Message;

pub(super) struct ConversationView {
    messages: Vec<ChatMessage>,
    assistant: Option<usize>,
    scroll: ScrollHandle,
}

impl ConversationView {
    pub(super) fn new() -> Self {
        Self {
            messages: Vec::new(),
            assistant: None,
            scroll: ScrollHandle::new(),
        }
    }

    pub(super) fn append(&mut self, message: Message, cx: &mut Context<Self>) {
        self.messages.push(ChatMessage::new(message, cx));
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    pub(super) fn start_assistant(&mut self) {
        self.assistant = None;
    }

    pub(super) fn append_assistant(&mut self, text: String, cx: &mut Context<Self>) {
        if text.is_empty() {
            return;
        }
        if let Some(index) = self.assistant {
            self.messages[index].append_text(&text, cx);
        } else {
            self.assistant = Some(self.messages.len());
            self.messages
                .push(ChatMessage::new(Message::Assistant(text), cx));
        }
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    pub(super) fn finish_assistant(&mut self, text: String, cx: &mut Context<Self>) {
        if let Some(index) = self.assistant.take() {
            if text.is_empty() {
                self.messages.remove(index);
            } else {
                self.messages[index].set_text(&text, cx);
            }
        } else if !text.is_empty() {
            self.messages
                .push(ChatMessage::new(Message::Assistant(text), cx));
        }
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    pub(super) fn clear(&mut self, cx: &mut Context<Self>) {
        self.messages.clear();
        self.assistant = None;
        self.scroll.set_offset(point(px(0.0), px(0.0)));
        cx.notify();
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
