use gpui_kit::{
    App, InteractiveElement as _, IntoElement, ParentElement as _, Styled as _,
    base::TestSupportExt as _, div, px, rgb,
};

use crate::{agent::Message, markdown::MarkdownContent};

pub(super) enum ChatMessage {
    User(MarkdownContent),
    Assistant(MarkdownContent),
}

impl ChatMessage {
    pub(super) fn new(message: Message, cx: &mut App) -> Self {
        match message {
            Message::User(text) => Self::User(MarkdownContent::new(&text, cx)),
            Message::Assistant(text) => Self::Assistant(MarkdownContent::new(&text, cx)),
        }
    }

    pub(super) fn append_text(&self, text: &str, cx: &mut App) {
        let (Self::User(content) | Self::Assistant(content)) = self;
        content.append_text(text, cx);
    }

    pub(super) fn set_text(&self, text: &str, cx: &mut App) {
        let (Self::User(content) | Self::Assistant(content)) = self;
        content.set_text(text, cx);
    }

    pub(super) fn view(&self, index: usize) -> impl IntoElement + use<> {
        let (label, color, markdown) = match self {
            Self::User(markdown) => ("You", rgb(0x2563eb), markdown),
            Self::Assistant(markdown) => ("Assistant", rgb(0x0f766e), markdown),
        };
        div()
            .id(("message", index))
            .test_support()
            .w_full()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_size(px(13.0))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(color)
                    .child(label),
            )
            .child(markdown.view())
    }
}
