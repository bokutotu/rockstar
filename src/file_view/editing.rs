use super::*;
use unicode_segmentation::UnicodeSegmentation;

pub(super) struct BlockInsert {
    pub first_row: usize,
    pub rows: usize,
    pub column: usize,
}
pub(super) struct InsertSession {
    pub count: usize,
    pub start: usize,
    pub open_line: bool,
    pub auto_indent: bool,
    pub setup_undo: Option<EditTransaction>,
    pub block: Option<BlockInsert>,
    pub replace_undo: Vec<EditTransaction>,
    pub register_pending: bool,
    pub literal_next: bool,
    pub temporary_normal: bool,
    pub control_g: bool,
    pub restart_at_cursor: bool,
}
impl InsertSession {
    pub(super) fn new(start: usize, count: usize) -> Self {
        Self {
            start,
            count,
            open_line: false,
            auto_indent: false,
            setup_undo: None,
            block: None,
            replace_undo: Vec::new(),
            register_pending: false,
            literal_next: false,
            temporary_normal: false,
            control_g: false,
            restart_at_cursor: false,
        }
    }
}

pub(super) fn difference(old: &str, new: &str) -> TextEdit {
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
    TextEdit {
        range: common..old.len() - suffix,
        replacement: new[common..new.len() - suffix].into(),
    }
}

impl FileView {
    pub(super) fn commit_transaction(
        &mut self,
        transaction: EditTransaction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let old = self.buffer.read(cx).text.clone();
        let Some(text) = transaction.apply(&old) else {
            return;
        };
        self.vim.cursor = if self.vim.mode.is_insert() {
            boundary(&text, transaction.cursor_after)
        } else {
            normal_cursor(&text, transaction.cursor_after)
        };
        let id = cx.entity_id();
        let insert = self.vim.mode.is_insert();
        let replay = self
            .shared
            .read(cx)
            .macros
            .running
            .then_some(self.shared.read(cx).macros.generation);
        if old != text {
            self.change_dirty = true;
            self.change_index = None;
            let buffer = self.buffer.entity_id();
            self.jumps
                .update(cx, |jumps, _| jumps.remap(buffer, &old, &text));
        }
        // Buffer first: the editor's queued Change notification then sees equal
        // text and cannot register this edit a second time.
        self.buffer.update(cx, |buffer, cx| {
            buffer.edit(
                text.clone(),
                transaction.cursor_before,
                transaction.cursor_after,
                id,
                insert,
                replay,
                cx,
            )
        });
        if self.input.read(cx).value().as_ref() != text {
            self.set_input(&text, self.vim.cursor..self.vim.cursor, true, window, cx);
        }
        self.remap_folds(&old, &text, cx);
        self.vim.column = None;
        self.select(cx);
    }
    pub(super) fn replace(
        &mut self,
        range: Range<usize>,
        replacement: &str,
        cursor: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.commit_transaction(
            EditTransaction::replace(range, replacement.into(), self.vim.cursor, cursor),
            window,
            cx,
        );
    }
    pub(super) fn input_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.input.read(cx).is_composing() {
            return;
        }
        let entered = self.input.read(cx).value().to_string();
        let old = self.buffer.read(cx).text.clone();
        if old == entered {
            return;
        }
        let native_cursor = self.input.read(cx).cursor();
        if self.vim.mode.is_insert() && self.insert.is_none() {
            self.insert = Some(InsertSession::new(self.vim.cursor.min(old.len()), 1));
        }
        let mut edit = difference(&old, &entered);
        // A minimal textual diff can slide through repeated characters. Use
        // the native insertion endpoint to recover the actual typed range.
        if entered.len() > old.len() {
            let inserted = entered.len() - old.len();
            if let Some(start) = native_cursor.checked_sub(inserted) {
                if old.is_char_boundary(start)
                    && entered.is_char_boundary(start)
                    && entered.is_char_boundary(native_cursor)
                    && old[..start] == entered[..start]
                    && old[start..] == entered[native_cursor..]
                {
                    edit = TextEdit {
                        range: start..start,
                        replacement: entered[start..native_cursor].into(),
                    };
                }
            }
        }
        if old.len() > entered.len() {
            let removed = old.len() - entered.len();
            let from = native_cursor;
            let to = from.saturating_add(removed);
            if old.is_char_boundary(from)
                && old.is_char_boundary(to)
                && entered.is_char_boundary(from)
                && old[..from] == entered[..from]
                && old[to..] == entered[from..]
            {
                edit = TextEdit {
                    range: from..to,
                    replacement: String::new(),
                };
            }
        }
        let before = self.vim.cursor.min(old.len());
        if let Some(session) = &mut self.insert {
            if session.restart_at_cursor {
                session.start = before;
                session.restart_at_cursor = false;
            }
        }
        self.record_text_edit(&old, &edit, before, true, cx);
        let transaction = if matches!(self.vim.mode, Mode::Replace | Mode::VirtualReplace)
            && edit.range.is_empty()
            && !edit.replacement.is_empty()
        {
            plan_input(
                &old,
                edit.range.start,
                &edit.replacement,
                self.vim.mode,
                self.indent,
            )
        } else {
            EditTransaction {
                edits: vec![edit],
                cursor_before: before,
                cursor_after: native_cursor,
            }
        };
        if matches!(self.vim.mode, Mode::Replace | Mode::VirtualReplace) {
            if let Some(session) = &mut self.insert {
                if let Some(edit) = transaction.edits.first() {
                    session.replace_undo.push(EditTransaction::replace(
                        edit.range.start..edit.range.start + edit.replacement.len(),
                        old[edit.range.clone()].into(),
                        transaction.cursor_after,
                        edit.range.start,
                    ));
                }
            }
        }
        self.commit_transaction(transaction, window, cx);
    }
    pub(super) fn read_register(
        &self,
        register: Option<char>,
        cx: &mut App,
    ) -> Option<RegisterValue> {
        let register = register.unwrap_or('"');
        if matches!(register, '+' | '*') {
            #[cfg(target_os = "linux")]
            let item = if register == '*' {
                cx.read_from_primary()
            } else {
                cx.read_from_clipboard()
            };
            #[cfg(not(target_os = "linux"))]
            let item = cx.read_from_clipboard();
            let item = item?;
            let text = item.text()?;
            return Some(match item.metadata().map(String::as_str) {
                Some("rockstar-characterwise") => RegisterValue::Character(text),
                Some("rockstar-linewise") => RegisterValue::Line(if text.ends_with('\n') {
                    text
                } else {
                    format!("{text}\n")
                }),
                Some(metadata) if metadata.starts_with("rockstar-blockwise:") => {
                    RegisterValue::Block {
                        rows: text.split('\n').map(str::to_owned).collect(),
                        width: metadata[19..].parse().ok()?,
                    }
                }
                _ => {
                    if text.ends_with('\n') {
                        RegisterValue::Line(text)
                    } else {
                        RegisterValue::Character(text)
                    }
                }
            });
        }
        if register == '%' {
            return Some(RegisterValue::Character(
                self.buffer.read(cx).path.display().to_string(),
            ));
        }
        if register == '.' {
            return Some(RegisterValue::Character(
                self.shared.read(cx).last_insert.clone(),
            ));
        }
        if register == '/' {
            return self
                .shared
                .read(cx)
                .search
                .as_ref()
                .map(|s| RegisterValue::Character(s.pattern.clone()));
        }
        if register == '_' {
            return Some(RegisterValue::Character(String::new()));
        }
        self.shared
            .read(cx)
            .registers
            .get(&register.to_ascii_lowercase())
            .cloned()
    }
    fn store_target(
        &mut self,
        target: &ResolvedTarget,
        register: Option<char>,
        yank: bool,
        cx: &mut Context<Self>,
    ) {
        let text = &self.buffer.read(cx).text;
        let value = capture_target(text, target, self.indent);
        let large = target.kind == SelectionKind::Line
            || target.ranges.iter().any(|r| text[r.clone()].contains('\n'));
        if matches!(register, Some('+' | '*')) {
            let metadata = match &value {
                RegisterValue::Line(_) => "rockstar-linewise".into(),
                RegisterValue::Block { width, .. } => format!("rockstar-blockwise:{width}"),
                _ => "rockstar-characterwise".into(),
            };
            let item = ClipboardItem::new_string_with_metadata(value.text(), metadata);
            #[cfg(target_os = "linux")]
            if register == Some('*') {
                cx.write_to_primary(item);
            } else {
                cx.write_to_clipboard(item);
            }
            #[cfg(not(target_os = "linux"))]
            cx.write_to_clipboard(item);
        }
        self.shared
            .update(cx, |state, _| state.store(register, value, yank, large));
    }
    pub(super) fn operate(
        &mut self,
        operator: Operator,
        target: ResolvedTarget,
        register: Option<char>,
        shifts: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.integration_operator(operator, &target, window, cx) {
            return;
        }
        if target.ranges.iter().all(Range::is_empty) && operator != Operator::Change {
            return;
        }
        let before = self.vim.cursor;
        let was_visual = self.vim.mode.is_visual();
        let text = self.buffer.read(cx).text.clone();
        let transaction = plan_operator(&text, &self.vim, operator, &target, self.indent, shifts);
        if matches!(
            operator,
            Operator::Delete | Operator::Change | Operator::Yank
        ) {
            self.store_target(&target, register, operator == Operator::Yank, cx);
        }
        self.remember_visual();
        if operator == Operator::Yank {
            self.vim.cursor = target.ranges.first().map_or(before, |r| {
                if was_visual {
                    r.start
                } else {
                    before.min(r.start)
                }
            });
            self.set_mode(Mode::Normal, cx);
            self.select(cx);
            return;
        }
        self.set_mode(
            if operator == Operator::Change {
                Mode::Insert
            } else {
                Mode::Normal
            },
            cx,
        );
        if let Some(transaction) = transaction {
            self.commit_transaction(transaction, window, cx);
        }
        if operator == Operator::Change {
            let mut setup_undo = None;
            let block = if target.kind == SelectionKind::Block {
                let column = target.block_columns.as_ref().unwrap().start;
                let current =
                    display_column(&self.buffer.read(cx).text, self.vim.cursor, self.indent);
                if current < column {
                    let at = self.vim.cursor;
                    setup_undo = Some(EditTransaction::replace(
                        at..at + column - current,
                        String::new(),
                        at + column - current,
                        at,
                    ));
                    self.replace(
                        at..at,
                        &" ".repeat(column - current),
                        at + column - current,
                        window,
                        cx,
                    );
                }
                Some(BlockInsert {
                    first_row: row(&text, target.ranges[0].start),
                    rows: target.ranges.len(),
                    column,
                })
            } else {
                None
            };
            let mut session = InsertSession::new(self.vim.cursor, 1);
            session.block = block;
            session.auto_indent = target.kind == SelectionKind::Line;
            session.setup_undo = setup_undo;
            self.insert = Some(session);
        }
    }
    pub(super) fn paste(
        &mut self,
        before: bool,
        after: bool,
        register: Option<char>,
        count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(value) = self.read_register(register, cx) else {
            return;
        };
        if value.text().is_empty() {
            return;
        }
        let text = self.buffer.read(cx).text.clone();
        let transaction = if self.vim.mode.is_visual() {
            let target = visual_target(&text, &self.vim, self.indent);
            let Some(transaction) =
                plan_visual_paste(&text, &self.vim, &target, &value, count, self.indent)
            else {
                return;
            };
            // Neovim's Visual P leaves the registers intact.
            if !before {
                self.store_target(&target, None, false, cx);
            }
            self.remember_visual();
            self.set_mode(Mode::Normal, cx);
            transaction
        } else {
            plan_paste(
                &text,
                self.vim.cursor,
                &value,
                before,
                after,
                count,
                self.indent,
            )
        };
        self.commit_transaction(transaction, window, cx);
    }
    pub(super) fn begin_insert(
        &mut self,
        placement: &str,
        count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reveal_folds(self.vim.cursor, true, cx);
        let text = self.buffer.read(cx).text.clone();
        let at = self.vim.cursor;
        let block = if self.vim.mode == Mode::VisualBlock && matches!(placement, "I" | "A") {
            let target = visual_target(&text, &self.vim, self.indent);
            let cols = target.block_columns.unwrap();
            Some(BlockInsert {
                first_row: row(&text, target.ranges[0].start),
                rows: target.ranges.len(),
                column: if placement == "I" {
                    cols.start
                } else {
                    cols.end
                },
            })
        } else {
            None
        };
        let mode = match placement {
            "R" => Mode::Replace,
            "gR" => Mode::VirtualReplace,
            _ => Mode::Insert,
        };
        self.remember_visual();
        self.set_mode(mode, cx);
        self.vim.cursor = match placement {
            "a" => next(&text, at).min(end(&text, at)),
            "I" => first_nonblank(&text, at),
            "A" => end(&text, at),
            "gI" => start(&text, at),
            _ => at,
        };
        let mut setup_undo = None;
        if let Some(block) = &block {
            let s = line_at(&text, block.first_row);
            let pos = at_column(&text, s, block.column, self.indent);
            let col = display_column(&text, pos, self.indent);
            self.vim.cursor = pos;
            if col < block.column {
                let width = if pos < end(&text, s) {
                    cell_width(&text[pos..next(&text, pos)], col, self.indent)
                } else {
                    block.column - col
                };
                let to = if pos < end(&text, s) {
                    next(&text, pos)
                } else {
                    pos
                };
                setup_undo = Some(EditTransaction::replace(
                    pos..pos + width,
                    text[pos..to].into(),
                    pos + block.column - col,
                    pos,
                ));
                self.replace(
                    pos..to,
                    &" ".repeat(width),
                    pos + block.column - col,
                    window,
                    cx,
                );
            }
        } else if matches!(placement, "o" | "O") {
            let indent = &text[start(&text, at)..first_nonblank(&text, at)];
            let (pos, replacement, cursor) = if placement == "o" {
                let pos = end(&text, at);
                (pos, format!("\n{indent}"), pos + 1 + indent.len())
            } else {
                let pos = start(&text, at);
                (pos, format!("{indent}\n"), pos + indent.len())
            };
            self.replace(pos..pos, &replacement, cursor, window, cx);
        }
        let mut session = InsertSession::new(self.vim.cursor, count);
        session.block = block;
        session.open_line = matches!(placement, "o" | "O");
        session.auto_indent = session.open_line;
        session.setup_undo = setup_undo;
        self.insert = Some(session);
        self.select(cx);
    }
    pub(super) fn finish_insert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(mut session) = self.insert.take() {
            if self.vim.cursor == session.start {
                if let Some(undo) = session.setup_undo.take() {
                    self.commit_transaction(undo, window, cx);
                    session.start = self.vim.cursor;
                }
            }
            let text = self.buffer.read(cx).text.clone();
            if session.auto_indent && self.vim.cursor == session.start {
                let range = start(&text, session.start)..end(&text, session.start);
                if !range.is_empty() && text[range.clone()].trim().is_empty() {
                    session.start = range.start;
                    self.replace(range, "", session.start, window, cx);
                }
            }
            let text = self.buffer.read(cx).text.clone();
            if session.start <= self.vim.cursor
                && text.is_char_boundary(session.start)
                && self.vim.cursor <= text.len()
            {
                let mut inserted = text[session.start..self.vim.cursor].to_string();
                if session.count > 1 && !inserted.is_empty() {
                    let repeat = if session.open_line {
                        let indent = &text[start(&text, session.start)..session.start];
                        format!("\n{indent}{inserted}").repeat(session.count.min(10_000) - 1)
                    } else {
                        inserted.repeat(session.count.min(10_000) - 1)
                    };
                    let transaction =
                        plan_input(&text, self.vim.cursor, &repeat, self.vim.mode, self.indent);
                    self.commit_transaction(transaction, window, cx);
                    if !session.open_line {
                        inserted.push_str(&repeat);
                    }
                }
                self.shared
                    .update(cx, |shared, _| shared.last_insert = inserted.clone());
                if let Some(block) = session.block.filter(|block| {
                    block.rows > 1 && !inserted.contains('\n') && !inserted.is_empty()
                }) {
                    let text = self.buffer.read(cx).text.clone();
                    let pos = line_at(&text, block.first_row + 1);
                    let rows = vec![inserted.clone(); block.rows - 1];
                    let width = inserted.graphemes(true).fold(0, |col, g| {
                        col + cell_width(g, block.column + col, self.indent)
                    });
                    let paste = plan_block_paste(
                        &text,
                        pos,
                        &rows,
                        width,
                        block.column,
                        false,
                        1,
                        self.indent,
                    );
                    if let Some(new) = paste.apply(&text) {
                        let transaction = EditTransaction::replace(
                            0..text.len(),
                            new,
                            self.vim.cursor,
                            self.vim.cursor,
                        );
                        self.commit_transaction(transaction, window, cx);
                    }
                }
            }
        }
        let at = self.vim.cursor;
        self.buffer.update(cx, |buffer, _| {
            buffer.positions.marks.insert('^', at);
        });
        let text = &self.buffer.read(cx).text;
        if self.vim.cursor > start(text, self.vim.cursor) {
            self.vim.cursor = prev(text, self.vim.cursor);
        }
        let id = cx.entity_id();
        self.buffer
            .update(cx, |buffer, _| buffer.finish_insert(id, self.vim.cursor));
        self.set_mode(Mode::Normal, cx);
        self.select(cx);
        self.finish_change(cx);
    }
    pub(super) fn type_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let old = self.buffer.read(cx).text.clone();
        let transaction = plan_input(&old, self.vim.cursor, text, self.vim.mode, self.indent);
        if matches!(self.vim.mode, Mode::Replace | Mode::VirtualReplace) {
            if let Some(session) = &mut self.insert {
                let edit = &transaction.edits[0];
                session.replace_undo.push(EditTransaction::replace(
                    edit.range.start..edit.range.start + edit.replacement.len(),
                    old[edit.range.clone()].into(),
                    transaction.cursor_after,
                    edit.range.start,
                ));
            }
        }
        self.commit_transaction(transaction, window, cx);
    }
}
