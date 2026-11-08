use super::*;
use unicode_segmentation::UnicodeSegmentation;

pub(crate) fn key_input(key: &Keystroke) -> ReplayInput {
    let text = if key.modifiers.control {
        format!("ctrl-{}", key.key)
    } else if key.modifiers.alt {
        format!("alt-{}", key.key)
    } else {
        let text = text_key(key);
        if text == " " {
            "space".into()
        } else {
            text.into()
        }
    };
    ReplayInput::Key(text)
}
pub(crate) fn text_keys(text: &str) -> Vec<ReplayInput> {
    text.chars()
        .map(|c| {
            ReplayInput::Key(match c {
                '\u{1b}' => "escape".into(),
                '\n' | '\r' => "enter".into(),
                '\t' => "tab".into(),
                '\u{08}' | '\u{7f}' => "backspace".into(),
                ' ' => "space".into(),
                c if c.is_ascii_control() => format!("ctrl-{}", char::from(c as u8 + 96)),
                c => c.to_string(),
            })
        })
        .collect()
}
impl FileView {
    pub(super) fn vim_error(
        &mut self,
        error: impl std::fmt::Display,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.shared.update(cx, |shared, cx| {
            shared.macros.stop();
            cx.notify();
        });
        window.push_notification(Notification::error(error.to_string()), cx);
    }
    pub(super) fn execute(
        &mut self,
        command: ParsedCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let changing = matches!(command.action,
            Action::Operate(op, _) if !matches!(op, Operator::Yank | Operator::Fold))
            || matches!(
                command.action,
                Action::Insert(_)
                    | Action::Replace(_)
                    | Action::Paste { .. }
                    | Action::Join(_)
                    | Action::SwapChars
                    | Action::DeleteChars(_)
                    | Action::ChangeLines
                    | Action::LineEnd(Operator::Delete | Operator::Change)
                    | Action::Number(_)
                    | Action::UndoLine
                    | Action::InsertLast
            );
        if changing {
            let text = &self.buffer.read(cx).text;
            let visual = self.vim.mode.is_visual().then(|| {
                let target = visual_target(text, &self.vim, self.indent);
                let a = self.vim.anchor.min(self.vim.cursor);
                let b = self.vim.anchor.max(self.vim.cursor);
                VisualShape {
                    kind: target.kind,
                    rows: row(text, b) - row(text, a) + 1,
                    columns: if target.kind == SelectionKind::Block {
                        self.vim.block_columns.0.abs_diff(self.vim.block_columns.1) + 1
                    } else {
                        column(text, b).saturating_sub(column(text, a)) + 1
                    },
                    end_column: column(text, b),
                }
            });
            self.change = Some(RepeatableChange {
                command: command.clone(),
                visual,
                input: Vec::new(),
            });
            self.change_dirty = false;
        }
        self.execute_inner(command, window, cx);
        if let Some(error) = self.command_error.take() {
            self.vim_error(error, window, cx);
        }
        if changing && !self.vim.mode.is_insert() {
            self.finish_change(cx);
        }
    }
    pub(super) fn finish_change(&mut self, cx: &mut Context<Self>) {
        if let Some(change) = self.change.take().filter(|_| self.change_dirty) {
            self.shared
                .update(cx, |shared, _| shared.last_change = Some(change));
        }
        self.change_dirty = false;
    }
    pub(super) fn record_insert_key(&mut self, key: &Keystroke) {
        if let Some(change) = &mut self.change {
            change.input.push(key_input(key));
        }
    }
    pub(super) fn record_native_input(
        &mut self,
        old: &str,
        new: &str,
        cursor: usize,
        insert: bool,
        cx: &mut Context<Self>,
    ) {
        if old == new {
            return;
        }
        let edit = editing::difference(old, new);
        self.record_text_edit(old, &edit, cursor, insert, cx);
    }
    pub(super) fn record_text_edit(
        &mut self,
        old: &str,
        edit: &TextEdit,
        cursor: usize,
        insert: bool,
        cx: &mut Context<Self>,
    ) {
        let cursor = boundary(old, cursor);
        let mut input = Vec::new();
        let target = cursor.clamp(edit.range.start, edit.range.end);
        let move_key = if cursor > target { "left" } else { "right" };
        let movements = old[cursor.min(target)..cursor.max(target)]
            .graphemes(true)
            .count();
        input.extend((0..movements).map(|_| ReplayInput::Key(move_key.into())));
        input.extend(
            old[edit.range.start..target]
                .graphemes(true)
                .map(|_| ReplayInput::Key("backspace".into())),
        );
        input.extend(
            old[target..edit.range.end]
                .graphemes(true)
                .map(|_| ReplayInput::Key("delete".into())),
        );
        if !edit.replacement.is_empty() {
            input.push(ReplayInput::Text(edit.replacement.clone()));
        }
        if insert && let Some(change) = &mut self.change {
            change.input.extend(input.iter().cloned());
        }
        self.shared.update(cx, |shared, _| {
            if !shared.macros.running
                && let Some((_, recorded)) = &mut shared.macros.recording
            {
                recorded.extend(input);
            }
        });
    }
    pub(super) fn record_macro(&mut self, name: char, window: &mut Window, cx: &mut Context<Self>) {
        if self.shared.read(cx).macros.running {
            return;
        }
        if !(name.is_ascii_alphanumeric() || name == '"') {
            self.vim_error("Invalid recording register", window, cx);
            return;
        }
        self.shared.update(cx, |shared, cx| {
            shared.macros.recording = Some((name, Vec::new()));
            cx.notify();
        });
    }
    pub(super) fn stop_recording(&mut self, cx: &mut Context<Self>) {
        self.shared.update(cx, |shared, cx| {
            if let Some((name, keys)) = shared.macros.recording.take() {
                let key = name.to_ascii_lowercase();
                let value = RegisterValue::Keys(keys);
                if name.is_ascii_uppercase() {
                    shared
                        .registers
                        .entry(key)
                        .and_modify(|old| old.append(value.clone()))
                        .or_insert(value);
                } else {
                    shared.registers.insert(key, value);
                }
                shared.macros.last_recorded = Some(key);
            }
            cx.notify();
        });
    }
    pub(super) fn queue_replay(
        &mut self,
        input: Vec<ReplayInput>,
        count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = self.shared.update(cx, |shared, cx| {
            let result = shared.macros.push(input, count);
            cx.notify();
            result
        });
        if let Err(error) = result {
            self.vim_error(error, window, cx);
        }
    }
    pub(super) fn replay_register(
        &mut self,
        mut name: char,
        count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if name == '@' {
            let state = &self.shared.read(cx).macros;
            let resolved = state.last_executed;
            let Some(resolved) = resolved else {
                self.vim_error("No previous macro", window, cx);
                return;
            };
            name = resolved;
        }
        if matches!(name, '%' | '#' | '=' | ':') {
            self.vim_error("This register cannot be executed here", window, cx);
            return;
        }
        let Some(value) = self.read_register(Some(name), cx) else {
            self.vim_error("Register is empty", window, cx);
            return;
        };
        let keys = match value {
            RegisterValue::Keys(keys) => keys,
            value => text_keys(&value.text()),
        };
        self.shared
            .update(cx, |shared, _| shared.macros.last_executed = Some(name));
        if self.vim.mode == Mode::VisualLine {
            let buffer = self.buffer.read(cx);
            let from = row(&buffer.text, self.vim.anchor.min(self.vim.cursor));
            let to = row(&buffer.text, self.vim.anchor.max(self.vim.cursor));
            if (to - from + 1).saturating_mul(keys.len()) > 100_000 {
                self.vim_error("Visual macro input limit exceeded", window, cx);
                return;
            }
            let locations: Vec<_> = (from..=to)
                .map(|line| BufferLocation {
                    buffer: self.buffer.entity_id(),
                    path: buffer.path.clone(),
                    line,
                    column: 0,
                })
                .collect();
            self.set_mode(Mode::Normal, cx);
            let result = self.shared.update(cx, |shared, cx| {
                for location in locations.into_iter().rev() {
                    shared.macros.push(keys.clone(), count)?;
                    shared.macros.frames.last_mut().unwrap().location = Some(location);
                }
                cx.notify();
                Ok::<_, &'static str>(())
            });
            if let Err(error) = result {
                self.vim_error(error, window, cx);
            }
        } else {
            self.queue_replay(keys, count, window, cx);
        }
    }
    pub(super) fn repeat_change(
        &mut self,
        count: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(mut change) = self.shared.read(cx).last_change.clone() else {
            self.vim_error("No change to repeat", window, cx);
            return;
        };
        if let Some(count) = count {
            change.command.count = count;
            change.command.explicit_count = true;
        }
        if let Some(name @ '1'..='8') = change.command.register {
            change.command.register = char::from_u32(name as u32 + 1);
        }
        if let Some(shape) = &change.visual {
            let text = &self.buffer.read(cx).text;
            let at = self.vim.cursor;
            let line = line_at(text, row(text, at) + shape.rows - 1);
            let mut to = if shape.rows == 1 { at } else { line };
            let columns = if shape.rows == 1 {
                shape.columns - 1
            } else {
                shape.end_column
            };
            for _ in 0..columns {
                to = next(text, to).min(end(text, to));
            }
            self.vim.anchor = at;
            self.vim.cursor = normal_cursor(text, to);
            let col = display_column(text, at, self.indent);
            self.vim.block_columns = (col, col + shape.columns - 1);
            self.set_mode(
                match shape.kind {
                    SelectionKind::Character => Mode::Visual,
                    SelectionKind::Line => Mode::VisualLine,
                    SelectionKind::Block => Mode::VisualBlock,
                },
                cx,
            );
        }
        if !change.input.is_empty() {
            self.queue_replay(change.input, 1, window, cx);
            self.shared
                .update(cx, |shared, _| shared.macros.running = true);
        }
        self.execute(change.command, window, cx);
    }
    pub(crate) fn accepts_replay(&self, window: &Window, cx: &App) -> bool {
        match *cx.global::<Mode>() {
            Mode::Search => self.search_open && self.search.focus_handle(cx).is_focused(window),
            Mode::LspPopup => {
                self.lsp_ui.focus.is_focused(window)
                    || self
                        .lsp_ui
                        .documentation
                        .as_ref()
                        .is_some_and(|panel| panel.content.focus_handle(cx).is_focused(window))
            }
            mode if !mode.is_overlay() => self.input.focus_handle(cx).is_focused(window),
            _ => false,
        }
    }
    pub(crate) fn replay_input(
        &mut self,
        input: ReplayInput,
        dispatched: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match input {
            ReplayInput::Text(text) if self.search_open => {
                let mut value = self.search.read(cx).value().to_string();
                value.push_str(&text);
                self.search
                    .update(cx, |search, cx| search.set_value(value, window, cx));
            }
            ReplayInput::Text(text) if self.vim.mode.is_insert() => {
                if let Some(change) = &mut self.change {
                    change.input.push(ReplayInput::Text(text.clone()));
                }
                self.type_text(&text, window, cx);
            }
            ReplayInput::Text(text) => self.queue_replay(text_keys(&text), 1, window, cx),
            ReplayInput::Key(name) => {
                let Ok(key) = Keystroke::parse_with_os(&name, cx.platform_info().os) else {
                    self.vim_error(format!("Invalid recorded key: {name}"), window, cx);
                    return;
                };
                if !dispatched && self.key(&key, window, cx) {
                    return;
                }
                if self.search_open {
                    let mut value = self.search.read(cx).value().to_string();
                    if name == "backspace" {
                        value.pop();
                    } else if name == "space" {
                        value.push(' ');
                    } else if name.chars().count() == 1 {
                        value.push_str(&name);
                    }
                    self.search
                        .update(cx, |search, cx| search.set_value(value, window, cx));
                } else if self.vim.mode.is_insert() {
                    if !matches!(
                        key.key.as_str(),
                        "left" | "right" | "up" | "down" | "home" | "end" | "pageup" | "pagedown"
                    ) {
                        self.record_insert_key(&key);
                    }
                    let text = self.buffer.read(cx).text.clone();
                    let at = self.vim.cursor;
                    match name.as_str() {
                        "backspace" if at > 0 => {
                            let from = prev(&text, at);
                            self.replace(from..at, "", from, window, cx);
                        }
                        "delete" => self.replace(at..next(&text, at), "", at, window, cx),
                        "left" => {
                            self.vim.cursor = prev(&text, at);
                            self.select(cx);
                        }
                        "right" => {
                            self.vim.cursor = next(&text, at);
                            self.select(cx);
                        }
                        "home" => {
                            self.vim.cursor = start(&text, at);
                            self.select(cx);
                        }
                        "end" => {
                            self.vim.cursor = end(&text, at);
                            self.select(cx);
                        }
                        "up" | "down" => {
                            let line = if name == "up" {
                                row(&text, at).saturating_sub(1)
                            } else {
                                row(&text, at) + 1
                            };
                            self.vim.cursor = at_column(
                                &text,
                                line_at(&text, line),
                                display_column(&text, at, self.indent),
                                self.indent,
                            );
                            self.select(cx);
                        }
                        "enter" => {
                            let indent = &text[start(&text, at)..first_nonblank(&text, at)];
                            self.type_text(&format!("\n{indent}"), window, cx);
                        }
                        "tab" => self.type_text(
                            &" ".repeat(
                                self.indent - display_column(&text, at, self.indent) % self.indent,
                            ),
                            window,
                            cx,
                        ),
                        "space" => self.type_text(" ", window, cx),
                        _ if name.chars().count() == 1 => self.type_text(&name, window, cx),
                        _ => self.vim_error(format!("Cannot replay {name}"), window, cx),
                    }
                }
            }
        }
    }
}
