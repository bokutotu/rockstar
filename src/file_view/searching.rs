use super::*;
use gpui_kit::{
    HighlightStyle, Pixels, Point,
    base::input::{TextDecoration, TextDecorationCollection},
};

pub(super) struct SearchSession {
    pub origin: VimViewState,
    pub scroll: Point<Pixels>,
    pub command: ParsedCommand,
    pub forward: bool,
    pub history: usize,
    pub previous_input: String,
    pub error: Option<String>,
}
pub(super) fn decorations(
    input: &Entity<EditorState>,
    cx: &mut Context<FileView>,
) -> TextDecorationCollection {
    input.update(cx, |input, cx| {
        input.create_decorations_collection(Vec::new(), cx)
    })
}
impl FileView {
    pub(super) fn begin_search(
        &mut self,
        command: ParsedCommand,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.search_session = Some(SearchSession {
            origin: self.vim.clone(),
            scroll: self.input.read(cx).scroll_offset(),
            command,
            forward,
            history: self.shared.read(cx).search_history.len(),
            previous_input: String::new(),
            error: None,
        });
        self.search_open = true;
        self.search
            .update(cx, |input, cx| input.set_value("", window, cx));
        cx.set_global(Mode::Search);
        self.search.focus_handle(cx).focus(window, cx);
        cx.notify();
    }
    pub(super) fn highlight_search(&self, ranges: &[Range<usize>], cx: &mut Context<Self>) {
        let style = HighlightStyle {
            background_color: Some(cx.theme().selection),
            ..Default::default()
        };
        self.search_highlights.set(
            ranges
                .iter()
                .filter(|r| !r.is_empty())
                .map(|r| TextDecoration::new(r.clone(), style))
                .collect(),
            cx,
        );
    }
    pub(super) fn update_search(&mut self, cx: &mut Context<Self>) {
        let input = self.search.read(cx).value().to_string();
        let Some(session) = &mut self.search_session else {
            return;
        };
        let old = std::mem::replace(&mut session.previous_input, input.clone());
        let forward = session.forward;
        let origin = session.origin.clone();
        let count = session.command.count;
        self.record_native_input(&old, &input, old.len(), false, cx);
        if input.is_empty() {
            self.vim = origin;
            self.select(cx);
            self.search_highlights.clear(cx);
            return;
        }
        let result = SearchSpec::parse(&input, forward, self.shared.read(cx).search.as_ref())
            .and_then(|spec| {
                let ranges = SearchMatcher::compile(&spec.pattern)?
                    .navigation_ranges(&self.buffer.read(cx).text)?;
                let destination = choose_match(
                    &self.buffer.read(cx).text,
                    &ranges,
                    origin.cursor,
                    forward,
                    count,
                    false,
                )
                .map(|range| spec.destination(&self.buffer.read(cx).text, range));
                Ok((ranges, destination))
            });
        match result {
            Ok((ranges, destination)) => {
                self.highlight_search(&ranges, cx);
                self.vim = origin;
                if let Some(to) = destination {
                    self.vim.cursor = to;
                }
                self.search_session.as_mut().unwrap().error =
                    destination.is_none().then(|| "Pattern not found".into());
                self.select(cx);
            }
            Err(error) => {
                self.search_session.as_mut().unwrap().error = Some(error.to_string());
            }
        }
        cx.notify();
    }
    pub(crate) fn end_search(&mut self, accept: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.search_session.take() else {
            return;
        };
        self.search_open = false;
        self.vim = session.origin;
        self.input
            .update(cx, |input, cx| input.set_scroll_offset(session.scroll, cx));
        cx.set_global(self.vim.mode);
        self.input.focus_handle(cx).focus(window, cx);
        self.select(cx);
        if !accept {
            self.search_highlights.clear(cx);
            return;
        }
        let input = self.search.read(cx).value().to_string();
        let spec = match SearchSpec::parse(
            &input,
            session.forward,
            self.shared.read(cx).search.as_ref(),
        ) {
            Ok(spec) => spec,
            Err(error) => {
                self.vim_error(error, window, cx);
                return;
            }
        };
        self.shared.update(cx, |shared, _| {
            if !input.is_empty() {
                shared.search_history.retain(|s| s != &input);
                shared.search_history.push(input);
                if shared.search_history.len() > 100 {
                    shared.search_history.remove(0);
                }
            }
            shared.search = Some(spec.clone());
        });
        let op = if let Action::Search(_, op) = session.command.action {
            op
        } else {
            None
        };
        let target = Target::Search {
            spec: Some(spec),
            reverse: false,
            select: false,
        };
        let mut command = session.command;
        command.action = op.map_or_else(
            || Action::Target(target.clone()),
            |op| Action::Operate(op, Some(target.clone())),
        );
        self.execute(command, window, cx);
    }
    pub(super) fn search_key(
        &mut self,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match key.key.as_str() {
            "enter" => {
                self.end_search(true, window, cx);
                true
            }
            "up" | "down" => {
                let history = &self.shared.read(cx).search_history;
                let Some(session) = &mut self.search_session else {
                    return false;
                };
                session.history = if key.key == "up" {
                    session.history.saturating_sub(1)
                } else {
                    (session.history + 1).min(history.len())
                };
                let value = history.get(session.history).cloned().unwrap_or_default();
                self.search
                    .update(cx, |input, cx| input.set_value(value, window, cx));
                true
            }
            "backspace" if self.search.read(cx).value().is_empty() => {
                self.end_search(false, window, cx);
                true
            }
            _ => false,
        }
    }
    pub(super) fn resolve_search(
        &mut self,
        target: &Target,
        command: &ParsedCommand,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<ResolvedTarget> {
        let text = self.buffer.read(cx).text.clone();
        let at = self.vim.cursor;
        let (spec, reverse, select) = match target {
            Target::WordSearch { forward, whole } => {
                let mut a = at;
                while a < end(&text, at)
                    && text[a..].chars().next().is_some_and(char::is_whitespace)
                {
                    a = next(&text, a);
                }
                anyhow::ensure!(a < text.len(), "No word under cursor");
                let keyword = |c: char| c.is_alphanumeric() || c == '_';
                let word = keyword(text[a..].chars().next().unwrap());
                let same = |c: char| !c.is_whitespace() && keyword(c) == word;
                let mut b = a;
                while a > start(&text, at) && same(text[prev(&text, a)..].chars().next().unwrap()) {
                    a = prev(&text, a);
                }
                while b < end(&text, at) && same(text[b..].chars().next().unwrap()) {
                    b = next(&text, b);
                }
                let literal = text[a..b].replace('\\', "\\\\");
                let pattern = if *whole && word {
                    format!("\\<\\V{literal}\\m\\>")
                } else {
                    format!("\\V{literal}")
                };
                let spec = SearchSpec {
                    pattern,
                    forward: *forward,
                    offset: SearchOffset::None,
                };
                self.shared
                    .update(cx, |shared, _| shared.search = Some(spec.clone()));
                (spec, false, false)
            }
            Target::Search {
                spec,
                reverse,
                select,
            } => (
                spec.clone()
                    .or_else(|| self.shared.read(cx).search.clone())
                    .ok_or_else(|| anyhow::anyhow!("No previous search pattern"))?,
                *reverse,
                *select,
            ),
            _ => anyhow::bail!("Not a search target"),
        };
        let ranges = SearchMatcher::compile(&spec.pattern)?.navigation_ranges(&text)?;
        let range = choose_match(
            &text,
            &ranges,
            at,
            spec.forward != reverse,
            command.count,
            select,
        )
        .ok_or_else(|| anyhow::anyhow!("Pattern not found: {}", spec.pattern))?
        .clone();
        self.highlight_search(&ranges, cx);
        if select {
            return Ok(ResolvedTarget::character(range));
        }
        let to = spec.destination(&text, &range);
        let operator = if let Action::Operate(op, _) = command.action {
            Some(op)
        } else {
            None
        };
        Ok(motion_target(
            &text,
            at,
            to,
            matches!(spec.offset, SearchOffset::Line(_)),
            matches!(spec.offset, SearchOffset::End(_)),
            operator,
            command.forced,
            self.indent,
        ))
    }
}
