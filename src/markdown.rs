//! The Markdown renderer shared by chat messages and source documentation.
use gpui_kit::{
    App, AppContext as _, Entity, FocusHandle,
    base::{TextView, TextViewState},
};

use std::path::Path;
use crate::{math::MathPlugin, mermaid::MermaidPlugin, media::markdown::MediaPlugin};

pub(crate) struct MarkdownContent {
    document: Entity<TextViewState>,
    inline_math: MathPlugin,
    block_math: MathPlugin,
    mermaid: MermaidPlugin,
    media: MediaPlugin,
    source: String,
}

impl MarkdownContent {
    pub(crate) fn new(source: &str, base_dir: &Path, cx: &mut App) -> Self {
        let document = cx.new(|cx| TextViewState::markdown(source, cx).selectable(true));
        Self {
            inline_math: MathPlugin::inline(&document),
            block_math: MathPlugin::block(&document),
            mermaid: MermaidPlugin::new(&document),
            media: MediaPlugin::new(&document, base_dir),
            source: source.to_owned(),
            document,
        }
    }

    pub(crate) fn view(&self) -> TextView {
        TextView::new(&self.document)
            .plugin(self.inline_math.clone())
            .plugin(self.block_math.clone())
            .plugin(self.mermaid.clone())
            .plugin(self.media.clone())
            .plugin(self.media.block())
            .image_source(self.media.image_resolver())
    }

    pub(crate) fn append_text(&mut self, text: &str, cx: &mut App) {
        self.source.push_str(text);
        self.media.appended(&self.source, cx);
        self.document
            .update(cx, |document, cx| document.push_str(text, cx));
    }

    pub(crate) fn set_text(&mut self, text: &str, cx: &mut App) {
        if !text.starts_with(&self.source) {
            self.media.reconcile(text, cx);
        } else {
            self.media.appended(text, cx);
        }
        self.source = text.to_owned();
        self.document
            .update(cx, |document, cx| document.set_text(text, cx));
    }

    pub(crate) fn set_visible(&self, visible: bool, cx: &mut App) {
        self.media.set_visible(visible, cx);
    }

    pub(crate) fn shutdown(&self, cx: &mut App) {
        self.media.shutdown(cx);
    }

    pub(crate) fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.document.read(cx).focus_handle().clone()
    }
}
