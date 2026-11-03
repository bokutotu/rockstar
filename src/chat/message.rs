use gpui_kit::{
    App, AppContext as _, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    Styled as _,
    base::{TestSupportExt as _, TextView, TextViewState},
    component::ActiveTheme as _,
    div, px,
};

use crate::{agent::Message, math::MathPlugin, mermaid::MermaidPlugin};

// GPUI rendering state for a single message.
pub(super) struct MarkdownContent {
    document: Entity<TextViewState>,
    inline_math: MathPlugin,
    block_math: MathPlugin,
    mermaid: MermaidPlugin,
}

impl MarkdownContent {
    fn new(source: &str, cx: &mut App) -> Self {
        let document = cx.new(|cx| TextViewState::markdown(source, cx).selectable(true));
        Self {
            inline_math: MathPlugin::inline(&document),
            block_math: MathPlugin::block(&document),
            mermaid: MermaidPlugin::new(&document),
            document,
        }
    }

    fn view(&self) -> TextView {
        TextView::new(&self.document)
            .plugin(self.inline_math.clone())
            .plugin(self.block_math.clone())
            .plugin(self.mermaid.clone())
    }
}

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
        content
            .document
            .update(cx, |document, cx| document.push_str(text, cx));
    }

    pub(super) fn set_text(&self, text: &str, cx: &mut App) {
        let (Self::User(content) | Self::Assistant(content)) = self;
        content
            .document
            .update(cx, |document, cx| document.set_text(text, cx));
    }

    pub(super) fn view(&self, index: usize, cx: &App) -> impl IntoElement + use<> {
        let (label, markdown) = match self {
            Self::User(markdown) => ("You", markdown),
            Self::Assistant(markdown) => ("Assistant", markdown),
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
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            )
            .child(markdown.view())
    }
}
