use std::path::Path;
use gpui_kit::{
    App, InteractiveElement as _, IntoElement, ParentElement as _, Styled as _,
    component::ActiveTheme as _, div, prelude::FluentBuilder as _, px, rgb, rgba,
};

use crate::{agent::Message, markdown::MarkdownContent};

pub(super) enum ChatMessage {
    User(MarkdownContent),
    Assistant(MarkdownContent),
}

impl ChatMessage {
    pub(super) fn new(message: Message, base: &Path, cx: &mut App) -> Self {
        match message {
            Message::User(text) => Self::User(MarkdownContent::new(&text, base, cx)),
            Message::Assistant(text) => Self::Assistant(MarkdownContent::new(&text, base, cx)),
        }
    }

    pub(super) fn append_text(&mut self, text: &str, cx: &mut App) {
        let (Self::User(content) | Self::Assistant(content)) = self;
        content.append_text(text, cx);
    }

    pub(super) fn set_text(&mut self, text: &str, cx: &mut App) {
        let (Self::User(content) | Self::Assistant(content)) = self;
        content.set_text(text, cx);
    }

    pub(super) fn set_visible(&self, visible: bool, cx: &mut App) {
        let (Self::User(content) | Self::Assistant(content)) = self;
        content.set_visible(visible, cx);
    }

    pub(super) fn shutdown(&self, cx: &mut App) {
        let (Self::User(content) | Self::Assistant(content)) = self;
        content.shutdown(cx);
    }

    pub(super) fn view(&self, index: usize, current: bool, cx: &App) -> impl IntoElement + use<> {
        let (label, color, markdown) = match self {
            Self::User(markdown) => ("You", rgb(0x2563eb), markdown),
            Self::Assistant(markdown) => ("Assistant", rgb(0x0f766e), markdown),
        };
        div()
            .id(("message", index))
            .w_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap_2()
            .border_l_2()
            .border_color(if current { color } else { rgba(0x00000000) })
            .pl_3()
            .when(current, |message| message.bg(cx.theme().accent))
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
