use super::*;

pub(crate) struct ViewGeometry {
    pub rows: Range<usize>,
    pub left_column: usize,
    pub columns: usize,
    pub tab: usize,
}

pub(crate) fn word_forward(text: &str, mut at: usize, count: usize, big: bool) -> usize {
    for _ in 0..count {
        let c = class(text, at, big);
        if c != 0 {
            while at < text.len() && class(text, at, big) == c {
                at = next(text, at);
            }
        } else if at < text.len() {
            at = next(text, at);
        }
        while at < text.len() && class(text, at, big) == 0 {
            if start(text, at) == at && end(text, at) == at {
                break;
            }
            at = next(text, at);
        }
    }
    at
}
pub(crate) fn word_backward(text: &str, mut at: usize, count: usize, big: bool) -> usize {
    for _ in 0..count {
        at = prev(text, at);
        while at > 0 && class(text, at, big) == 0 {
            if start(text, at) == at && end(text, at) == at {
                break;
            }
            at = prev(text, at);
        }
        let c = class(text, at, big);
        if c != 0 {
            while at > 0 && class(text, prev(text, at), big) == c {
                at = prev(text, at);
            }
        }
    }
    at
}
fn word_end(text: &str, mut at: usize, count: usize, big: bool, current: bool) -> usize {
    for n in 0..count {
        if !current || n > 0 {
            at = next(text, at);
        }
        while at < text.len() && class(text, at, big) == 0 {
            at = next(text, at);
        }
        let c = class(text, at, big);
        while next(text, at) < text.len() && class(text, next(text, at), big) == c {
            at = next(text, at);
        }
    }
    at
}
fn paragraph(text: &str, at: usize, count: usize, forward: bool) -> usize {
    let last = row(text, normal_cursor(text, text.len()));
    let mut line = row(text, at);
    let blank = |n| {
        let p = line_at(text, n);
        p == end(text, p)
    };
    for _ in 0..count {
        if forward {
            while line < last && blank(line) {
                line += 1;
            }
            while line < last && !blank(line) {
                line += 1;
            }
        } else {
            while line > 0 && blank(line) {
                line -= 1;
            }
            while line > 0 && !blank(line) {
                line -= 1;
            }
        }
    }
    if forward && line == last && !blank(line) {
        normal_cursor(text, text.len())
    } else {
        line_at(text, line)
    }
}
pub(super) fn sentence_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    let mut at = 0;
    while at < text.len() {
        let c = text[at..].chars().next().unwrap();
        let mut p = next(text, at);
        if matches!(c, '.' | '!' | '?') {
            while p < text.len() && text[p..].starts_with([')', ']', '"', '\'']) {
                p = next(text, p);
            }
            if p == text.len() || text[p..].starts_with(char::is_whitespace) {
                while p < text.len() && text[p..].starts_with(char::is_whitespace) {
                    p = next(text, p);
                }
                if p < text.len() {
                    starts.push(p);
                }
            }
        } else if c == '\n' && (at == 0 || text[..at].ends_with('\n')) {
            starts.push(at);
            while p < text.len() && text[p..].starts_with(char::is_whitespace) {
                p = next(text, p);
            }
            if p < text.len() {
                starts.push(p);
            }
        }
        at = next(text, at);
    }
    starts.sort_unstable();
    starts.dedup();
    starts
}

/// Skip quoted strings and comments when looking for code delimiters. A lone
/// apostrophe (e.g. a Rust lifetime) is not a quoted string.
pub(super) fn bracket_pairs(text: &str, open: char, close: char) -> Vec<(usize, usize)> {
    let mut stack = Vec::new();
    let mut pairs = Vec::new();
    let mut at = 0;
    while at < text.len() {
        if text[at..].starts_with("//") {
            at = next(text, end(text, at));
            continue;
        }
        if text[at..].starts_with("/*") {
            if let Some(n) = text[at + 2..].find("*/") {
                at += n + 4;
                continue;
            }
        }
        let c = text[at..].chars().next().unwrap();
        if matches!(c, '\'' | '"' | '`') {
            let mut p = next(text, at);
            while p < end(text, at) {
                if text[p..].starts_with('\\') {
                    p = next(text, next(text, p));
                    continue;
                }
                if text[p..].starts_with(c) {
                    at = next(text, p);
                    break;
                }
                p = next(text, p);
            }
            if p < end(text, p) && text[p..].starts_with(c) {
                continue;
            }
        }
        if c == open {
            stack.push(at);
        }
        if c == close {
            if let Some(start) = stack.pop() {
                pairs.push((start, at));
            }
        }
        at = next(text, at);
    }
    pairs
}
fn match_percent(text: &str, at: usize) -> Option<usize> {
    // C comment boundaries are also '%' targets.
    let line_end = end(text, at);
    for (off, c) in text[at..line_end].char_indices() {
        let pos = at + off;
        if text[pos..].starts_with("/*") {
            return text[pos + 2..].find("*/").map(|n| pos + 3 + n);
        }
        if text[pos..].starts_with("*/") {
            return text[..pos].rfind("/*");
        }
        let (open, close) = match c {
            '(' | ')' => ('(', ')'),
            '[' | ']' => ('[', ']'),
            '{' | '}' => ('{', '}'),
            _ => continue,
        };
        if let Some((a, b)) = bracket_pairs(text, open, close)
            .into_iter()
            .find(|(a, b)| *a == pos || *b == pos)
        {
            return Some(if a == pos { b } else { a });
        }
        return None;
    }
    None
}
fn char_find(text: &str, at: usize, find: CharFind, count: usize, repeat: bool) -> Option<usize> {
    let mut from = at;
    // Repeating 't' must get past the match immediately beside the cursor.
    if repeat && find.till {
        if (find.forward && from >= end(text, at)) || (!find.forward && from <= start(text, at)) {
            return None;
        }
        from = if find.forward {
            next(text, from)
        } else {
            prev(text, from)
        };
    }
    let found = if find.forward {
        text[next(text, from).min(end(text, at))..end(text, at)]
            .char_indices()
            .filter(|(_, c)| *c == find.character)
            .nth(count - 1)
            .map(|(i, _)| next(text, from) + i)
    } else {
        text[start(text, at)..from]
            .char_indices()
            .rev()
            .filter(|(_, c)| *c == find.character)
            .nth(count - 1)
            .map(|(i, _)| start(text, at) + i)
    }?;
    Some(if find.till {
        if find.forward {
            prev(text, found)
        } else {
            next(text, found)
        }
    } else {
        found
    })
}

pub(crate) fn visual_target(text: &str, view: &VimViewState, tab: usize) -> ResolvedTarget {
    match view.mode {
        Mode::VisualLine => ResolvedTarget::line(line_range(text, view.anchor, view.cursor)),
        Mode::VisualBlock => block_target(text, view.anchor, view.cursor, view.block_columns, tab),
        _ => ResolvedTarget::character(
            view.anchor.min(view.cursor)..next(text, view.anchor.max(view.cursor)),
        ),
    }
}
pub(crate) fn block_target(
    text: &str,
    anchor: usize,
    cursor: usize,
    columns: (usize, usize),
    tab: usize,
) -> ResolvedTarget {
    let cols = columns.0.min(columns.1)..columns.0.max(columns.1).saturating_add(1);
    let ranges = (row(text, anchor.min(cursor))..=row(text, anchor.max(cursor)))
        .map(|line| {
            let s = line_at(text, line);
            let a = at_column(text, s, cols.start, tab);
            let b = at_column(text, s, cols.end, tab);
            a..if b < end(text, s) && display_column(text, b, tab) < cols.end {
                next(text, b)
            } else {
                b
            }
        })
        .collect();
    ResolvedTarget {
        destination: cursor,
        ranges,
        kind: SelectionKind::Block,
        block_columns: Some(cols),
    }
}

pub(crate) fn resolve_target(
    text: &str,
    view: &VimViewState,
    shared: &VimSharedState,
    target: &Target,
    count: usize,
    explicit: bool,
    operator: Option<Operator>,
    forced: Option<SelectionKind>,
    geometry: &ViewGeometry,
) -> Option<ResolvedTarget> {
    let count = count.clamp(1, 100_000);
    let at = boundary(text, view.cursor);
    if let Target::TextObject { object, around } = target {
        let mut resolved = super::object::text_object(text, view, *object, *around, count)?;
        let range = resolved.ranges.first()?.clone();
        if forced == Some(SelectionKind::Line) {
            resolved = ResolvedTarget::line(line_range(text, range.start, prev(text, range.end)));
        } else if forced == Some(SelectionKind::Character) {
            resolved.kind = SelectionKind::Character;
        } else if forced == Some(SelectionKind::Block) {
            let to = prev(text, range.end);
            resolved = block_target(
                text,
                range.start,
                to,
                (
                    display_column(text, range.start, geometry.tab),
                    display_column(text, to, geometry.tab),
                ),
                geometry.tab,
            );
        } else if operator == Some(Operator::Delete)
            && resolved.kind == SelectionKind::Character
            && row(text, range.start) < row(text, range.end)
            && range.start <= first_nonblank(text, range.start)
            && text[range.end..end(text, range.end)].trim().is_empty()
        {
            // A counted word object can end on the empty-line word itself;
            // sentence objects instead end just before that paragraph boundary.
            let empty_word =
                matches!(object, 'w' | 'W') && count > 1 && range.end == end(text, range.end);
            let last = if start(text, range.end) == range.end && !empty_word {
                prev(text, range.end)
            } else {
                range.end
            };
            resolved = ResolvedTarget::line(line_range(text, range.start, last));
        }
        return Some(resolved);
    }
    if matches!(target, Target::Lines) {
        return Some(ResolvedTarget::line(line_range(
            text,
            at,
            line_at(text, row(text, at) + count - 1),
        )));
    }
    let Target::Motion(motion) = target else {
        return None;
    };
    let mut to = at;
    let mut inclusive = false;
    let mut linewise = false;
    match motion {
        Motion::Find(find) => {
            to = char_find(text, at, *find, count, false)?;
            inclusive = find.forward;
        }
        Motion::RepeatFind(same) => {
            let mut find = shared.last_find?;
            if !same {
                find.forward = !find.forward;
            }
            to = char_find(text, at, find, count, true)?;
            inclusive = find.forward;
        }
        Motion::Key(key) => match key.as_str() {
            "j" | "down" | "k" | "up" | "gj" | "gk" => {
                let col = view
                    .column
                    .unwrap_or_else(|| display_column(text, at, geometry.tab));
                let line = if matches!(key.as_str(), "j" | "down" | "gj") {
                    row(text, at).saturating_add(count)
                } else {
                    row(text, at).saturating_sub(count)
                };
                let s = line_at(text, line);
                to = if col == usize::MAX {
                    end(text, s)
                } else {
                    at_column(text, s, col, geometry.tab)
                };
                linewise = true;
            }
            "h" | "left" | "backspace" => {
                for _ in 0..count {
                    if to > start(text, to) || key == "backspace" {
                        to = prev(text, to);
                    }
                }
            }
            "l" | "right" | " " => {
                for _ in 0..count {
                    to = if key == " " {
                        next(text, to)
                    } else {
                        next(text, to).min(end(text, to))
                    };
                }
            }
            "0" | "home" => to = start(text, at),
            "^" => to = first_nonblank(text, at),
            "$" | "end" | "g_" => {
                let s = line_at(text, row(text, at) + count - 1);
                to = end(text, s);
                if key == "g_" {
                    while to > s && text[prev(text, to)..to].chars().all(char::is_whitespace) {
                        to = prev(text, to);
                    }
                }
                to = if to > s { prev(text, to) } else { to };
                inclusive = true;
            }
            "w" | "W" => {
                if operator == Some(Operator::Change) && class(text, at, false) != 0 {
                    to = word_end(text, at, count, key == "W", true);
                    inclusive = true;
                } else {
                    let last = if count == 1 {
                        at
                    } else {
                        word_forward(text, at, count - 1, key == "W")
                    };
                    to = word_forward(text, last, 1, key == "W");
                    if operator.is_some() {
                        if class(text, last, false) != 0 && to > end(text, last) {
                            to = end(text, last);
                        }
                        to = to.min(end(text, normal_cursor(text, text.len())));
                        if operator == Some(Operator::Change) && at == end(text, at) {
                            to = at;
                        }
                    }
                }
            }
            "b" | "B" => to = word_backward(text, at, count, key == "B"),
            "e" | "E" => {
                to = word_end(text, at, count, key == "E", false);
                inclusive = true;
            }
            "ge" | "gE" => {
                for _ in 0..count {
                    let c = class(text, to, key == "gE");
                    if c != 0 {
                        while to > 0 && class(text, prev(text, to), key == "gE") == c {
                            to = prev(text, to);
                        }
                    }
                    to = prev(text, to);
                    while to > 0 && class(text, to, key == "gE") == 0 {
                        if start(text, to) == to && end(text, to) == to {
                            break;
                        }
                        to = prev(text, to);
                    }
                }
                inclusive = true;
            }
            "gg" | "G" => {
                to = first_nonblank(
                    text,
                    if key == "G" && !explicit {
                        normal_cursor(text, text.len())
                    } else {
                        line_at(text, count - 1)
                    },
                );
                linewise = true;
            }
            "enter" | "+" | "-" | "_" => {
                let line = match key.as_str() {
                    "-" => row(text, at).saturating_sub(count),
                    "_" => row(text, at) + count - 1,
                    _ => row(text, at) + count,
                };
                to = first_nonblank(text, line_at(text, line));
                linewise = true;
            }
            "|" => to = at_column(text, start(text, at), count - 1, geometry.tab),
            "go" => to = boundary(text, count - 1),
            "%" if explicit => {
                if count > 100 {
                    return None;
                }
                let total = row(text, normal_cursor(text, text.len())) + 1;
                to = first_nonblank(
                    text,
                    line_at(text, (total * count).div_ceil(100).saturating_sub(1)),
                );
                linewise = true;
            }
            "%" => {
                to = match_percent(text, at)?;
                inclusive = true;
            }
            "{" | "}" => to = paragraph(text, at, count, key == "}"),
            "(" | ")" => {
                let starts = sentence_starts(text);
                for _ in 0..count {
                    to = if key == ")" {
                        starts
                            .iter()
                            .copied()
                            .find(|p| *p > to)
                            .unwrap_or(text.len())
                    } else {
                        starts.iter().rev().copied().find(|p| *p < to).unwrap_or(0)
                    };
                }
            }
            "H" | "M" | "L" => {
                let line = match key.as_str() {
                    "H" => geometry.rows.start + count - 1,
                    "L" => geometry
                        .rows
                        .end
                        .saturating_sub(count)
                        .max(geometry.rows.start),
                    _ => (geometry.rows.start + geometry.rows.end.saturating_sub(1)) / 2,
                };
                to = first_nonblank(text, line_at(text, line));
                linewise = true;
            }
            "g0" | "g^" | "g$" | "gm" | "gM" => {
                let col = match key.as_str() {
                    "g0" | "g^" => geometry.left_column,
                    "g$" => geometry.left_column + geometry.columns.saturating_sub(1),
                    "gm" => geometry.left_column + geometry.columns / 2,
                    _ => {
                        display_column(text, end(text, at), geometry.tab)
                            * if explicit { count.min(100) } else { 50 }
                            / 100
                    }
                };
                to = at_column(text, start(text, at), col, geometry.tab);
                if key == "g^" {
                    while to < end(text, at) && class(text, to, false) == 0 {
                        to = next(text, to);
                    }
                }
                inclusive = key == "g$";
            }
            "[(" | "])" | "[{" | "]}" => {
                let (open, close) = if key.contains('(') || key.contains(')') {
                    ('(', ')')
                } else {
                    ('{', '}')
                };
                let mut enclosing = bracket_pairs(text, open, close)
                    .into_iter()
                    .filter(|(a, b)| *a < at && *b > at)
                    .collect::<Vec<_>>();
                enclosing.sort_by_key(|(a, b)| b - a);
                let (a, b) = *enclosing.get(count - 1)?;
                to = if key.starts_with('[') { a } else { b };
            }
            "[[" | "]]" | "[]" | "][" | "[m" | "]m" | "[M" | "]M" => {
                let forward = key.starts_with(']');
                let wanted = if matches!(key.as_str(), "[]" | "][" | "[M" | "]M") {
                    '}'
                } else {
                    '{'
                };
                let method = key.ends_with(['m', 'M']);
                let positions = text
                    .char_indices()
                    .filter(|(i, c)| *c == wanted && (method || start(text, *i) == *i))
                    .map(|(i, _)| i)
                    .collect::<Vec<_>>();
                to = if forward {
                    positions.into_iter().filter(|p| *p > at).nth(count - 1)
                } else {
                    positions
                        .into_iter()
                        .rev()
                        .filter(|p| *p < at)
                        .nth(count - 1)
                }?;
            }
            "[/" | "[*" | "]/" | "]*" => {
                let forward = key.starts_with(']');
                to = if forward {
                    text[at..]
                        .match_indices("*/")
                        .nth(count - 1)
                        .map(|(i, _)| at + i + 1)
                } else {
                    text[..at]
                        .rmatch_indices("/*")
                        .nth(count - 1)
                        .map(|(i, _)| i)
                }?;
            }
            "[#" | "]#" => {
                let forward = key.starts_with(']');
                let mut nesting = 0usize;
                let mut found = 0;
                let lines = text
                    .split_inclusive('\n')
                    .scan(0, |pos, line| {
                        let p = *pos;
                        *pos += line.len();
                        Some((p, line.trim_start()))
                    })
                    .collect::<Vec<_>>();
                let iter: Box<dyn Iterator<Item = &(usize, &str)>> = if forward {
                    Box::new(lines.iter())
                } else {
                    Box::new(lines.iter().rev())
                };
                let mut result = None;
                for &(pos, line) in iter {
                    if (forward && pos <= start(text, at)) || (!forward && pos >= start(text, at)) {
                        continue;
                    }
                    let line = line.strip_prefix('#').map(str::trim_start).unwrap_or("");
                    let open = line.starts_with("if");
                    let close = line.starts_with("endif");
                    let branch = line.starts_with("else") || line.starts_with("elif");
                    if if forward { open } else { close } {
                        nesting += 1;
                    } else if if forward { close } else { open } {
                        if nesting > 0 {
                            nesting -= 1;
                        } else {
                            found += 1;
                        }
                    } else if branch && nesting == 0 {
                        found += 1;
                    }
                    if found == count {
                        result = Some(first_nonblank(text, pos));
                        break;
                    }
                }
                to = result?;
            }
            _ => return None,
        },
    }
    to = if inclusive {
        normal_cursor(text, to)
    } else {
        boundary(text, to)
    };
    if operator.is_some()
        && to == at
        && !inclusive
        && !linewise
        && operator != Some(Operator::Change)
    {
        return None;
    }
    if forced == Some(SelectionKind::Character)
        && matches!(motion, Motion::Key(key) if matches!(key.as_str(), "l" | "right"))
        && normal_cursor(text, to) == at
    {
        return None;
    }
    if let Some(kind) = forced {
        match kind {
            SelectionKind::Line => linewise = true,
            SelectionKind::Character => {
                inclusive = linewise || !inclusive;
                linewise = false;
            }
            SelectionKind::Block => {
                return Some(block_target(
                    text,
                    at,
                    to,
                    (
                        display_column(text, at, geometry.tab),
                        display_column(text, to, geometry.tab),
                    ),
                    geometry.tab,
                ));
            }
        }
    }
    let mut lo = at.min(to);
    let mut hi = at.max(to);
    if !linewise && !inclusive && row(text, lo) < row(text, hi) && start(text, hi) == hi {
        hi = prev(text, hi);
        if lo <= first_nonblank(text, lo) {
            linewise = true;
        }
    }
    let ranges = if linewise {
        vec![line_range(text, lo, hi)]
    } else {
        if inclusive {
            hi = next(text, hi).min(end(text, hi));
        }
        lo = boundary(text, lo);
        // d-special: a multiline delete with only whitespace outside it
        // removes whole lines, unless characterwise operation was forced.
        if operator == Some(Operator::Delete)
            && forced != Some(SelectionKind::Character)
            && row(text, lo) < row(text, hi)
            && lo <= first_nonblank(text, lo)
            && text[hi..end(text, hi)].trim().is_empty()
        {
            linewise = true;
            vec![line_range(text, lo, hi)]
        } else {
            vec![lo..hi]
        }
    };
    Some(ResolvedTarget {
        destination: normal_cursor(text, to),
        ranges,
        kind: if linewise {
            SelectionKind::Line
        } else {
            SelectionKind::Character
        },
        block_columns: None,
    })
}
