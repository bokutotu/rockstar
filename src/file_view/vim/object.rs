use super::motion::{bracket_pairs, sentence_starts, word_forward};
use super::*;

pub(super) fn text_object(
    text: &str,
    view: &VimViewState,
    object: char,
    around: bool,
    count: usize,
) -> Option<ResolvedTarget> {
    let at = boundary(text, view.cursor);
    match object {
        'w' | 'W' => {
            let big = object == 'W';
            let c = class(text, at, big);
            let mut lo = at;
            while lo > start(text, at) && class(text, prev(text, lo), big) == c {
                lo = prev(text, lo);
            }
            let mut hi = at;
            if at == end(text, at) {
                if !around && count == 1 {
                    return Some(ResolvedTarget::character(at..at));
                }
                hi = word_forward(text, at, 1, big);
                let next_class = class(text, hi, big);
                while hi < text.len() && class(text, hi, big) == next_class {
                    hi = next(text, hi);
                }
                return Some(ResolvedTarget::character(lo..hi));
            }
            while hi < end(text, at) && class(text, hi, big) == c {
                hi = next(text, hi);
            }
            for _ in 1..count {
                if hi >= end(text, normal_cursor(text, text.len())) {
                    return None;
                }
                if text[hi..].starts_with('\n') {
                    hi = next(text, hi);
                }
                if around {
                    while hi < text.len() && class(text, hi, big) == 0 {
                        hi = next(text, hi);
                    }
                }
                if hi == start(text, hi) && hi == end(text, hi) {
                    continue;
                }
                let class = class(text, hi, big);
                while hi < text.len() && super::class(text, hi, big) == class {
                    hi = next(text, hi);
                }
            }
            if around {
                if c == 0 {
                    let next_class = class(text, hi, big);
                    while hi < text.len() && class(text, hi, big) == next_class {
                        hi = next(text, hi);
                    }
                } else {
                    let after = hi;
                    while hi < end(text, hi) && class(text, hi, big) == 0 {
                        hi = next(text, hi);
                    }
                    if after == hi && lo > first_nonblank(text, lo) {
                        while lo > start(text, lo) && class(text, prev(text, lo), big) == 0 {
                            lo = prev(text, lo);
                        }
                    }
                }
            }
            if view.mode.is_visual() && view.anchor < at && lo <= view.anchor {
                hi = word_forward(text, hi, 1, big).max(hi);
            }
            Some(ResolvedTarget::character(lo..hi))
        }
        '"' | '\'' | '`' => {
            let s = start(text, at);
            let e = end(text, at);
            let mut quotes = Vec::new();
            let mut escaped = false;
            for (i, c) in text[s..e].char_indices() {
                if escaped {
                    escaped = false;
                    continue;
                }
                if c == '\\' {
                    escaped = true;
                    continue;
                }
                if c == object {
                    quotes.push(s + i);
                }
            }
            let pair = quotes.chunks_exact(2).find(|pair| pair[1] >= at)?;
            let (mut lo, mut hi) = (pair[0], next(text, pair[1]));
            if !around && count == 1 {
                lo = next(text, lo);
                hi = pair[1];
            } else if around {
                let after = hi;
                while hi < e && text[hi..].starts_with([' ', '\t']) {
                    hi = next(text, hi);
                }
                if after == hi {
                    while lo > s && text[prev(text, lo)..lo].starts_with([' ', '\t']) {
                        lo = prev(text, lo);
                    }
                }
            }
            Some(ResolvedTarget::character(lo..hi))
        }
        '(' | ')' | 'b' | '[' | ']' | '{' | '}' | 'B' | '<' | '>' => {
            let (open, close) = match object {
                '(' | ')' | 'b' => ('(', ')'),
                '[' | ']' => ('[', ']'),
                '{' | '}' | 'B' => ('{', '}'),
                _ => ('<', '>'),
            };
            // Find an unmatched opening delimiter backwards, then use the
            // normal forward matching rules from that delimiter.
            let mut stack = Vec::new();
            let mut raw_pairs = Vec::new();
            let mut escaped = false;
            let mut enclosing = Vec::new();
            for (i, c) in text.char_indices() {
                if escaped {
                    escaped = false;
                    continue;
                }
                if c == '\\' {
                    escaped = true;
                    continue;
                }
                if c == open {
                    stack.push(i);
                }
                if i == at {
                    enclosing = stack.clone();
                }
                if c == close {
                    if let Some(a) = stack.pop() {
                        raw_pairs.push((a, i));
                    }
                }
            }
            let pairs = bracket_pairs(text, open, close);
            if enclosing.is_empty() {
                if let Some((i, _)) = text[at..].char_indices().find(|(_, c)| *c == open) {
                    enclosing.push(at + i);
                }
            }
            let lo = *enclosing.iter().rev().nth(count - 1)?;
            let hi = pairs
                .iter()
                .chain(raw_pairs.iter())
                .find(|(a, _)| *a == lo)?
                .1;
            if around {
                return Some(ResolvedTarget::character(lo..next(text, hi)));
            }
            let mut lo = next(text, lo);
            let mut hi = hi;
            if text[lo..].starts_with('\n') {
                lo = next(text, lo);
            }
            if text[start(text, hi)..hi].trim().is_empty() {
                hi = start(text, hi).max(lo);
            }
            if lo < hi && start(text, lo) == lo && start(text, hi) == hi {
                Some(ResolvedTarget::line(lo..hi))
            } else {
                Some(ResolvedTarget::character(lo..hi))
            }
        }
        'p' => {
            let last = row(text, normal_cursor(text, text.len()));
            let blank = |line| {
                let s = line_at(text, line);
                text[s..end(text, s)].trim().is_empty()
            };
            let mut first = row(text, at);
            let mut last_selected = first;
            let empty = blank(first);
            while first > 0 && blank(first - 1) == empty {
                first -= 1;
            }
            while last_selected < last && blank(last_selected + 1) == empty {
                last_selected += 1;
            }
            for _ in 1..count {
                while last_selected < last && blank(last_selected + 1) {
                    last_selected += 1;
                }
                if last_selected < last {
                    last_selected += 1;
                }
                while last_selected < last && !blank(last_selected + 1) {
                    last_selected += 1;
                }
            }
            if around {
                if empty {
                    while last_selected < last && !blank(last_selected + 1) {
                        last_selected += 1;
                    }
                } else if last_selected < last {
                    while last_selected < last && blank(last_selected + 1) {
                        last_selected += 1;
                    }
                } else {
                    while first > 0 && blank(first - 1) {
                        first -= 1;
                    }
                }
            }
            Some(ResolvedTarget::line(line_range(
                text,
                line_at(text, first),
                line_at(text, last_selected),
            )))
        }
        's' => {
            if at == end(text, at) {
                return Some(ResolvedTarget::character(at..next(text, at)));
            }
            let starts = sentence_starts(text);
            let n = starts.iter().rposition(|p| *p <= at).unwrap_or(0);
            let mut lo = starts[n];
            let file_end = end(text, normal_cursor(text, text.len()));
            let mut hi = starts
                .get(n + count)
                .copied()
                .unwrap_or(file_end)
                .min(file_end);
            let trim = |lo: usize, hi: usize| {
                let mut p = hi;
                while p > lo && text[prev(text, p)..p].chars().all(char::is_whitespace) {
                    p = prev(text, p);
                }
                let tail = text[lo..p].trim_end_matches([')', ']', '\'', '"']);
                if tail.ends_with(['.', '!', '?']) || !text[p..hi].contains('\n') {
                    p
                } else {
                    hi
                }
            };
            let mut content_end = trim(lo, hi);
            if at >= content_end && content_end < hi {
                lo = content_end;
                if around {
                    hi = starts
                        .get(n + count + 1)
                        .copied()
                        .unwrap_or(file_end)
                        .min(file_end);
                    content_end = trim(lo, hi);
                }
            } else if !around {
                hi = content_end;
            }
            if around && (hi == content_end || text[content_end..hi].contains('\n')) {
                hi = content_end;
                while lo > start(text, lo)
                    && text[prev(text, lo)..lo]
                        .chars()
                        .all(|c| matches!(c, ' ' | '\t'))
                {
                    lo = prev(text, lo);
                }
            }
            Some(ResolvedTarget::character(lo..hi))
        }
        't' => tag_object(text, at, count, around),
        _ => None,
    }
}

fn tag_object(text: &str, at: usize, count: usize, around: bool) -> Option<ResolvedTarget> {
    let mut stack: Vec<(String, usize, usize)> = Vec::new();
    let mut pairs = Vec::new();
    let mut pos = 0;
    while let Some(i) = text[pos..].find('<') {
        let open = pos + i;
        if text[open..].starts_with("<!--") {
            pos = open + 4 + text[open + 4..].find("-->")? + 3;
            continue;
        }
        let mut quote = None;
        let mut close = None;
        for (i, c) in text[open + 1..].char_indices() {
            if quote == Some(c) {
                quote = None;
            } else if quote.is_none() && matches!(c, '\'' | '"') {
                quote = Some(c);
            } else if quote.is_none() && c == '>' {
                close = Some(open + 1 + i);
                break;
            }
        }
        let Some(close) = close else {
            break;
        };
        pos = close + 1;
        let contents = text[open + 1..close].trim();
        if contents.starts_with(['!', '?']) || contents.ends_with('/') {
            continue;
        }
        let closing = contents.starts_with('/');
        let name = contents
            .trim_start_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if name.is_empty() {
            continue;
        }
        if closing {
            if let Some(n) = stack.iter().rposition(|(tag, _, _)| *tag == name) {
                let (_, start, inner) = stack.remove(n);
                stack.truncate(n);
                if start <= at && at <= close {
                    pairs.push((start, inner, open, close + 1));
                }
            }
        } else if !matches!(
            name.as_str(),
            "area"
                | "base"
                | "br"
                | "col"
                | "embed"
                | "hr"
                | "img"
                | "input"
                | "link"
                | "meta"
                | "param"
                | "source"
                | "track"
                | "wbr"
        ) {
            stack.push((name, open, close + 1));
        }
    }
    pairs.sort_by_key(|(a, _, _, b)| b - a);
    let &(a, inner_a, inner_b, b) = pairs.get(count - 1)?;
    Some(ResolvedTarget::character(if around {
        a..b
    } else {
        inner_a..inner_b
    }))
}
