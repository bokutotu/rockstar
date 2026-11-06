use std::{ops::Range, path::Path, time::Duration};

use gpui_kit::{
    App, AppContext as _, ClipboardItem, Context, Entity, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, Keystroke, ParentElement as _, Render, Role,
    SharedString, StatefulInteractiveElement as _, Styled as _, Task, TextRun, Window,
    canvas,
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
};
mod documentation;
mod lsp;
mod vim;
use lsp::LspViewState;
pub(crate) use lsp::Navigate;
use vim::*;

pub(crate) struct FileView {
    pub(crate) buffer: Entity<FileBuffer>,
    input: Entity<EditorState>,
    vim: VimState,
    search: Entity<InputState>,
    search_open: bool,
    search_origin: usize,
    insert_j: Option<(usize, usize)>,
    chord_timer: Option<Task<()>>,
    indent: usize,
    lsp_ui: LspViewState,
}

impl FileView {
    pub(crate) fn new(
        buffer: Entity<FileBuffer>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.on_release(|this, cx| {
            if let Some(panel) = &this.lsp_ui.documentation { panel.content.shutdown(cx); }
        }).detach();
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
        input.update(cx, |input, cx| input.set_readonly(true, cx));
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search (smartcase)…"));
        cx.observe_in(&buffer, window, |this, _, window, cx| {
            let text = this.buffer.read(cx).text.clone();
            let old = this.input.read(cx).value();
            if old.as_ref() != text {
                let selection = this.input.read(cx).selected_range();
                let mapped = map_offset(&old, &text, this.vim.cursor);
                this.vim.cursor = mapped;
                this.vim.anchor = map_offset(&old, &text, this.vim.anchor);
                let selection = map_offset(&old, &text, selection.start)
                    ..map_offset(&old, &text, selection.end);
                this.set_input(&text, selection, true, window, cx);
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
                this.insert_j = None;
                this.chord_timer = None;
                this.buffer.update(cx, |buffer, _| buffer.end_insert());
            }
            if matches!(event, InputEvent::Change) {
                let text = this.input.read(cx).value().to_string();
                let before = this.vim.cursor;
                this.vim.cursor = this.input.read(cx).cursor();
                let id = cx.entity_id();
                let insert = this.vim.mode == Mode::Insert;
                this.buffer
                    .update(cx, |buffer, cx| buffer.edit(text, before, id, insert, cx));
                cx.notify();
            }
        })
        .detach();
        cx.observe_in(&input, window, |this, _, window, cx| {
            if !this.vim.mode.is_visual() && !this.search_open {
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
            vim: VimState::default(),
            search,
            search_open: false,
            search_origin: 0,
            insert_j: None,
            chord_timer: None,
            indent,
            lsp_ui,
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

    fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        self.vim.mode = mode;
        self.vim.pending.clear();
        self.insert_j = None;
        self.chord_timer = None;
        if mode != Mode::Insert {
            self.buffer.update(cx, |buffer, _| buffer.end_insert());
        }
        self.input
            .update(cx, |input, cx| input.set_readonly(mode != Mode::Insert, cx));
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
        let range = if self.vim.mode == Mode::VisualLine {
            line_range(text, self.vim.anchor, self.vim.cursor)
        } else if self.vim.mode == Mode::Visual {
            self.vim.anchor.min(self.vim.cursor)..next(text, self.vim.anchor.max(self.vim.cursor))
        } else {
            self.vim.cursor..self.vim.cursor
        };
        self.input
            .update(cx, |input, cx| input.set_selected_range(range, cx));
        cx.notify();
    }

    fn replace(
        &mut self,
        range: Range<usize>,
        replacement: &str,
        cursor: usize,
        insert: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut text = self.buffer.read(cx).text.clone();
        let before = self.vim.cursor;
        text.replace_range(range, replacement);
        self.vim.cursor = boundary(&text, cursor);
        if !insert {
            self.vim.cursor = normal_cursor(&text, self.vim.cursor);
        }
        self.set_input(&text, self.vim.cursor..self.vim.cursor, true, window, cx);
        let id = cx.entity_id();
        self.buffer
            .update(cx, |buffer, cx| buffer.edit(text, before, id, insert, cx));
        self.vim.column = None;
        cx.notify();
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
                        Notification::error(format!("{error:#}")).autohide(false),
                        cx,
                    );
                }
            });
        })
        .detach();
    }

    fn undo(&mut self, redo: bool, window: &mut Window, cx: &mut Context<Self>) {
        let cursor = self
            .buffer
            .update(cx, |buffer, cx| buffer.undo(self.vim.cursor, redo, cx));
        if let Some(cursor) = cursor {
            self.vim.cursor = normal_cursor(&self.buffer.read(cx).text, cursor);
            let text = self.buffer.read(cx).text.clone();
            self.set_input(&text, self.vim.cursor..self.vim.cursor, false, window, cx);
            self.select(cx);
        }
    }

    fn yank(&self, range: Range<usize>, linewise: bool, cx: &mut App) {
        let mut text = self.buffer.read(cx).text[range].to_string();
        if linewise && !text.ends_with('\n') {
            text.push('\n');
        }
        cx.write_to_clipboard(ClipboardItem::new_string_with_metadata(
            text,
            if linewise {
                "rockstar-linewise"
            } else {
                "rockstar-characterwise"
            }
            .into(),
        ));
    }

    fn operate(
        &mut self,
        operator: char,
        mut range: Range<usize>,
        linewise: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if range.is_empty() && operator != 'c' {
            self.set_mode(Mode::Normal, cx);
            return;
        }
        if matches!(operator, 'd' | 'c' | 'y') {
            self.yank(range.clone(), linewise, cx);
        }
        let cursor = range.start;
        match operator {
            'y' => {
                self.vim.cursor = cursor;
                self.set_mode(Mode::Normal, cx);
                self.select(cx);
            }
            'd' => {
                let text = &self.buffer.read(cx).text;
                if linewise && range.end == text.len() && !text.ends_with('\n') && range.start > 0 {
                    range.start -= 1;
                }
                self.set_mode(Mode::Normal, cx);
                self.replace(range, "", cursor, false, window, cx);
            }
            'c' => {
                let text = &self.buffer.read(cx).text;
                let replacement = if linewise {
                    let indent = &text[range.start..first_nonblank(text, range.start)];
                    format!(
                        "{indent}{}",
                        if text[range.clone()].ends_with('\n') {
                            "\n"
                        } else {
                            ""
                        }
                    )
                } else {
                    String::new()
                };
                let cursor = cursor + replacement.trim_end_matches('\n').len();
                self.set_mode(Mode::Insert, cx);
                self.replace(range, &replacement, cursor, true, window, cx);
            }
            '>' | '<' => {
                let range = line_range(
                    &self.buffer.read(cx).text,
                    range.start,
                    range.end.saturating_sub(1),
                );
                let old = &self.buffer.read(cx).text[range.clone()];
                let replacement = old
                    .split_inclusive('\n')
                    .map(|line| {
                        if operator == '>' {
                            format!("{}{line}", " ".repeat(self.indent))
                        } else {
                            let remove = if line.starts_with('\t') {
                                1
                            } else {
                                line.chars()
                                    .take(self.indent)
                                    .take_while(|c| *c == ' ')
                                    .count()
                            };
                            line[remove..].to_string()
                        }
                    })
                    .collect::<String>();
                let cursor = range.start;
                self.set_mode(Mode::Normal, cx);
                self.replace(range, &replacement, cursor, false, window, cx);
                self.vim.cursor = first_nonblank(&self.buffer.read(cx).text, cursor);
                self.select(cx);
            }
            _ => {}
        }
    }

    fn update_search(&mut self, cx: &mut Context<Self>) {
        let query = self.search.read(cx).value().to_string();
        let insensitive = !query.chars().any(char::is_uppercase);
        self.input.update(cx, |input, cx| {
            input.set_search_query(query, insensitive, cx)
        });
        let ranges = self
            .input
            .read(cx)
            .search_session()
            .matcher
            .matched_ranges();
        if let Some(range) = ranges
            .iter()
            .find(|r| r.start >= self.search_origin)
            .or_else(|| ranges.first())
        {
            self.vim.cursor = range.start;
            self.select(cx);
        }
        cx.notify();
    }

    fn search_next(&mut self, forward: bool, cx: &mut Context<Self>) {
        let query = self.search.read(cx).value().to_string();
        self.input.update(cx, |input, cx| {
            input.set_search_query(query.clone(), !query.chars().any(char::is_uppercase), cx)
        });
        let ranges = self
            .input
            .read(cx)
            .search_session()
            .matcher
            .matched_ranges();
        let found = if forward {
            ranges
                .iter()
                .find(|r| r.start > self.vim.cursor)
                .or_else(|| ranges.first())
        } else {
            ranges
                .iter()
                .rev()
                .find(|r| r.start < self.vim.cursor)
                .or_else(|| ranges.last())
        };
        if let Some(range) = found {
            self.vim.cursor = range.start;
            self.select(cx);
        }
    }

    pub(crate) fn end_search(&mut self, accept: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !accept {
            self.vim.cursor = self.search_origin;
        }
        self.search_open = false;
        self.set_mode(Mode::Normal, cx);
        self.select(cx);
        self.input.focus_handle(cx).focus(window, cx);
    }

    pub(crate) fn key(
        &mut self,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.lsp_ui.documentation.is_some() {
            return self.documentation_key(key, window, cx);
        }
        if self.lsp_key(key, window, cx) {
            return true;
        }
        let k = text_key(key);
        if key.key == "escape" {
            if self.search_open {
                self.end_search(false, window, cx);
                return true;
            }
            if self.vim.mode == Mode::Normal {
                self.input.update(cx, |input, cx| input.close_search(cx));
            } else if self.vim.mode == Mode::Insert {
                let text = &self.buffer.read(cx).text;
                if self.vim.cursor > start(text, self.vim.cursor) {
                    self.vim.cursor = prev(text, self.vim.cursor);
                }
            }
            self.set_mode(Mode::Normal, cx);
            self.select(cx);
            return true;
        }
        if self.search_open {
            return false;
        }
        if key.modifiers.control {
            match key.key.as_str() {
                "r" if self.vim.mode != Mode::Insert => self.undo(true, window, cx),
                "z" => self.undo(key.modifiers.shift, window, cx),
                "d" | "u" if self.vim.mode == Mode::Normal => {
                    let rows = self
                        .input
                        .read(cx)
                        .visible_row_range()
                        .map_or(20, |r| r.len() / 2)
                        .max(1);
                    let direction = if key.key == "d" { "j" } else { "k" };
                    self.vim.cursor = motion(
                        &self.buffer.read(cx).text,
                        self.vim.cursor,
                        direction,
                        rows,
                        &mut self.vim.column,
                        true,
                    )
                    .unwrap();
                    self.select(cx);
                }
                _ => return false,
            }
            return true;
        }
        if key.modifiers.alt || key.modifiers.platform {
            return false;
        }
        if self.vim.mode == Mode::Insert {
            let at = self.input.read(cx).cursor();
            if let Some((start, count)) = self.insert_j {
                let text = &self.buffer.read(cx).text;
                let intact = at == start + count
                    && text.get(start..at).is_some_and(|s| s == "j".repeat(count));
                if intact && ((k == "k" && count == 1) || (k == "j" && count == 2)) {
                    self.replace(start..at, "", start, true, window, cx);
                    self.set_mode(Mode::Normal, cx);
                    if k == "j" {
                        let text = &self.buffer.read(cx).text;
                        self.vim.cursor =
                            first_nonblank(text, line_at(text, row(text, self.vim.cursor) + 1));
                    } else {
                        let text = &self.buffer.read(cx).text;
                        if self.vim.cursor > vim::start(text, self.vim.cursor) {
                            self.vim.cursor = prev(text, self.vim.cursor);
                        }
                        self.save(window, cx);
                    }
                    self.select(cx);
                    return true;
                }
            }
            if k == "j" {
                let (start, count) = self
                    .insert_j
                    .filter(|(s, n)| at == s + n)
                    .unwrap_or((at, 0));
                self.insert_j = Some((start, count + 1));
                self.chord_timer = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(Duration::from_secs(1)).await;
                    let _ = this.update(cx, |this, _| this.insert_j = None);
                }));
            } else {
                self.insert_j = None;
                self.chord_timer = None;
            }
            return false;
        }
        if self.vim.mode.is_visual() && matches!(k, "d" | "x" | "c" | "y" | ">" | "<") {
            let range = self.input.read(cx).selected_range();
            let op = if k == "x" {
                'd'
            } else {
                k.chars().next().unwrap()
            };
            self.operate(op, range, self.vim.mode == Mode::VisualLine, window, cx);
            return true;
        }
        if matches!(k, "v" | "V") {
            let mode = if k == "v" {
                Mode::Visual
            } else {
                Mode::VisualLine
            };
            if self.vim.mode == mode {
                self.set_mode(Mode::Normal, cx);
            } else {
                if !self.vim.mode.is_visual() {
                    self.vim.anchor = self.vim.cursor;
                }
                self.set_mode(mode, cx);
            }
            self.select(cx);
            return true;
        }
        if k.len() > 1
            && !matches!(
                k,
                "left" | "right" | "up" | "down" | "home" | "end" | "enter"
            )
        {
            return false;
        }
        self.vim.pending.push_str(k);
        let pending = self.vim.pending.clone();
        let (count, command, explicit) = count_prefix(&pending);
        if command.is_empty() || command == "g" || command == " " {
            return true;
        }
        if command == " w" {
            self.vim.pending.clear();
            self.save(window, cx);
            return true;
        }
        let text = self.buffer.read(cx).text.clone();
        let at = self.vim.cursor;
        if let Some(position) = motion(&text, at, command, count, &mut self.vim.column, explicit) {
            self.vim.pending.clear();
            self.vim.cursor = position;
            self.select(cx);
            return true;
        }
        if let Some(operator @ ('d' | 'c' | 'y' | '>' | '<')) = command.chars().next() {
            let tail = &command[1..];
            let (motion_count, motion_key, motion_explicit) = count_prefix(tail);
            if motion_key.is_empty() || motion_key == "g" {
                return true;
            }
            let total = count.saturating_mul(motion_count).min(100_000);
            let repeated = motion_key.len() == 1 && motion_key.starts_with(operator);
            let linewise = repeated || matches!(motion_key, "j" | "k" | "gg" | "G");
            let destination = if repeated {
                Some(line_at(&text, row(&text, at) + total - 1))
            } else {
                motion(
                    &text,
                    at,
                    if operator == 'c'
                        && motion_key == "w"
                        && !text[at..].starts_with(char::is_whitespace)
                    {
                        "e"
                    } else {
                        motion_key
                    },
                    total,
                    &mut self.vim.column,
                    explicit || motion_explicit,
                )
            };
            self.vim.pending.clear();
            if let Some(destination) = destination {
                let range = if linewise {
                    line_range(&text, at, destination)
                } else if motion_key == "w"
                    && (operator != 'c' || text[at..].starts_with(char::is_whitespace))
                {
                    let mut to = word_forward(&text, at, total);
                    if total == 1
                        && !text[at..].starts_with(char::is_whitespace)
                        && to > end(&text, at)
                    {
                        to = end(&text, at);
                    }
                    at..to
                } else if motion_key == "l" {
                    let mut to = at;
                    for _ in 0..total {
                        to = next(&text, to).min(end(&text, at));
                    }
                    at..to
                } else {
                    let inclusive = matches!(motion_key, "e" | "$")
                        || (operator == 'c'
                            && motion_key == "w"
                            && !text[at..].starts_with(char::is_whitespace));
                    let hi = at.max(destination);
                    at.min(destination)..if inclusive {
                        next(&text, hi).min(end(&text, hi))
                    } else {
                        hi
                    }
                };
                self.operate(operator, range, linewise, window, cx);
            }
            return true;
        }
        self.vim.pending.clear();
        match command {
            "i" | "a" | "I" | "A" => {
                self.vim.cursor = match command {
                    "a" => next(&text, at).min(end(&text, at)),
                    "I" => first_nonblank(&text, at),
                    "A" => end(&text, at),
                    _ => at,
                };
                self.set_mode(Mode::Insert, cx);
                self.select(cx);
            }
            "o" | "O" => {
                let indent = &text[start(&text, at)..first_nonblank(&text, at)];
                let (position, replacement, cursor) = if command == "o" {
                    let position = end(&text, at);
                    (position, format!("\n{indent}"), position + 1 + indent.len())
                } else {
                    let position = start(&text, at);
                    (position, format!("{indent}\n"), position + indent.len())
                };
                self.set_mode(Mode::Insert, cx);
                self.replace(position..position, &replacement, cursor, true, window, cx);
            }
            "x" | "D" | "C" => {
                let mut to = at;
                for _ in 0..count {
                    to = next(&text, to).min(end(&text, at));
                }
                if command != "x" {
                    to = end(&text, at);
                }
                self.operate(
                    if command == "C" { 'c' } else { 'd' },
                    at..to,
                    false,
                    window,
                    cx,
                );
            }
            "p" | "P" => {
                if let Some(clipboard) = cx.read_from_clipboard()
                    && let Some(value) = clipboard.text()
                {
                    let linewise = clipboard
                        .metadata()
                        .is_some_and(|m| m == "rockstar-linewise");
                    let mut value = value.repeat(count.min(10_000));
                    let position = if linewise {
                        if command == "P" {
                            start(&text, at)
                        } else {
                            let e = end(&text, at);
                            if e == text.len() && !text.is_empty() {
                                value.insert(0, '\n');
                            }
                            next(&text, e)
                        }
                    } else if command == "P" {
                        at
                    } else {
                        next(&text, at).min(end(&text, at))
                    };
                    let cursor = if linewise {
                        position + usize::from(value.starts_with('\n'))
                    } else {
                        position
                            + value
                                .len()
                                .saturating_sub(value.chars().next_back().map_or(0, char::len_utf8))
                    };
                    self.replace(position..position, &value, cursor, false, window, cx);
                }
            }
            "u" => {
                for _ in 0..count {
                    self.undo(false, window, cx);
                }
            }
            "/" => {
                self.search_origin = at;
                self.search_open = true;
                self.search
                    .update(cx, |search, cx| search.set_value("", window, cx));
                cx.set_global(Mode::Search);
                self.search.focus_handle(cx).focus(window, cx);
            }
            "n" | "N" => {
                for _ in 0..count {
                    self.search_next(command == "n", cx);
                }
            }
            "enter" => {
                self.vim.cursor = first_nonblank(&text, line_at(&text, row(&text, at) + count));
                self.select(cx);
            }
            _ => {}
        }
        true
    }
}

impl Focusable for FileView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Render for FileView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                                let current = state.cursor_position().line as usize;
                                // text_bounds already includes the editor's scroll offset.
                                let top = state.text_bounds().map_or(bounds.top(), |b| b.top());
                                let rows = rows
                                    .map(|line| {
                                        (line, state.display_map().buffer_line_to_display_row(line))
                                    })
                                    .collect::<Vec<_>>();
                                for (line, display_row) in rows {
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
                            .readonly(mode != Mode::Insert)
                            .aria_label("Source code"),
                    )
                    .child(
                        canvas(
                            |_, _, _| {},
                            move |_, _, window, cx| {
                                if mode == Mode::Normal && input.focus_handle(cx).is_focused(window)
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
                                .prefix(div().child("/"))
                                .aria_label("Search source"),
                        ),
                )
            })
            .children(lsp_popup)
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
    }
}

fn map_offset(old: &str, new: &str, offset: usize) -> usize {
    let common = old
        .chars()
        .zip(new.chars())
        .take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf8())
        .sum::<usize>();
    let suffix = old[common..]
        .chars()
        .rev()
        .zip(new[common..].chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf8())
        .sum::<usize>();
    let old_end = old.len() - suffix;
    let new_end = new.len() - suffix;
    boundary(
        new,
        if offset < common {
            offset
        } else if offset >= old_end {
            new_end + offset - old_end
        } else {
            new_end
        },
    )
}

fn language_settings(path: &Path) -> (&'static str, usize) {
    match path.extension().and_then(|s| s.to_str()).unwrap_or("") {
        "rs" => ("rust", 4),
        "py" => ("python", 4),
        "nix" => ("nix", 2),
        "lua" => ("lua", 2),
        "js" => ("javascript", 4),
        "jsx" => ("javascript", 2),
        "ts" | "mts" | "cts" => ("typescript", 4),
        "tsx" => ("tsx", 2),
        "json" => ("json", 4),
        "c" | "h" => ("c", 4),
        "cc" | "cpp" | "cxx" | "c++" | "C" | "hh" | "hpp" | "hxx" | "h++" | "H" | "cu" | "cuh" => {
            ("cpp", 4)
        }
        "tf" | "tfvars" => ("hcl", 2),
        "cs" => ("c_sharp", 4),
        "go" => ("go", 4),
        "hs" | "hsc" | "lhs" | "cabal" => ("haskell", 4),
        "rb" => ("ruby", 2),
        "html" => ("html", 2),
        "css" => ("css", 2),
        "sh" | "bash" => ("bash", 2),
        "toml" => ("toml", 4),
        "yaml" | "yml" => ("yaml", 2),
        "md" => ("markdown", 4),
        "sql" => ("sql", 2),
        "kt" => ("kotlin", 4),
        "java" => ("java", 4),
        "php" => ("php", 4),
        "dart" => ("dart", 2),
        "tex" => ("latex", 2),
        "xaml" | "csproj" => ("xml", 2),
        _ if path.file_name().is_some_and(|n| n == "CMakeLists.txt") => ("cmake", 2),
        _ => ("plain", 4),
    }
}
