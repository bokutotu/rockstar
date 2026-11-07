use super::*;

impl FileView {
    pub(crate) fn key(
        &mut self,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.lsp_ui.documentation.is_some() {
            return self.documentation_key(key, window, cx);
        }
        if self.input.read(cx).is_composing() {
            return false;
        }
        // A pending character is data, even when it is K, Space, a colon, etc.
        if self.vim.mode.is_insert() && self.insert.as_ref().is_some_and(|s| s.literal_next) {
            return self.insert_key(key, window, cx);
        }
        if self.vim.pending.is_empty()
            && !self.insert.as_ref().is_some_and(|s| s.register_pending)
            && self.lsp_key(key, window, cx)
        {
            return true;
        }
        let escape =
            key.key == "escape" || (key.modifiers.control && matches!(key.key.as_str(), "[" | "c"));
        if escape {
            if self.search_open {
                self.end_search(false, window, cx);
            } else if self.vim.mode.is_insert() {
                if key.modifiers.control && key.key == "c" {
                    if let Some(session) = &mut self.insert {
                        session.count = 1;
                    }
                }
                self.finish_insert(window, cx);
            } else {
                self.insert = None;
                if self.vim.mode == Mode::Normal {
                    self.input.update(cx, |input, cx| input.close_search(cx));
                }
                self.set_mode(Mode::Normal, cx);
                self.select(cx);
            }
            return true;
        }
        if self.search_open {
            return false;
        }
        if key.modifiers.alt || key.modifiers.platform {
            return false;
        }
        if self.vim.mode.is_insert() {
            return self.insert_key(key, window, cx);
        }
        let control;
        let k = if key.modifiers.control {
            match key.key.as_str() {
                "h" => "backspace",
                "j" => "j",
                "m" => "enter",
                "n" | "p" => return false, // User's existing bindings remain in force.
                _ => {
                    control = format!("ctrl-{}", key.key);
                    &control
                }
            }
        } else {
            text_key(key)
        };
        let Some(command) = self.vim.pending.feed(k, self.vim.mode.is_visual()) else {
            cx.notify();
            return true;
        };
        self.execute(command, window, cx);
        if self.insert.as_ref().is_some_and(|s| s.temporary_normal) && self.vim.mode == Mode::Normal
        {
            if let Some(session) = &mut self.insert {
                session.temporary_normal = false;
            }
            self.set_mode(Mode::Insert, cx);
            self.select(cx);
        }
        true
    }

    fn insert_key(&mut self, key: &Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.insert.is_none() {
            self.insert = Some(InsertSession::new(self.vim.cursor, 1));
        }
        let k = text_key(key);
        if self.insert.as_ref().is_some_and(|s| s.register_pending) {
            if key.modifiers.control && matches!(key.key.as_str(), "r" | "o" | "p") {
                return true;
            }
            if let Some(session) = &mut self.insert {
                session.register_pending = false;
            }
            if let Some(register) = k.chars().next().filter(|_| k.chars().count() == 1) {
                if let Some(value) = self.read_register(Some(register), cx) {
                    self.type_text(&value.text(), window, cx);
                }
            }
            return true;
        }
        if self.insert.as_ref().is_some_and(|s| s.literal_next) {
            if let Some(session) = &mut self.insert {
                session.literal_next = false;
            }
            let literal = if key.modifiers.control {
                key.key
                    .chars()
                    .next()
                    .filter(|c| c.is_ascii())
                    .map(|c| char::from(c.to_ascii_uppercase() as u8 & 0x1f).to_string())
            } else {
                match key.key.as_str() {
                    "enter" => Some("\n".into()),
                    "tab" => Some("\t".into()),
                    _ if k.chars().count() == 1 => Some(k.into()),
                    _ => None,
                }
            };
            if let Some(text) = literal {
                self.type_text(&text, window, cx);
            }
            return true;
        }
        if key.modifiers.control {
            let text = self.buffer.read(cx).text.clone();
            let at = self.vim.cursor;
            match key.key.as_str() {
                "n" | "p" => return false,
                "r" => {
                    if let Some(session) = &mut self.insert {
                        session.register_pending = true;
                    }
                }
                "v" | "q" => {
                    if let Some(session) = &mut self.insert {
                        session.literal_next = true;
                    }
                }
                "h" => {
                    if self.restore_replaced(window, cx) {
                        return true;
                    }
                    if at > 0 {
                        let from = prev(&text, at);
                        self.replace(from..at, "", from, window, cx);
                    }
                }
                "j" | "m" => self.type_text("\n", window, cx),
                "w" => {
                    let from = word_backward(&text, at, 1, false).max(start(&text, at));
                    self.replace(from..at, "", from, window, cx);
                }
                "u" => {
                    let from = self
                        .insert
                        .as_ref()
                        .map_or(start(&text, at), |s| s.start.max(start(&text, at)).min(at));
                    self.replace(from..at, "", from, window, cx);
                }
                "t" | "d" => {
                    let target = ResolvedTarget::line(line_range(&text, at, at));
                    let op = if key.key == "t" {
                        Operator::Indent
                    } else {
                        Operator::Outdent
                    };
                    if let Some(mut tx) =
                        plan_operator(&text, &self.vim, op, &target, self.indent, 1)
                    {
                        if let Some(edit) = tx.edits.first() {
                            tx.cursor_after = if at >= edit.range.end {
                                at + edit.replacement.len() - edit.range.len()
                            } else {
                                edit.range.start + edit.replacement.len()
                            };
                            if let Some(session) = &mut self.insert {
                                session.start = session.start.min(tx.cursor_after);
                                session.count = 1;
                            }
                        }
                        self.commit_transaction(tx, window, cx);
                    }
                }
                "e" | "y" => {
                    let current = row(&text, at);
                    let wanted = if key.key == "e" {
                        current + 1
                    } else {
                        current.saturating_sub(1)
                    };
                    if wanted != current {
                        let line = line_at(&text, wanted);
                        let pos = at_column(
                            &text,
                            line,
                            display_column(&text, at, self.indent),
                            self.indent,
                        );
                        if row(&text, line) == wanted && pos < end(&text, line) {
                            self.type_text(&text[pos..next(&text, pos)], window, cx);
                        }
                    }
                }
                "o" => {
                    if let Some(session) = &mut self.insert {
                        session.temporary_normal = true;
                        session.count = 1;
                    }
                    self.vim.cursor = normal_cursor(&text, at);
                    self.set_mode(Mode::Normal, cx);
                    self.select(cx);
                }
                "left" | "right" | "home" | "end" => {
                    self.break_insert(cx);
                    return false;
                }
                _ => {} // Do not fall through to Ctrl-a/select-all, Ctrl-x/cut, etc.
            }
            return true;
        }
        if key.key == "backspace" && self.restore_replaced(window, cx) {
            return true;
        }
        if key.key == "insert" {
            let mode = if self.vim.mode == Mode::Insert {
                Mode::Replace
            } else {
                Mode::Insert
            };
            self.set_mode(mode, cx);
            return true;
        }
        if matches!(
            key.key.as_str(),
            "left" | "right" | "up" | "down" | "home" | "end" | "pageup" | "pagedown"
        ) {
            self.break_insert(cx);
        }
        false
    }
    fn break_insert(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = &mut self.insert {
            session.count = 1;
            session.block = None;
            session.replace_undo.clear();
        }
        self.buffer.update(cx, |buffer, _| buffer.end_insert());
    }
    fn restore_replaced(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !matches!(self.vim.mode, Mode::Replace | Mode::VirtualReplace) {
            return false;
        }
        let tx = self.insert.as_mut().and_then(|session| {
            if session
                .replace_undo
                .last()
                .is_some_and(|tx| tx.cursor_before == self.vim.cursor)
            {
                session.replace_undo.pop()
            } else {
                None
            }
        });
        if let Some(tx) = tx {
            self.commit_transaction(tx, window, cx);
            true
        } else {
            false
        }
    }
    fn resolve(
        &mut self,
        target: &Target,
        command: &ParsedCommand,
        operator: Option<Operator>,
        cx: &mut Context<Self>,
    ) -> Option<ResolvedTarget> {
        if let Target::Motion(Motion::Find(find)) = target {
            self.shared
                .update(cx, |shared, _| shared.last_find = Some(*find));
        }
        if self.vim.mode == Mode::VisualBlock {
            self.vim.column = Some(self.vim.block_columns.1);
        }
        resolve_target(
            &self.buffer.read(cx).text,
            &self.vim,
            self.shared.read(cx),
            target,
            command.count,
            command.explicit_count,
            operator,
            command.forced,
            &self.geometry(cx),
        )
    }
    fn set_selection(&mut self, target: ResolvedTarget, source: &Target, cx: &mut Context<Self>) {
        let text = &self.buffer.read(cx).text;
        let old_column = display_column(text, self.vim.cursor, self.indent);
        if matches!(source, Target::TextObject { .. }) && self.vim.mode.is_visual() {
            let Some(first) = target.ranges.first() else {
                return;
            };
            let last = target.ranges.last().unwrap();
            self.vim.anchor = first.start;
            self.vim.cursor = if last.is_empty() {
                last.end
            } else {
                prev(text, last.end)
            };
            if target.kind == SelectionKind::Line {
                self.set_mode(Mode::VisualLine, cx);
            } else {
                self.set_mode(Mode::Visual, cx);
            }
            self.vim.column = None;
        } else {
            let vertical = matches!(source, Target::Motion(Motion::Key(key)) if matches!(key.as_str(), "j" | "k" | "up" | "down" | "gj" | "gk"));
            let line_end = matches!(source, Target::Motion(Motion::Key(key)) if matches!(key.as_str(), "$" | "end"));
            if vertical {
                self.vim.column.get_or_insert(old_column);
            } else {
                self.vim.column = if line_end { Some(usize::MAX) } else { None };
            }
            if self.vim.mode == Mode::VisualBlock && !vertical {
                self.vim.block_columns.1 = display_column(text, target.destination, self.indent);
            }
            self.vim.cursor = target.destination;
        }
        self.select(cx);
    }

    fn execute(&mut self, command: ParsedCommand, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.buffer.read(cx).text.clone();
        let at = self.vim.cursor;
        match command.action.clone() {
            Action::Target(target) => {
                if let Some(resolved) = self.resolve(&target, &command, None, cx) {
                    self.set_selection(resolved, &target, cx);
                }
            }
            Action::Operate(op, target) => {
                let resolved = if let Some(target) = target {
                    self.resolve(&target, &command, Some(op), cx)
                } else {
                    Some(visual_target(&text, &self.vim, self.indent))
                };
                if let Some(resolved) = resolved {
                    let shifts = if self.vim.mode.is_visual() {
                        command.count
                    } else {
                        1
                    };
                    self.operate(op, resolved, command.register, shifts, window, cx);
                }
            }
            Action::Insert(placement) => self.begin_insert(placement, command.count, window, cx),
            Action::Visual(kind) => {
                let mode = match kind {
                    SelectionKind::Character => Mode::Visual,
                    SelectionKind::Line => Mode::VisualLine,
                    SelectionKind::Block => Mode::VisualBlock,
                };
                if mode == self.vim.mode {
                    self.set_mode(Mode::Normal, cx);
                } else {
                    if !self.vim.mode.is_visual() {
                        self.vim.anchor = at;
                    }
                    self.vim.block_columns = (
                        display_column(&text, self.vim.anchor, self.indent),
                        display_column(&text, at, self.indent),
                    );
                    self.set_mode(mode, cx);
                }
                self.select(cx);
            }
            Action::ExchangeEnds(horizontal) => {
                if self.vim.mode == Mode::VisualBlock {
                    std::mem::swap(&mut self.vim.block_columns.0, &mut self.vim.block_columns.1);
                    if horizontal {
                        self.vim.anchor = at_column(
                            &text,
                            start(&text, self.vim.anchor),
                            self.vim.block_columns.0,
                            self.indent,
                        );
                        self.vim.cursor = at_column(
                            &text,
                            start(&text, at),
                            self.vim.block_columns.1,
                            self.indent,
                        );
                    } else {
                        std::mem::swap(&mut self.vim.anchor, &mut self.vim.cursor);
                    }
                } else {
                    std::mem::swap(&mut self.vim.anchor, &mut self.vim.cursor);
                }
                self.select(cx);
            }
            Action::Reselect => {
                if let Some(selection) = self.vim.last_visual.clone() {
                    self.remember_visual();
                    self.vim.anchor = normal_cursor(&text, selection.anchor);
                    self.vim.cursor = normal_cursor(&text, selection.cursor);
                    self.vim.block_columns = selection.columns;
                    self.set_mode(
                        match selection.kind {
                            SelectionKind::Character => Mode::Visual,
                            SelectionKind::Line => Mode::VisualLine,
                            SelectionKind::Block => Mode::VisualBlock,
                        },
                        cx,
                    );
                    self.select(cx);
                }
            }
            Action::Replace(c) => {
                let target = if self.vim.mode.is_visual() {
                    visual_target(&text, &self.vim, self.indent)
                } else {
                    let mut to = at;
                    for _ in 0..command.count {
                        if to >= end(&text, at) {
                            return;
                        }
                        to = next(&text, to);
                    }
                    ResolvedTarget::character(at..to)
                };
                let edits = target
                    .ranges
                    .iter()
                    .map(|range| {
                        let replacement = if c == '\n' {
                            "\n".into()
                        } else {
                            let mut s = String::new();
                            let mut i = range.start;
                            while i < range.end {
                                if text[i..].starts_with('\n') {
                                    s.push('\n');
                                } else {
                                    s.push(c);
                                }
                                i = next(&text, i);
                            }
                            s
                        };
                        TextEdit {
                            range: range.clone(),
                            replacement,
                        }
                    })
                    .collect();
                let cursor = if c == '\n' {
                    target.ranges[0].start + 1
                } else if self.vim.mode.is_visual() {
                    target.ranges[0].start
                } else {
                    at + c.len_utf8() * (command.count - 1)
                };
                self.set_mode(Mode::Normal, cx);
                self.commit_transaction(
                    EditTransaction {
                        edits,
                        cursor_before: at,
                        cursor_after: cursor,
                    },
                    window,
                    cx,
                );
            }
            Action::Paste { before, after } => {
                self.paste(before, after, command.register, command.count, window, cx)
            }
            Action::DeleteChars(backward) => {
                let target = if self.vim.mode.is_visual() {
                    ResolvedTarget::line(line_range(&text, self.vim.anchor, at))
                } else {
                    let mut to = at;
                    for _ in 0..command.count {
                        to = if backward {
                            prev(&text, to).max(start(&text, at))
                        } else {
                            next(&text, to).min(end(&text, at))
                        };
                    }
                    ResolvedTarget::character(to.min(at)..to.max(at))
                };
                self.operate(Operator::Delete, target, command.register, 1, window, cx);
            }
            Action::LineEnd(op) => {
                let target = if self.vim.mode.is_visual() {
                    Some(ResolvedTarget::line(line_range(&text, self.vim.anchor, at)))
                } else {
                    self.resolve(
                        &Target::Motion(Motion::Key("$".into())),
                        &command,
                        Some(op),
                        cx,
                    )
                };
                if let Some(target) = target {
                    self.operate(op, target, command.register, 1, window, cx);
                }
            }
            Action::ChangeLines => {
                let target = if self.vim.mode.is_visual() {
                    ResolvedTarget::line(line_range(&text, self.vim.anchor, at))
                } else {
                    ResolvedTarget::line(line_range(
                        &text,
                        at,
                        line_at(&text, row(&text, at) + command.count - 1),
                    ))
                };
                self.operate(Operator::Change, target, command.register, 1, window, cx);
            }
            Action::SwapChars => {
                let mut to = at;
                for _ in 0..command.count {
                    to = next(&text, to).min(end(&text, at));
                }
                if let Some(mut tx) = plan_operator(
                    &text,
                    &self.vim,
                    Operator::Swap,
                    &ResolvedTarget::character(at..to),
                    self.indent,
                    1,
                ) {
                    tx.cursor_after =
                        at + tx.edits.iter().map(|e| e.replacement.len()).sum::<usize>();
                    self.commit_transaction(tx, window, cx);
                }
            }
            Action::Join(space) => {
                let (first, count) = if self.vim.mode.is_visual() {
                    (
                        start(&text, at.min(self.vim.anchor)),
                        row(&text, at.max(self.vim.anchor)) - row(&text, at.min(self.vim.anchor))
                            + 1,
                    )
                } else {
                    (start(&text, at), command.count.max(2))
                };
                if let Some(tx) = plan_join(&text, at, first, count, space) {
                    self.set_mode(Mode::Normal, cx);
                    self.commit_transaction(tx, window, cx);
                }
            }
            Action::Number(increase) => {
                if self.vim.mode.is_visual() {
                    let target = visual_target(&text, &self.vim, self.indent);
                    let mut edits = Vec::new();
                    for range in target.ranges {
                        for line in row(&text, range.start)..=row(&text, prev(&text, range.end)) {
                            if let Some(tx) = plan_number(
                                &text,
                                line_at(&text, line).max(range.start),
                                increase,
                                command.count,
                            ) {
                                edits.extend(tx.edits.into_iter().filter(|e| {
                                    e.range.start >= range.start && e.range.end <= range.end
                                }));
                            }
                        }
                    }
                    self.set_mode(Mode::Normal, cx);
                    self.commit_transaction(
                        EditTransaction {
                            edits,
                            cursor_before: at,
                            cursor_after: at,
                        },
                        window,
                        cx,
                    );
                } else if let Some(tx) = plan_number(&text, at, increase, command.count) {
                    self.commit_transaction(tx, window, cx);
                }
            }
            Action::Undo | Action::Redo => {
                for _ in 0..command.count {
                    self.undo(matches!(command.action, Action::Redo), window, cx);
                }
            }
            Action::Search => {
                self.search_origin = at;
                self.search_open = true;
                self.search
                    .update(cx, |search, cx| search.set_value("", window, cx));
                cx.set_global(Mode::Search);
                self.search.focus_handle(cx).focus(window, cx);
            }
            Action::SearchNext(forward) => {
                for _ in 0..command.count {
                    self.search_next(forward, cx);
                }
            }
            Action::Scroll(key) => self.scroll_key(&key, command.count, command.explicit_count, cx),
            Action::Save => self.save(window, cx),
        }
    }

    fn scroll_key(&mut self, key: &str, count: usize, explicit: bool, cx: &mut Context<Self>) {
        let geometry = self.geometry(cx);
        let text = self.buffer.read(cx).text.clone();
        let current = row(&text, self.vim.cursor);
        let height = self.input.read(cx).line_height().unwrap_or(px(16.));
        let cell = cx.theme().mono_font_size * 0.6;
        let mut offset = self.input.read(cx).scroll_offset();
        let rows = geometry.rows.len().max(1);
        match key {
            "ctrl-d" | "ctrl-u" | "ctrl-f" | "ctrl-b" | "pageup" | "pagedown" => {
                let forward = matches!(key, "ctrl-d" | "ctrl-f" | "pagedown");
                let half = matches!(key, "ctrl-d" | "ctrl-u");
                let amount = if half {
                    if explicit { count } else { (rows / 2).max(1) }
                } else {
                    rows.saturating_sub(2).max(1).saturating_mul(count)
                };
                let line = if forward {
                    current.saturating_add(amount)
                } else {
                    current.saturating_sub(amount)
                };
                let col = self
                    .vim
                    .column
                    .unwrap_or_else(|| display_column(&text, self.vim.cursor, self.indent));
                self.vim.column = Some(col);
                self.vim.cursor = normal_cursor(
                    &text,
                    at_column(&text, line_at(&text, line), col, self.indent),
                );
                let moved = row(&text, self.vim.cursor) as f32 - current as f32;
                offset.y -= height * moved;
            }
            "ctrl-e" | "ctrl-y" => {
                let forward = key == "ctrl-e";
                let top = if forward {
                    geometry.rows.start.saturating_add(count)
                } else {
                    geometry.rows.start.saturating_sub(count)
                };
                let line = current.clamp(top, top + rows - 1);
                self.vim.cursor = normal_cursor(
                    &text,
                    at_column(
                        &text,
                        line_at(&text, line),
                        display_column(&text, self.vim.cursor, self.indent),
                        self.indent,
                    ),
                );
                offset.y = -(height * top as f32);
            }
            "zt" | "zz" | "zb" | "zenter" | "z." | "z-" => {
                if explicit {
                    self.vim.cursor = normal_cursor(&text, line_at(&text, count - 1));
                }
                if matches!(key, "zenter" | "z." | "z-") {
                    self.vim.cursor = first_nonblank(&text, self.vim.cursor);
                }
                let margin = if matches!(key, "zz" | "z.") {
                    rows / 2
                } else if matches!(key, "zb" | "z-") {
                    rows - 1
                } else {
                    0
                };
                offset.y = -(height * row(&text, self.vim.cursor).saturating_sub(margin) as f32);
            }
            "zh" => offset.x += cell * count as f32,
            "zl" => offset.x -= cell * count as f32,
            "zH" => offset.x += cell * (geometry.columns / 2 * count) as f32,
            "zL" => offset.x -= cell * (geometry.columns / 2 * count) as f32,
            "zs" => offset.x = -(cell * display_column(&text, self.vim.cursor, self.indent) as f32),
            "ze" => {
                offset.x = -(cell
                    * display_column(&text, self.vim.cursor, self.indent)
                        .saturating_sub(geometry.columns - 1) as f32)
            }
            _ => return,
        }
        self.select(cx);
        self.input
            .update(cx, |input, cx| input.set_scroll_offset(offset, cx));
    }
}
