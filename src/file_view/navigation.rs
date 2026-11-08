use super::*;
use gpui_kit::EventEmitter;

#[derive(Clone)]
pub(crate) enum VimEvent {
    Navigate(BufferLocation, bool),
    Workspace(String, usize, bool),
    ApplyEdit(lsp_types::WorkspaceEdit, crate::lsp::language::Encoding),
}
impl EventEmitter<VimEvent> for FileView {}

impl FileView {
    pub(crate) fn location(&self, cx: &App) -> BufferLocation {
        let buffer = self.buffer.read(cx);
        BufferLocation::new(
            self.buffer.entity_id(),
            buffer.path.clone(),
            &buffer.text,
            self.vim.cursor,
        )
    }
    pub(crate) fn remember_jump(&mut self, cx: &mut Context<Self>) {
        let location = self.location(cx);
        self.jumps.update(cx, |jumps, _| jumps.push(location));
    }
    pub(super) fn set_mark(&mut self, name: char, cx: &mut Context<Self>) {
        if name.is_ascii_uppercase() {
            let location = self.location(cx);
            self.shared.update(cx, |shared, _| {
                shared.marks.insert(name, location);
            });
        } else if name.is_ascii_lowercase() || "[]<>.^".contains(name) {
            let at = self.vim.cursor;
            self.buffer.update(cx, |buffer, _| {
                buffer.positions.marks.insert(name, at);
            });
        } else if matches!(name, '\'' | '`') {
            self.remember_jump(cx);
        } else {
            self.command_error = Some("Invalid mark name".into());
        }
    }
    fn mark_location(&self, name: char, cx: &App) -> Option<BufferLocation> {
        if name.is_ascii_uppercase() {
            return self.shared.read(cx).marks.get(&name).cloned();
        }
        if matches!(name, '\'' | '`') {
            return self.jumps.read(cx).previous.clone();
        }
        let buffer = self.buffer.read(cx);
        let at = match name {
            '<' => self
                .vim
                .last_visual
                .as_ref()
                .map(|s| s.anchor.min(s.cursor)),
            '>' => self
                .vim
                .last_visual
                .as_ref()
                .map(|s| s.anchor.max(s.cursor)),
            _ => buffer.positions.marks.get(&name).copied(),
        }?;
        Some(BufferLocation::new(
            self.buffer.entity_id(),
            buffer.path.clone(),
            &buffer.text,
            at,
        ))
    }
    pub(super) fn resolve_mark_target(
        &mut self,
        name: char,
        linewise: bool,
        jump: bool,
        operator: Option<Operator>,
        cx: &mut Context<Self>,
    ) -> Option<ResolvedTarget> {
        let Some(mut location) = self.mark_location(name, cx) else {
            self.command_error = Some(format!("Mark {name} is not set"));
            return None;
        };
        if location.buffer != self.buffer.entity_id() {
            if operator.is_some() {
                self.command_error = Some("An operator cannot cross buffers".into());
                return None;
            }
            if jump {
                self.remember_jump(cx);
            }
            if linewise {
                location.column = 0;
            }
            self.request_location(location, linewise, cx);
            return None;
        }
        let text = &self.buffer.read(cx).text;
        let to = if linewise {
            first_nonblank(text, location.offset(text))
        } else {
            normal_cursor(text, location.offset(text))
        };
        Some(motion_target(
            text,
            self.vim.cursor,
            to,
            linewise,
            false,
            operator,
            None,
            self.indent,
        ))
    }
    fn request_location(
        &mut self,
        location: BufferLocation,
        linewise: bool,
        cx: &mut Context<Self>,
    ) {
        self.shared.update(cx, |shared, cx| {
            if shared.macros.running {
                shared.macros.waiting = true;
            }
            cx.notify();
        });
        cx.emit(VimEvent::Navigate(location, linewise));
    }
    pub(super) fn tag_back(&mut self, count: usize, cx: &mut Context<Self>) {
        let Some(location) = self.jumps.update(cx, |jumps, _| jumps.pop_tag(count)) else {
            self.command_error = Some("Tag stack is empty".into());
            return;
        };
        self.remember_jump(cx);
        if location.buffer == self.buffer.entity_id() {
            self.vim.cursor = normal_cursor(
                &self.buffer.read(cx).text,
                location.offset(&self.buffer.read(cx).text),
            );
            self.vim.column = None;
            self.reveal_folds(self.vim.cursor, false, cx);
            self.select(cx);
        } else {
            self.request_location(location, false, cx);
        }
    }
    pub(super) fn jump_history(&mut self, newer: bool, count: usize, cx: &mut Context<Self>) {
        let current = self.location(cx);
        let location = self
            .jumps
            .update(cx, |jumps, _| jumps.step(current, newer, count));
        let Some(location) = location else {
            self.command_error = Some("No more jumps".into());
            return;
        };
        if location.buffer == self.buffer.entity_id() {
            self.vim.cursor = normal_cursor(
                &self.buffer.read(cx).text,
                location.offset(&self.buffer.read(cx).text),
            );
            self.vim.column = None;
            self.reveal_folds(self.vim.cursor, false, cx);
            self.select(cx);
        } else {
            self.request_location(location, false, cx);
        }
    }
    pub(super) fn change_history(&mut self, newer: bool, count: usize, cx: &mut Context<Self>) {
        let changes = &self.buffer.read(cx).positions.changes;
        if changes.is_empty() {
            self.command_error = Some("Change list is empty".into());
            return;
        }
        let current = self.change_index.unwrap_or(changes.len());
        let index = if newer {
            (current + count).min(changes.len() - 1)
        } else {
            current.saturating_sub(count)
        };
        if index == current {
            self.command_error = Some("No more changes".into());
            return;
        }
        self.vim.cursor = normal_cursor(&self.buffer.read(cx).text, changes[index]);
        self.change_index = Some(index);
        self.vim.column = None;
        self.reveal_folds(self.vim.cursor, false, cx);
        self.select(cx);
    }
    pub(super) fn insert_last(
        &mut self,
        count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(at) = self.buffer.read(cx).positions.marks.get(&'^').copied() {
            self.vim.cursor = boundary(&self.buffer.read(cx).text, at);
        }
        self.begin_insert("i", count, window, cx);
    }
    pub(super) fn restore_view(
        &mut self,
        before: &str,
        at: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = self.buffer.read(cx).text.clone();
        let id = self.buffer.entity_id();
        self.jumps
            .update(cx, |jumps, _| jumps.remap(id, before, &text));
        self.vim.cursor = normal_cursor(&text, at);
        self.set_input(&text, self.vim.cursor..self.vim.cursor, false, window, cx);
        self.remap_folds(before, &text, cx);
        self.reveal_folds(self.vim.cursor, false, cx);
        self.select(cx);
    }
    pub(super) fn undo_time(
        &mut self,
        newer: bool,
        count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.insert = None;
        self.change = None;
        let before = self.buffer.read(cx).text.clone();
        if let Some(at) = self
            .buffer
            .update(cx, |buffer, cx| buffer.undo_time(newer, count, cx))
        {
            self.restore_view(&before, at, window, cx);
        } else {
            self.command_error = Some("No older/newer text state".into());
        }
    }
    pub(super) fn undo_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((from, saved)) = self.buffer.read(cx).positions.line_undo.clone() else {
            self.command_error = Some("No line to restore".into());
            return;
        };
        let text = self.buffer.read(cx).text.clone();
        let from = start(&text, from);
        let to = end(&text, from);
        let previous = text[from..to].to_string();
        self.replace(from..to, &saved, from, window, cx);
        self.buffer.update(cx, |buffer, _| {
            buffer.positions.line_undo = Some((from, previous))
        });
    }
}
