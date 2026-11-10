//! Keyboard-invoked documentation. Mouse hover continues to use the native popover.
use super::*;
use crate::markdown::MarkdownContent;
use gpui_kit::{AnyElement, ScrollHandle, deferred, relative};
use lsp_types::{HoverContents, MarkedString, MarkupKind};

pub(super) struct DocumentationPanel {
    pub content: MarkdownContent,
    pub scroll: ScrollHandle,
    pub version: i32,
    pub position: usize,
}

/// Normalize only the LSP envelope, not the Markdown supplied by the server.
/// Plain text must not turn into headings, HTML or code through Markdown parsing.
pub(super) fn hover_markdown(contents: &HoverContents) -> Option<String> {
    fn marked_string(value: &MarkedString) -> String {
        match value {
            MarkedString::String(value) => value.clone(),
            MarkedString::LanguageString(value) => {
                if value.value.trim().is_empty() {
                    return String::new();
                }
                // Tilde fences allow backticks in the info string and code. A
                // longer fence also safely contains examples with tilde fences.
                let longest = value
                    .value
                    .split(|c| c != '~')
                    .map(str::len)
                    .max()
                    .unwrap_or(0);
                let fence = "~".repeat(3.max(longest + 1));
                let language = value.language.split_whitespace().next().unwrap_or("");
                format!("{fence} {language}\n{}\n{fence}", value.value)
            }
        }
    }

    let markdown = match contents {
        HoverContents::Scalar(value) => marked_string(value),
        HoverContents::Array(values) => values
            .iter()
            .map(marked_string)
            .filter(|value| !value.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        HoverContents::Markup(value) if value.kind == MarkupKind::PlainText => {
            let mut escaped = String::new();
            let mut line_start = true;
            for c in value.value.replace("\r\n", "\n").chars() {
                match c {
                    '\n' => {
                        escaped.push_str("  \n");
                        line_start = true;
                    }
                    ' ' if line_start => escaped.push_str("&#32;"),
                    '\t' => escaped.push_str("&#9;"),
                    c => {
                        if c.is_ascii_punctuation() {
                            escaped.push('\\');
                        }
                        escaped.push(c);
                        line_start = false;
                    }
                }
            }
            // Test emptiness before escaping whitespace into character references.
            if value.value.trim().is_empty() {
                return None;
            }
            escaped
        }
        HoverContents::Markup(value) => value.value.clone(),
    };
    (!markdown.trim().is_empty()).then_some(markdown)
}

impl FileView {
    pub(crate) fn set_media_visible(&self, visible: bool, cx: &mut App) {
        if let Some(preview) = &self.preview {
            preview.content.set_visible(visible && preview.active, cx);
        }
        if let Some(panel) = &self.lsp_ui.documentation {
            panel.content.set_visible(visible, cx);
        }
    }

    pub(super) fn show_documentation(
        &mut self,
        markdown: String,
        version: i32,
        position: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_lsp_popup(window, cx);
        self.input
            .update(cx, |input, cx| input.dismiss_lsp_overlays(cx));
        let path = self.buffer.read(cx).path.clone();
        let panel = DocumentationPanel {
            content: MarkdownContent::new(&markdown, path.parent().unwrap(), cx),
            scroll: ScrollHandle::new(),
            version,
            position,
        };
        let focus = panel.content.focus_handle(cx);
        self.lsp_ui.documentation = Some(panel);
        cx.set_global(Mode::LspPopup);
        focus.focus(window, cx);
        cx.notify();
    }

    pub(super) fn close_documentation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_lsp_popup(window, cx);
    }

    pub(super) fn documentation_key(
        &mut self,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(panel) = &self.lsp_ui.documentation else {
            return false;
        };
        if key.key == "escape" {
            self.close_documentation(window, cx);
            return true;
        }
        // Let the shared TextView handle selection and copying. Other keys
        // cannot reach Vim or edit the source while documentation is open.
        if (key.modifiers.control || key.modifiers.platform)
            && !key.modifiers.alt
            && matches!(key.key.as_str(), "a" | "c")
        {
            return false;
        }
        if key.modifiers.control || key.modifiers.platform || key.modifiers.alt {
            return true;
        }
        let line = cx.theme().font_size * 1.5;
        let page = panel.scroll.bounds().size.height * 0.9;
        let delta = match text_key(key) {
            "down" | "j" => line,
            "up" | "k" => -line,
            "pagedown" => page,
            "pageup" => -page,
            _ => return true,
        };
        let mut offset = panel.scroll.offset();
        offset.y = (offset.y - delta).clamp(-panel.scroll.max_offset().y, px(0.));
        panel.scroll.set_offset(offset);
        cx.notify();
        true
    }

    pub(super) fn render_documentation(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let panel = self.lsp_ui.documentation.as_ref()?;
        // This is a sibling of Editor, so it inherits the same root typography
        // as chat, not Editor's monospace/line-height overrides. Keep the body
        // padding and Markdown renderer identical to the conversation view.
        Some(
            deferred(
                div()
                    .id("documentation-overlay")
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        // The backdrop is a sibling, not an ancestor that stops
                        // the text's mouse-down bubbling. TextSelectionLayer
                        // needs that event to start a pointer selection.
                        div()
                            .id("documentation-backdrop")
                            .absolute()
                            .inset_0()
                            .occlude()
                            .on_any_mouse_down(cx.listener(|this, _, window, cx| {
                                this.close_documentation(window, cx);
                                window.prevent_default();
                                cx.stop_propagation();
                            }))
                            .on_scroll(|_, _, cx| cx.stop_propagation()),
                    )
                    .child(
                        div()
                            .id("documentation-panel")
                            .role(Role::Dialog)
                            .aria_label("Documentation")
                            .track_focus(&self.lsp_ui.focus)
                            .w(relative(0.9))
                            .h(relative(0.8))
                            .min_w_0()
                            .min_h_0()
                            .flex()
                            .flex_col()
                            .bg(cx.theme().background)
                            .border_1()
                            .border_color(cx.theme().border)
                            .shadow_lg()
                            .occlude()
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .px_6()
                                    .py_2()
                                    .border_b_1()
                                    .border_color(cx.theme().border)
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Documentation · Esc to close"),
                            )
                            .child(
                                div()
                                    .id("documentation-scroll")
                                    .flex_1()
                                    .min_w_0()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .track_scroll(&panel.scroll)
                                    .p_6()
                                    .child(panel.content.view()),
                            ),
                    ),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }
}
