use super::*;
use crate::markdown::MarkdownContent;
use gpui_kit::{ScrollHandle, component::button::Button};

pub(super) struct MarkdownPreview {
    pub content: MarkdownContent,
    pub scroll: ScrollHandle,
    pub active: bool,
}

impl FileView {
    pub(crate) fn toggle_markdown(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            language_settings(&self.buffer.read(cx).path).0 == "markdown",
            "Select a Markdown file first"
        );
        self.close_lsp_popup(window, cx);
        if let Some(panel) = self.lsp_ui.documentation.take() {
            panel.content.shutdown(cx);
        }
        self.search_open = false;
        self.set_mode(Mode::Normal, cx);
        if let Some(preview) = &mut self.preview {
            preview.active = !preview.active;
            preview.content.set_visible(preview.active, cx);
        } else {
            let buffer = self.buffer.read(cx);
            let text = buffer.text.clone();
            let base = buffer.path.parent().unwrap().to_path_buf();
            self.preview = Some(MarkdownPreview {
                content: MarkdownContent::new(&text, &base, cx),
                scroll: ScrollHandle::new(),
                active: true,
            });
        }
        self.focus(window, cx);
        cx.notify();
        Ok(())
    }

    pub(super) fn preview_active(&self) -> bool {
        self.preview.as_ref().is_some_and(|preview| preview.active)
    }

    pub(super) fn preview_key(
        &mut self,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if key.modifiers.alt || key.modifiers.platform {
            return false;
        }
        if !key.modifiers.control && matches!(key.key.as_str(), "escape" | "i") {
            let _ = self.toggle_markdown(window, cx);
            if key.key == "i" {
                self.set_mode(Mode::Insert, cx);
            }
            return true;
        }
        let Some(preview) = self.preview.as_ref() else {
            return false;
        };
        let height = preview.scroll.bounds().size.height.max(px(24.));
        let mut offset = preview.scroll.offset();
        match (key.modifiers.control, text_key(key)) {
            (false, "j" | "down") => offset.y -= px(24.),
            (false, "k" | "up") => offset.y += px(24.),
            (false, "pageup") | (true, "b") => offset.y += height * 0.9,
            (false, "pagedown") | (true, "f") => offset.y -= height * 0.9,
            (true, "u") => offset.y += height * 0.5,
            (true, "d") => offset.y -= height * 0.5,
            (false, "home") => offset.y = px(0.),
            (false, "end" | "G") => offset.y = -preview.scroll.max_offset().y,
            _ => return false,
        }
        offset.y = offset.y.clamp(-preview.scroll.max_offset().y, px(0.));
        preview.scroll.set_offset(offset);
        cx.notify();
        true
    }

    pub(super) fn render_preview(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let preview = self.preview.as_ref().unwrap();
        let buffer = self.buffer.read(cx);
        div()
            .id("markdown-preview")
            .role(Role::Group)
            .aria_label(format!("Markdown preview {}", buffer.path.display()))
            .size_full()
            .flex()
            .flex_col()
            .min_w_0()
            .min_h_0()
            .child(
                div()
                    .px_2()
                    .py_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .bg(cx.theme().muted)
                    .child(div().flex_1().child(if buffer.dirty() {
                        "Markdown preview · unsaved changes"
                    } else {
                        "Markdown preview"
                    }))
                    .child(
                        Button::new("markdown-source")
                            .label("Source (Esc)")
                            .on_click(cx.listener(|this, _, window, cx| {
                                let _ = this.toggle_markdown(window, cx);
                            })),
                    ),
            )
            .child(
                div()
                    .id("markdown-preview-content")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&preview.scroll)
                    .p_6()
                    .child(preview.content.view()),
            )
            .into_any_element()
    }
}
