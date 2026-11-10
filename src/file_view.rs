use std::{ops::Range, time::Duration};

use gpui_kit::{
    App, AppContext as _, ClipboardItem, Context, Entity, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, Keystroke, ParentElement as _, Render, Role,
    SharedString, StatefulInteractiveElement as _, Styled as _, Task, TextRun, Window, canvas,
    component::{
        ActiveTheme as _, WindowExt as _,
        input::{Editor, EditorState, Enter, Input, InputEvent, InputState, Redo, TabSize, Undo},
        notification::Notification,
    },
    div, point,
    prelude::FluentBuilder as _,
    px,
};

use crate::{
    file_buffer::{self, FileBuffer},
    mode::{Mode, text_key},
    syntax::language_settings,
};
mod documentation;
mod editing;
mod integrations;
mod keys;
mod lsp;
mod lsp_actions;
mod navigation;
mod preview;
mod replay;
mod searching;
mod vim;
use editing::InsertSession;
use lsp::LspViewState;
pub(crate) use lsp::Navigate;
pub(crate) use navigation::VimEvent;
pub(crate) use replay::key_input;
use vim::*;
pub(crate) use vim::{BufferLocation, JumpList, ReplayInput, VimSharedState};

pub(crate) struct FileView {
    pub(crate) buffer: Entity<FileBuffer>,
    input: Entity<EditorState>,
    vim: VimViewState,
    shared: Entity<VimSharedState>,
    insert: Option<InsertSession>,
    search: Entity<InputState>,
    search_open: bool,
    search_session: Option<searching::SearchSession>,
    search_highlights: gpui_kit::base::input::TextDecorationCollection,
    jumps: Entity<JumpList>,
    change: Option<RepeatableChange>,
    change_dirty: bool,
    change_index: Option<usize>,
    command_error: Option<String>,
    folds: Vec<(Range<usize>, bool)>,
    fold_enabled: bool,
    indent: usize,
    lsp_ui: LspViewState,
    preview: Option<preview::MarkdownPreview>,
}

impl FileView {
    pub(crate) fn new(
        buffer: Entity<FileBuffer>,
        shared: Entity<VimSharedState>,
        jumps: Entity<JumpList>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.on_release(|this, cx| {
            if let Some(preview) = &this.preview { preview.content.shutdown(cx); }
            if let Some(panel) = &this.lsp_ui.documentation {
                panel.content.shutdown(cx);
            }
        })
        .detach();
        cx.observe(&shared, |_, _, cx| cx.notify()).detach();
        let document = buffer.read(cx);
        let (language, indent) = language_settings(&document.path);
        let text = document.text.clone();
        let input = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(language)
                .line_number(false)
                .folding(false)
                .soft_wrap(false)
                .tab_size(TabSize {
                    tab_size: indent,
                    hard_tabs: false,
                })
                .default_value(text)
                .searchable(false)
        });
        input.update(cx, |input, cx| {
            input.set_readonly(true, cx);
            // Pair insertion is not a Neovim default and also interferes with R.
            input.set_auto_close(false, window, cx);
        });
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search…"));
        let search_highlights = searching::decorations(&input, cx);
        cx.observe_in(&buffer, window, |this, _, window, cx| {
            let text = this.buffer.read(cx).text.clone();
            if let Some(preview) = &mut this.preview { preview.content.set_text(&text, cx); }
            let old = this.input.read(cx).value();
            if old.as_ref() != text {
                // An external edit invalidates insertion replay positions.
                this.insert = None;
                this.change = None;
                this.vim.pending.clear();
                let id = this.buffer.entity_id();
                this.jumps
                    .update(cx, |jumps, _| jumps.remap(id, &old, &text));
                this.vim.cursor = map_offset(&old, &text, this.vim.cursor);
                this.vim.anchor = map_offset(&old, &text, this.vim.anchor);
                if !this.vim.mode.is_insert() {
                    this.vim.cursor = normal_cursor(&text, this.vim.cursor);
                }
                this.set_input(&text, this.vim.cursor..this.vim.cursor, true, window, cx);
                this.remap_folds(&old, &text, cx);
                this.select(cx);
            }
            cx.notify();
        })
        .detach();
        cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
            if let InputEvent::AnnotationClick(id) = event {
                this.invoke_lens(*id, window, cx);
            }
            if matches!(event, InputEvent::Blur) {
                this.vim.pending.clear();
                if let Some(insert) = &mut this.insert {
                    insert.count = 1;
                    insert.block = None;
                }
                this.buffer.update(cx, |buffer, _| buffer.end_insert());
            }
            if matches!(event, InputEvent::Change) {
                this.input_changed(window, cx);
            }
        })
        .detach();
        cx.observe_in(&input, window, |this, _, window, cx| {
            // Native Change and observation callbacks may be delivered in
            // either order. Keep the pre-edit cursor until Change commits it.
            if this.input.read(cx).value().as_ref() != this.buffer.read(cx).text {
                return;
            }
            if !this.vim.mode.is_visual()
                && !this.search_open
                && !this.input.read(cx).is_composing()
            {
                let input = this.input.read(cx);
                let selection = input.selected_range();
                let cursor = input.cursor();
                if this.vim.mode == Mode::Normal
                    && !selection.is_empty()
                    && this.input.focus_handle(cx).is_focused(window)
                {
                    let last = prev(&input.value(), selection.end);
                    (this.vim.anchor, this.vim.cursor) = if cursor == selection.start {
                        (last, selection.start)
                    } else {
                        (selection.start, last)
                    };
                    this.set_mode(Mode::Visual, cx);
                } else {
                    if this.vim.cursor != cursor {
                        this.vim.column = None;
                    }
                    this.vim.cursor = cursor;
                    if let Some(session) = &mut this.insert {
                        if session.restart_at_cursor {
                            session.start = cursor;
                            session.restart_at_cursor = false;
                        }
                    }
                }
            } else if matches!(this.vim.mode, Mode::Visual | Mode::VisualLine)
                && !this.search_open
                && this.input.focus_handle(cx).is_focused(window)
            {
                let input = this.input.read(cx);
                let selection = input.selected_range();
                let expected = visual_target(&this.buffer.read(cx).text, &this.vim, this.indent);
                if expected.ranges.first() != Some(&selection) {
                    let cursor = input.cursor();
                    if selection.is_empty() {
                        this.vim.cursor = normal_cursor(&input.value(), cursor);
                        this.set_mode(Mode::Normal, cx);
                    } else {
                        let last = prev(&input.value(), selection.end);
                        (this.vim.anchor, this.vim.cursor) = if cursor == selection.start {
                            (last, selection.start)
                        } else {
                            (selection.start, last)
                        };
                        this.set_mode(Mode::Visual, cx);
                    }
                }
            }
            if this.input.focus_handle(cx).is_focused(window) {
                window.invalidate_character_coordinates();
            }
            this.schedule_lsp_cursor(window, cx);
            cx.notify();
        })
        .detach();
        cx.subscribe_in(&search, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.update_search(cx);
            }
        })
        .detach();
        let lsp_ui = LspViewState::new(&input, cx);
        let mut view = Self {
            buffer,
            input,
            shared,
            vim: VimViewState::default(),
            insert: None,
            search,
            search_open: false,
            search_session: None,
            search_highlights,
            jumps,
            change: None,
            change_dirty: false,
            change_index: None,
            command_error: None,
            folds: Vec::new(),
            fold_enabled: true,
            indent,
            lsp_ui,
            preview: None,
        };
        view.init_lsp(window, cx);
        view
    }

    pub(crate) fn mode(&self) -> Mode {
        self.vim.mode
    }
    pub(crate) fn pending(&self) -> bool {
        !self.vim.pending.is_empty()
    }
    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
        if let Some(preview) = &self.preview && preview.active {
            if *cx.global::<Mode>() != Mode::Normal { cx.set_global(Mode::Normal); }
            preview.content.focus_handle(cx).focus(window, cx);
            return;
        }
        if self.focus_lsp_popup(window, cx) {
            return;
        }
        let mode = if self.search_open {
            Mode::Search
        } else {
            self.vim.mode
        };
        if *cx.global::<Mode>() != mode {
            cx.set_global(mode);
        }
        if self.search_open {
            self.search.focus_handle(cx).focus(window, cx);
        } else {
            self.input.focus_handle(cx).focus(window, cx);
        }
    }
    fn remember_visual(&mut self) {
        if self.vim.mode.is_visual() {
            self.vim.last_visual = Some(VisualSelection {
                kind: match self.vim.mode {
                    Mode::VisualLine => SelectionKind::Line,
                    Mode::VisualBlock => SelectionKind::Block,
                    _ => SelectionKind::Character,
                },
                anchor: self.vim.anchor,
                cursor: self.vim.cursor,
                columns: self.vim.block_columns,
            });
        }
    }
    fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        if !mode.is_visual() {
            self.remember_visual();
        }
        self.vim.mode = mode;
        self.vim.pending.clear();
        if !mode.is_insert() {
            self.buffer.update(cx, |buffer, _| buffer.end_insert());
        }
        self.input
            .update(cx, |input, cx| input.set_readonly(!mode.is_insert(), cx));
        cx.set_global(mode);
        cx.notify();
    }
    fn set_input(
        &self,
        text: &str,
        selection: Range<usize>,
        preserve_scroll: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.input.update(cx, |input, cx| {
            let scroll = input.scroll_offset();
            input.set_value(text.to_string(), window, cx);
            input.set_selected_range(selection, cx);
            if preserve_scroll {
                input.set_scroll_offset(scroll, cx);
            }
        });
    }
    fn select(&self, cx: &mut Context<Self>) {
        let text = &self.buffer.read(cx).text;
        let mut ranges = if self.vim.mode.is_visual() {
            visual_target(text, &self.vim, self.indent).ranges
        } else {
            vec![self.vim.cursor..self.vim.cursor]
        };
        let reversed = if self.vim.mode == Mode::VisualBlock {
            if self.vim.cursor < self.vim.anchor {
                ranges.reverse();
            }
            self.vim.block_columns.1 < self.vim.block_columns.0
        } else {
            self.vim.mode.is_visual() && self.vim.cursor < self.vim.anchor
        };
        self.input.update(cx, |input, cx| {
            if let Some(first) = ranges.first() {
                input.set_selected_range(first.clone(), cx);
            }
            for range in ranges.iter().skip(1) {
                input.add_selection(range.clone(), cx);
            }
            input.set_selection_reversed(reversed, cx);
        });
        cx.notify();
    }
    fn geometry(&self, cx: &App) -> ViewGeometry {
        let input = self.input.read(cx);
        let width = input.text_bounds().map_or(80., |b| {
            f32::from(b.size.width) / f32::from(cx.theme().mono_font_size) / 0.6
        });
        let left =
            (-f32::from(input.scroll_offset().x) / f32::from(cx.theme().mono_font_size) / 0.6)
                .max(0.);
        ViewGeometry {
            rows: input.visible_row_range().unwrap_or(0..20),
            left_column: left as usize,
            columns: (width as usize).max(1),
            tab: self.indent,
        }
    }
    pub(crate) fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let buffer = self.buffer.clone();
        let state = buffer.read(cx);
        if state.saving {
            return;
        }
        let (path, expected, contents) =
            (state.path.clone(), state.saved_text(), state.serialized());
        buffer.update(cx, |buffer, cx| {
            buffer.saving = true;
            buffer.disk_epoch += 1;
            cx.notify();
        });
        let saving = cx.background_spawn(async move {
            let result = file_buffer::save(&path, &expected, &contents);
            (contents, result)
        });
        cx.spawn_in(window, async move |_, cx| {
            let (contents, result) = saving.await;
            let _ = cx.update(|window, cx| match result {
                Ok(()) => buffer.update(cx, |buffer, cx| buffer.finish_save(contents, cx)),
                Err(error) => {
                    buffer.update(cx, |buffer, cx| {
                        buffer.saving = false;
                        buffer.disk_epoch += 1;
                        cx.notify();
                    });
                    window.push_notification(
                        Notification::error(format!("{error:#}")),
                        cx,
                    );
                }
            });
        })
        .detach();
    }
    fn undo(&mut self, redo: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.insert = None;
        self.change = None;
        let before = self.buffer.read(cx).text.clone();
        let cursor = self
            .buffer
            .update(cx, |buffer, cx| buffer.undo(self.vim.cursor, redo, cx));
        if let Some(cursor) = cursor {
            self.restore_view(&before, cursor, window, cx);
        }
    }
}
impl Focusable for FileView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        if let Some(preview) = &self.preview && preview.active {
            preview.content.focus_handle(cx)
        } else {
            self.input.focus_handle(cx)
        }
    }
}

impl Render for FileView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.preview_active() { return self.render_preview(cx); }
        let lsp_popup = self.render_lsp_popup(cx);
        let documentation = self.render_documentation(cx);
        let lsp_status = self.lsp_status(cx);
        let buffer = self.buffer.read(cx);
        let status = format!(
            "{} · {}:{}{}{}{}",
            self.vim.mode.label(),
            row(&buffer.text, self.vim.cursor) + 1,
            column(&buffer.text, self.vim.cursor) + 1,
            if buffer.saving {
                " · Saving…"
            } else if buffer.dirty() {
                " · Modified"
            } else {
                " · Saved"
            },
            match &buffer.disk_state {
                file_buffer::DiskState::Current => "",
                file_buffer::DiskState::Changed => " · Changed on disk (:e! to reload)",
                file_buffer::DiskState::Unavailable(_) => " · File unavailable on disk",
            },
            lsp_status
        );
        let status = if let Some((register, _)) = &self.shared.read(cx).macros.recording {
            format!("{status} · recording @{register}")
        } else {
            status
        };
        let numbers = self.input.clone();
        let input = self.input.clone();
        let cursor = self.vim.cursor;
        let mode = self.vim.mode;
        div()
            .id("fileview")
            .role(Role::Group)
            .aria_label(format!("File {}", buffer.path.display()))
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .capture_action(cx.listener(|this, _: &Undo, window, cx| {
                if this.lsp_ui.documentation.is_none() {
                    this.undo(false, window, cx);
                }
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &Redo, window, cx| {
                if this.lsp_ui.documentation.is_none() {
                    this.undo(true, window, cx);
                }
                cx.stop_propagation();
            }))
            .child(
                div()
                    .id("source-area")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .relative()
                    .child(
                        canvas(
                            |_, _, _| {},
                            move |bounds, _, window, cx| {
                                let state = numbers.read(cx);
                                let Some(rows) = state.visible_row_range() else {
                                    return;
                                };
                                let Some(height) = state.line_height() else {
                                    return;
                                };
                                let current = row(&state.value(), cursor);
                                let top = state.text_bounds().map_or(bounds.top(), |b| b.top());
                                let rows = rows
                                    .map(|line| {
                                        (line, state.display_map().buffer_line_to_display_row(line))
                                    })
                                    .collect::<Vec<_>>();
                                let folds = state.display_map().folded_ranges().to_vec();
                                for (line, display_row) in rows {
                                    if folds
                                        .iter()
                                        .any(|r| r.start_line < line && line < r.end_line)
                                    {
                                        continue;
                                    }
                                    let value: SharedString = if line == current {
                                        (line + 1).to_string()
                                    } else {
                                        line.abs_diff(current).to_string()
                                    }
                                    .into();
                                    let run = TextRun {
                                        len: value.len(),
                                        font: window.text_style().font(),
                                        color: if line == current {
                                            cx.theme().foreground
                                        } else {
                                            cx.theme().muted_foreground
                                        },
                                        background_color: None,
                                        underline: None,
                                        strikethrough: None,
                                    };
                                    let shaped = window.text_system().shape_line(
                                        value,
                                        cx.theme().mono_font_size,
                                        &[run],
                                        None,
                                    );
                                    let y = top + height * display_row as f32;
                                    if y + height >= bounds.top() && y < bounds.bottom() {
                                        let _ = shaped.paint(
                                            point(bounds.right() - shaped.width - px(8.), y),
                                            height,
                                            gpui_kit::TextAlign::Left,
                                            None,
                                            window,
                                            cx,
                                        );
                                    }
                                }
                            },
                        )
                        .w(px(48.))
                        .h_full()
                        .flex_shrink_0(),
                    )
                    .child(
                        Editor::new(&self.input)
                            .h_full()
                            .w_full()
                            .appearance(false)
                            .bordered(false)
                            .readonly(!mode.is_insert())
                            .aria_label("Source code"),
                    )
                    .child(
                        canvas(
                            |_, _, _| {},
                            move |_, _, window, cx| {
                                if (mode == Mode::Normal
                                    || matches!(mode, Mode::Replace | Mode::VirtualReplace))
                                    && input.focus_handle(cx).is_focused(window)
                                {
                                    let state = input.read(cx);
                                    let text = state.value();
                                    if let Some(mut bounds) =
                                        state.range_to_bounds(&(cursor..next(&text, cursor)))
                                    {
                                        if bounds.size.width < px(1.) {
                                            bounds.size.width = cx.theme().mono_font_size * 0.6;
                                        }
                                        window.paint_quad(gpui_kit::outline(
                                            bounds,
                                            cx.theme().foreground,
                                            Default::default(),
                                        ));
                                    }
                                }
                            },
                        )
                        .absolute()
                        .size_full(),
                    )
                    .children(documentation),
            )
            .when(self.search_open, |this| {
                this.child(
                    div()
                        .id("file-search")
                        .capture_action(cx.listener(|this, _: &Enter, window, cx| {
                            this.end_search(true, window, cx);
                            cx.stop_propagation();
                        }))
                        .child(
                            Input::new(&self.search)
                                .prefix(div().child(
                                    if self.search_session.as_ref().is_none_or(|s| s.forward) {
                                        "/"
                                    } else {
                                        "?"
                                    },
                                ))
                                .aria_label("Search source"),
                        ),
                )
            })
            .children(lsp_popup)
            .when(language_settings(&buffer.path).0 == "markdown", |view| view.child(
                gpui_kit::component::button::Button::new("open-markdown-preview")
                    .label("Markdown preview (:markdown)")
                    .on_click(cx.listener(|this, _, window, cx| { let _ = this.toggle_markdown(window, cx); }))
            ))
            .child(
                div()
                    .id("file-status")
                    .role(Role::Status)
                    .aria_label(status.clone())
                    .px_2()
                    .py_1()
                    .bg(cx.theme().muted)
                    .child(status),
            )
            .into_any_element()
    }
}

fn map_offset(old: &str, new: &str, offset: usize) -> usize {
    let edit = editing::difference(old, new);
    boundary(
        new,
        if offset < edit.range.start {
            offset
        } else if offset >= edit.range.end {
            edit.range.start + edit.replacement.len() + offset - edit.range.end
        } else {
            edit.range.start + edit.replacement.len()
        },
    )
}
