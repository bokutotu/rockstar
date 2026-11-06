//! The Markdown renderer shared by chat messages and source documentation.
use gpui_kit::{
    App, AppContext as _, Entity, FocusHandle,
    base::{TextView, TextViewState},
};

use crate::{math::MathPlugin, mermaid::MermaidPlugin};

pub(crate) struct MarkdownContent {
    document: Entity<TextViewState>,
    inline_math: MathPlugin,
    block_math: MathPlugin,
    mermaid: MermaidPlugin,
}

impl MarkdownContent {
    pub(crate) fn new(source: &str, cx: &mut App) -> Self {
        let document = cx.new(|cx| TextViewState::markdown(source, cx).selectable(true));
        Self {
            inline_math: MathPlugin::inline(&document),
            block_math: MathPlugin::block(&document),
            mermaid: MermaidPlugin::new(&document),
            document,
        }
    }

    pub(crate) fn view(&self) -> TextView {
        TextView::new(&self.document)
            .plugin(self.inline_math.clone())
            .plugin(self.block_math.clone())
            .plugin(self.mermaid.clone())
    }

    pub(crate) fn append_text(&self, text: &str, cx: &mut App) {
        self.document
            .update(cx, |document, cx| document.push_str(text, cx));
    }

    pub(crate) fn set_text(&self, text: &str, cx: &mut App) {
        self.document
            .update(cx, |document, cx| document.set_text(text, cx));
    }

    pub(crate) fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.document.read(cx).focus_handle().clone()
    }
}
