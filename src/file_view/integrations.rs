use super::*;
use crate::lsp::language;
use gpui_kit::base::input::FoldRange;
use serde_json::json;

impl FileView {
    pub(crate) fn is_composing(&self, cx: &App) -> bool {
        self.input.read(cx).is_composing()
    }
    pub(super) fn integration(
        &mut self,
        name: &str,
        count: usize,
        explicit: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match name {
            name if name.starts_with("ctrl-w")
                || matches!(name, "gt" | "gT" | "gf" | "gF" | "ctrl-^" | "ctrl-6") =>
            {
                cx.emit(VimEvent::Workspace(name.into(), count, explicit))
            }
            "ctrl-]" => {
                self.start_lsp_request("textDocument/definition", "definitionProvider", window, cx)
            }
            "ctrl-t" => self.tag_back(count, cx),
            "gd" | "gD" => self.local_declaration(name == "gD", cx),
            "grr" => {
                self.start_lsp_request("textDocument/references", "referencesProvider", window, cx)
            }
            "gri" => self.start_lsp_request(
                "textDocument/implementation",
                "implementationProvider",
                window,
                cx,
            ),
            "grt" => self.start_lsp_request(
                "textDocument/typeDefinition",
                "typeDefinitionProvider",
                window,
                cx,
            ),
            "gO" => self.start_lsp_request(
                "textDocument/documentSymbol",
                "documentSymbolProvider",
                window,
                cx,
            ),
            "signature" => self.start_lsp_request(
                "textDocument/signatureHelp",
                "signatureHelpProvider",
                window,
                cx,
            ),
            "grn" => self.rename_symbol(window, cx),
            "gra" => self.code_actions(window, cx),
            "[d" | "]d" | "[D" | "]D" => self.diagnostic_jump(name, count, window, cx),
            name if name.starts_with('z') => self.fold_command(name, count, cx),
            _ => self.command_error = Some(format!("Unsupported command: {name}")),
        }
    }
    pub(super) fn integration_operator(
        &mut self,
        op: Operator,
        target: &ResolvedTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !matches!(
            op,
            Operator::Reindent
                | Operator::Format
                | Operator::FormatKeep
                | Operator::Comment
                | Operator::Fold
        ) {
            return false;
        }
        let text = self.buffer.read(cx).text.clone();
        let Some(first) = target.ranges.first() else {
            return true;
        };
        let Some(last) = target.ranges.last() else {
            return true;
        };
        let range = line_range(
            &text,
            first.start,
            if last.is_empty() {
                last.end
            } else {
                prev(&text, last.end)
            },
        );
        if op == Operator::Fold {
            let rows = row(&text, range.start)..row(&text, prev(&text, range.end)) + 1;
            if rows.len() > 1 {
                self.folds.push((rows, true));
                self.publish_folds(cx);
            }
            self.vim.cursor = range.start;
            self.set_mode(Mode::Normal, cx);
            self.select(cx);
            return true;
        }
        if op == Operator::Format && self.format_lsp(range.clone(), window, cx) {
            self.set_mode(Mode::Normal, cx);
            return true;
        }
        let cursor = self.vim.cursor;
        let replacement = match op {
            Operator::Comment => {
                let path = &self.buffer.read(cx).path;
                let prefix = match path.extension().and_then(|s| s.to_str()).unwrap_or("") {
                    "py" | "sh" | "bash" | "nix" | "yaml" | "yml" | "toml" | "rb" | "pl"
                    | "conf" => "#",
                    "lua" | "sql" | "hs" => "--",
                    "vim" => "\"",
                    "rs" | "c" | "h" | "cpp" | "hpp" | "cc" | "js" | "jsx" | "ts" | "tsx"
                    | "go" | "java" | "cs" | "swift" | "kt" | "scala" | "jsonc" => "//",
                    _ => {
                        self.command_error =
                            Some("No line comment syntax configured for this file".into());
                        return true;
                    }
                };
                let lines: Vec<_> = text[range.clone()].split_inclusive('\n').collect();
                let remove = lines
                    .iter()
                    .filter(|l| !l.trim().is_empty())
                    .all(|l| l.trim_start().starts_with(prefix));
                lines
                    .into_iter()
                    .map(|line| {
                        if line.trim().is_empty() {
                            return line.to_string();
                        }
                        let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
                        if remove {
                            let mut at = indent + prefix.len();
                            if line[at..].starts_with(' ') {
                                at += 1;
                            }
                            format!("{}{}", &line[..indent], &line[at..])
                        } else {
                            format!("{}{prefix} {}", &line[..indent], &line[indent..])
                        }
                    })
                    .collect()
            }
            Operator::Format | Operator::FormatKeep => wrap_paragraphs(&text[range.clone()], 79),
            Operator::Reindent => reindent_lines(&text, range.clone(), self.indent),
            _ => unreachable!(),
        };
        self.set_mode(Mode::Normal, cx);
        let at = match op {
            Operator::FormatKeep => cursor,
            Operator::Format => {
                range.start
                    + replacement
                        .strip_suffix('\n')
                        .unwrap_or(&replacement)
                        .rfind('\n')
                        .map_or(0, |i| i + 1)
            }
            _ => range.start,
        };
        self.replace(range.clone(), &replacement, at, window, cx);
        self.vim.cursor = if op == Operator::FormatKeep {
            normal_cursor(&self.buffer.read(cx).text, at)
        } else {
            first_nonblank(&self.buffer.read(cx).text, at)
        };
        self.select(cx);
        true
    }
    fn format_lsp(
        &mut self,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(doc) = self.buffer.read(cx).lsp.clone() else {
            return false;
        };
        if !doc
            .read(cx)
            .session
            .supports("documentRangeFormattingProvider")
        {
            return false;
        }
        let Ok(snap) = doc.read(cx).snapshot(cx) else {
            return false;
        };
        let encoding = doc.read(cx).session.encoding();
        let Ok(start) = encoding.position(&snap.text, range.start) else {
            return false;
        };
        let Ok(end) = encoding.position(&snap.text, range.end) else {
            return false;
        };
        let params = json!({"range":{"start":start,"end":end},"options":{"tabSize":self.indent,"insertSpaces":true}});
        let response = doc.update(cx, |doc, cx| {
            doc.request("textDocument/rangeFormatting", &snap, params, cx)
        });
        let Ok(response) = response else {
            return false;
        };
        let change = self.change.clone();
        let cursor = self.vim.cursor;
        self.shared.update(cx, |vim, _| {
            if vim.macros.running {
                vim.macros.waiting = true;
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = response.recv().await.and_then(|value| {
                let edits: Option<Vec<lsp_types::TextEdit>> = serde_json::from_value(value)?;
                language::apply_edits(&snap.text, &edits.unwrap_or_default(), encoding)
            });
            let _ = this.update_in(cx, |this, window, cx| {
                this.shared.update(cx, |vim, _| vim.macros.waiting = false);
                if !doc.read(cx).is_current(&snap, cx)
                    || this.vim.mode.is_insert()
                    || this.vim.cursor != cursor
                {
                    this.vim_error(
                        "Formatting discarded: document or cursor changed",
                        window,
                        cx,
                    );
                    return;
                }
                match result {
                    Ok(text) => {
                        this.change = change;
                        this.change_dirty = false;
                        let edit = editing::difference(&snap.text, &text);
                        this.commit_transaction(
                            EditTransaction {
                                edits: vec![edit],
                                cursor_before: cursor,
                                cursor_after: cursor.min(text.len()),
                            },
                            window,
                            cx,
                        );
                        this.finish_change(cx);
                    }
                    Err(error) => this.vim_error(error, window, cx),
                }
            });
        })
        .detach();
        true
    }
    fn diagnostic_jump(
        &mut self,
        command: &str,
        count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(doc) = self.buffer.read(cx).lsp.clone() else {
            self.command_error = Some("No diagnostics".into());
            return;
        };
        let text = &self.buffer.read(cx).text;
        let encoding = doc.read(cx).session.encoding();
        let mut diagnostics: Vec<_> = doc
            .read(cx)
            .analysis
            .diagnostics
            .value
            .iter()
            .filter_map(|d| {
                encoding
                    .offset(text, d.range.start)
                    .ok()
                    .map(|at| (at, d.message.clone()))
            })
            .collect();
        diagnostics.sort_by_key(|(at, _)| *at);
        if diagnostics.is_empty() {
            self.command_error = Some("No diagnostics".into());
            return;
        }
        let index = match command {
            "[D" => 0,
            "]D" => diagnostics.len() - 1,
            "]d" => {
                (diagnostics
                    .iter()
                    .position(|(at, _)| *at > self.vim.cursor)
                    .unwrap_or(0)
                    + count
                    - 1)
                    % diagnostics.len()
            }
            _ => {
                (diagnostics
                    .iter()
                    .rposition(|(at, _)| *at < self.vim.cursor)
                    .unwrap_or(diagnostics.len() - 1)
                    + diagnostics.len()
                    - (count - 1) % diagnostics.len())
                    % diagnostics.len()
            }
        };
        let (at, message) = diagnostics[index].clone();
        self.remember_jump(cx);
        self.vim.cursor = at;
        self.select(cx);
        window.push_notification(Notification::info(message), cx);
    }
    fn local_declaration(&mut self, global: bool, cx: &mut Context<Self>) {
        let text = &self.buffer.read(cx).text;
        let at = self.vim.cursor;
        let key = |c: char| c.is_alphanumeric() || c == '_';
        let mut a = at;
        let mut b = at;
        while a > 0 && text[..a].chars().next_back().is_some_and(key) {
            a = prev(text, a);
        }
        while b < text.len() && text[b..].chars().next().is_some_and(key) {
            b = next(text, b);
        }
        if a == b {
            self.command_error = Some("No identifier under cursor".into());
            return;
        }
        let from = if global {
            0
        } else {
            text[..a].rfind('{').map_or(0, |i| start(text, i))
        };
        let pattern = format!("\\<\\V{}\\m\\>", &text[a..b]);
        let target = SearchMatcher::compile(&pattern)
            .and_then(|m| m.ranges(&text[from..]))
            .ok()
            .and_then(|r| r.first().map(|r| from + r.start));
        if let Some(target) = target {
            self.remember_jump(cx);
            self.vim.cursor = target;
            self.select(cx);
        }
    }
    pub(crate) fn file_name_at_cursor(
        &self,
        cx: &App,
    ) -> Option<(std::path::PathBuf, Option<usize>)> {
        let text = &self.buffer.read(cx).text;
        let allowed = |c: char| !c.is_whitespace() && !"\"'`<>[](){};,".contains(c);
        let mut a = self.vim.cursor;
        let mut b = a;
        while a > 0 && text[..a].chars().next_back().is_some_and(allowed) {
            a = prev(text, a);
        }
        while b < text.len() && text[b..].chars().next().is_some_and(allowed) {
            b = next(text, b);
        }
        let name = text[a..b].trim();
        if name.is_empty() {
            return None;
        }
        let (name, line) = name
            .rsplit_once(':')
            .and_then(|(name, line)| {
                line.parse::<usize>()
                    .ok()
                    .map(|line| (name, Some(line.saturating_sub(1))))
            })
            .unwrap_or((name, None));
        let path = self.buffer.read(cx).path.parent()?.join(name);
        Some((path, line))
    }
    pub(super) fn remap_folds(&mut self, old: &str, new: &str, cx: &mut Context<Self>) {
        for (range, _) in &mut self.folds {
            let from = line_at(old, range.start);
            let to = if range.end > row(old, old.len().saturating_sub(1)) {
                old.len()
            } else {
                line_at(old, range.end)
            };
            range.start = row(new, file_buffer::map_position(old, new, from));
            let end = file_buffer::map_position(old, new, to);
            range.end = row(new, end) + usize::from(end == new.len() && !new.ends_with('\n'));
        }
        self.folds.retain(|(range, _)| range.end > range.start + 1);
        self.publish_folds(cx);
    }
    pub(super) fn folded_motion(&self, downward: bool, count: usize, cx: &App) -> Option<usize> {
        if !self.fold_enabled || self.folds.is_empty() {
            return None;
        }
        let text = &self.buffer.read(cx).text;
        let mut line = row(text, self.vim.cursor);
        for _ in 0..count {
            if downward {
                let last = self
                    .folds
                    .iter()
                    .filter(|(r, closed)| *closed && r.contains(&line))
                    .map(|(r, _)| r.end - 1)
                    .max()
                    .unwrap_or(line);
                let next = last.saturating_add(1);
                if next <= row(text, normal_cursor(text, text.len())) {
                    line = next;
                }
            } else {
                line = line.saturating_sub(1);
                line = self
                    .folds
                    .iter()
                    .filter(|(r, closed)| *closed && r.contains(&line))
                    .map(|(r, _)| r.start)
                    .min()
                    .unwrap_or(line);
            }
        }
        let column = self
            .vim
            .column
            .unwrap_or_else(|| display_column(text, self.vim.cursor, self.indent));
        Some(normal_cursor(
            text,
            at_column(text, line_at(text, line), column, self.indent),
        ))
    }
    pub(super) fn reveal_folds(&mut self, at: usize, header: bool, cx: &mut Context<Self>) {
        let line = row(&self.buffer.read(cx).text, at);
        let mut changed = false;
        for (r, closed) in &mut self.folds {
            if *closed && r.contains(&line) && (header || line > r.start) {
                *closed = false;
                changed = true;
            }
        }
        if changed {
            self.publish_folds(cx);
        }
    }
    pub(super) fn publish_folds(&mut self, cx: &mut Context<Self>) {
        let folds: Vec<_> = self
            .folds
            .iter()
            .filter(|(_, closed)| *closed && self.fold_enabled)
            .map(|(rows, _)| FoldRange {
                start_line: rows.start,
                end_line: rows.end,
            })
            .collect();
        self.input
            .update(cx, |input, cx| input.set_manual_folds(folds, cx));
    }
    fn fold_command(&mut self, command: &str, count: usize, cx: &mut Context<Self>) {
        let line = row(&self.buffer.read(cx).text, self.vim.cursor);
        match command {
            "zj" | "zk" => {
                let mut rows: Vec<_> = self.folds.iter().map(|(r, _)| r.start).collect();
                rows.sort();
                rows.dedup();
                let candidates: Vec<_> = if command == "zj" {
                    rows.into_iter().filter(|r| *r > line).collect()
                } else {
                    rows.into_iter().rev().filter(|r| *r < line).collect()
                };
                if let Some(target) = candidates
                    .get(count.saturating_sub(1))
                    .or_else(|| candidates.last())
                {
                    self.vim.cursor = line_at(&self.buffer.read(cx).text, *target);
                }
            }
            "zm" | "zr" => {
                for _ in 0..count {
                    let levels: Vec<_> = self
                        .folds
                        .iter()
                        .map(|(r, _)| {
                            self.folds
                                .iter()
                                .filter(|(outer, _)| outer.start <= r.start && r.end <= outer.end)
                                .count()
                        })
                        .collect();
                    let levels_to_change = self
                        .folds
                        .iter()
                        .zip(&levels)
                        .filter(|((_, closed), _)| *closed == (command == "zr"))
                        .map(|(_, level)| *level);
                    let target = if command == "zm" {
                        levels_to_change.max()
                    } else {
                        levels_to_change.min()
                    };
                    if let Some(target) = target {
                        for ((_, closed), level) in self.folds.iter_mut().zip(levels) {
                            if level == target {
                                *closed = command == "zm";
                            }
                        }
                    }
                }
            }
            "zE" => self.folds.clear(),
            "zM" | "zR" => {
                self.fold_enabled = true;
                for (_, closed) in &mut self.folds {
                    *closed = command == "zM";
                }
            }
            "zn" => self.fold_enabled = false,
            "zN" => self.fold_enabled = true,
            "zi" => self.fold_enabled = !self.fold_enabled,
            _ => {
                for _ in 0..count {
                    let mut containing: Vec<_> = self
                        .folds
                        .iter()
                        .enumerate()
                        .filter(|(_, (r, _))| r.contains(&line))
                        .map(|(i, (r, _))| (i, r.len()))
                        .collect();
                    containing.sort_by_key(|(_, len)| *len);
                    let closed = containing
                        .iter()
                        .rev()
                        .find(|(i, _)| self.folds[*i].1)
                        .copied();
                    let open_fold = containing.iter().find(|(i, _)| !self.folds[*i].1).copied();
                    let candidate = match command {
                        "zo" | "zO" => closed,
                        "zc" | "zC" => open_fold,
                        _ => closed.or(open_fold),
                    };
                    let Some((index, _)) = candidate else {
                        break;
                    };
                    let recursive = matches!(command, "zA" | "zO" | "zC" | "zD");
                    let open = matches!(command, "zo" | "zO")
                        || (matches!(command, "za" | "zA") && self.folds[index].1);
                    let delete = matches!(command, "zd" | "zD");
                    if recursive {
                        let range = self.folds[index].0.clone();
                        if delete {
                            self.folds
                                .retain(|(r, _)| !(range.start <= r.start && r.end <= range.end));
                        } else {
                            for (r, closed) in &mut self.folds {
                                if range.start <= r.start && r.end <= range.end {
                                    *closed = !open;
                                }
                            }
                        }
                    } else if delete {
                        self.folds.remove(index);
                    } else {
                        self.folds[index].1 = !open;
                    }
                }
            }
        }
        self.publish_folds(cx);
        let line = row(&self.buffer.read(cx).text, self.vim.cursor);
        if let Some((range, _)) = self
            .folds
            .iter()
            .find(|(r, closed)| *closed && r.contains(&line))
        {
            self.vim.cursor = line_at(&self.buffer.read(cx).text, range.start);
        }
        self.select(cx);
    }
}
// Generic fallback, not a replacement for Neovim's language-specific indentexpr.
fn reindent_lines(text: &str, range: Range<usize>, step: usize) -> String {
    let opens = |line: &str| {
        line.split("//")
            .next()
            .unwrap_or(line)
            .trim_end()
            .ends_with(['{', '[', '(', ':'])
    };
    let mut indent = if range.start > 0 {
        let line = start(text, range.start - 1);
        display_column(text, first_nonblank(text, line), step)
            + usize::from(opens(&text[line..range.start])) * step
    } else {
        0
    };
    text[range]
        .split_inclusive('\n')
        .map(|line| {
            if line.trim().is_empty() {
                return if line.ends_with('\n') {
                    "\n".into()
                } else {
                    String::new()
                };
            }
            let body = line.trim_start_matches([' ', '\t']);
            if body.starts_with(['}', ']', ')']) {
                indent = indent.saturating_sub(step);
            }
            let result = format!("{}{body}", " ".repeat(indent));
            if opens(body) {
                indent = indent.saturating_add(step);
            }
            result
        })
        .collect()
}
fn wrap_paragraphs(text: &str, width: usize) -> String {
    let mut result = String::new();
    let mut line = String::new();
    let mut indent = String::new();
    for source in text.split_inclusive('\n') {
        if source.trim().is_empty() {
            if !line.is_empty() {
                result.push_str(&line);
                result.push('\n');
                line.clear();
            }
            result.push_str(source);
            indent.clear();
            continue;
        }
        if line.is_empty() && indent.is_empty() {
            indent = source[..source.len() - source.trim_start_matches([' ', '\t']).len()].into();
        }
        for word in source.split_whitespace() {
            if line.is_empty() {
                line = indent.clone();
            }
            let space = usize::from(line.len() > indent.len());
            if unicode_width::UnicodeWidthStr::width(line.as_str())
                + space
                + unicode_width::UnicodeWidthStr::width(word)
                > width
                && space > 0
            {
                result.push_str(&line);
                result.push('\n');
                line = indent.clone();
            } else if space > 0 {
                line.push(' ');
            }
            line.push_str(word);
        }
    }
    result.push_str(&line);
    if text.ends_with('\n') && !line.is_empty() {
        result.push('\n');
    }
    result
}
