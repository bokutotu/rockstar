//! UTF-8-safe motions used by the file view. All offsets are byte offsets.
use crate::mode::Mode;
use std::ops::Range;

pub(super) struct VimState {
    pub mode: Mode,
    pub pending: String,
    pub cursor: usize,
    pub anchor: usize,
    pub column: Option<usize>,
}

impl Default for VimState {
    fn default() -> Self {
        Self {
            mode: Mode::Normal,
            pending: String::new(),
            cursor: 0,
            anchor: 0,
            column: None,
        }
    }
}

pub(super) fn boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}
pub(super) fn prev(text: &str, at: usize) -> usize {
    text[..boundary(text, at)]
        .char_indices()
        .next_back()
        .map_or(0, |(i, _)| i)
}
pub(super) fn next(text: &str, at: usize) -> usize {
    let at = boundary(text, at);
    at + text[at..].chars().next().map_or(0, char::len_utf8)
}
pub(super) fn start(text: &str, at: usize) -> usize {
    text[..boundary(text, at)].rfind('\n').map_or(0, |i| i + 1)
}
pub(super) fn end(text: &str, at: usize) -> usize {
    let at = boundary(text, at);
    text[at..].find('\n').map_or(text.len(), |i| at + i)
}
pub(super) fn normal_cursor(text: &str, at: usize) -> usize {
    let mut at = boundary(text, at);
    // A terminating newline belongs to the last line; it is not an extra Vim line.
    if at == text.len() && text.ends_with('\n') {
        at = prev(text, at);
    }
    let s = start(text, at);
    let e = end(text, at);
    if e > s && at == e { prev(text, at) } else { at }
}
pub(super) fn first_nonblank(text: &str, at: usize) -> usize {
    let s = start(text, at);
    text[s..end(text, at)]
        .char_indices()
        .find(|(_, c)| !c.is_whitespace())
        .map_or(s, |(i, _)| s + i)
}
pub(super) fn line_range(text: &str, a: usize, b: usize) -> Range<usize> {
    start(text, a.min(b))..next(text, end(text, a.max(b)))
}
pub(super) fn line_at(text: &str, line: usize) -> usize {
    if line == 0 {
        return 0;
    }
    let last = start(
        text,
        text.len().saturating_sub(usize::from(text.ends_with('\n'))),
    );
    text.match_indices('\n')
        .nth(line - 1)
        .map_or(last, |(i, _)| (i + 1).min(last))
}
pub(super) fn row(text: &str, at: usize) -> usize {
    text[..boundary(text, at)]
        .bytes()
        .filter(|b| *b == b'\n')
        .count()
}
pub(super) fn column(text: &str, at: usize) -> usize {
    text[start(text, at)..boundary(text, at)].chars().count()
}
fn class(c: char) -> u8 {
    if c.is_whitespace() {
        0
    } else if c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}
fn char_class(text: &str, at: usize) -> u8 {
    text[at..].chars().next().map_or(0, class)
}

pub(super) fn word_forward(text: &str, mut at: usize, count: usize) -> usize {
    for _ in 0..count {
        let c = char_class(text, at);
        while at < text.len() && char_class(text, at) == c {
            at = next(text, at);
        }
        while at < text.len() && char_class(text, at) == 0 {
            at = next(text, at);
        }
    }
    at
}

pub(super) fn motion(
    text: &str,
    cursor: usize,
    key: &str,
    count: usize,
    desired_column: &mut Option<usize>,
    explicit_count: bool,
) -> Option<usize> {
    let count = count.clamp(1, 100_000);
    let mut at = boundary(text, cursor);
    match key {
        "j" | "down" | "k" | "up" => {
            let column = *desired_column.get_or_insert_with(|| column(text, at));
            let current = row(text, at);
            let target = if matches!(key, "j" | "down") {
                current.saturating_add(count)
            } else {
                current.saturating_sub(count)
            };
            let s = line_at(text, target);
            let e = end(text, s);
            at = text[s..e]
                .char_indices()
                .nth(column)
                .map_or(e, |(i, _)| s + i);
        }
        "h" | "left" => {
            for _ in 0..count {
                if at > start(text, at) {
                    at = prev(text, at);
                }
            }
        }
        "l" | "right" => {
            for _ in 0..count {
                at = next(text, at).min(end(text, at));
            }
        }
        "0" | "home" => at = start(text, at),
        "^" => at = first_nonblank(text, at),
        "$" | "end" => {
            at = end(text, line_at(text, row(text, at).saturating_add(count - 1)));
        }
        "gg" => at = first_nonblank(text, line_at(text, count - 1)),
        "G" => {
            at = first_nonblank(
                text,
                if explicit_count {
                    line_at(text, count - 1)
                } else {
                    start(
                        text,
                        text.len().saturating_sub(usize::from(text.ends_with('\n'))),
                    )
                },
            )
        }
        "w" => at = word_forward(text, at, count),
        "b" => {
            for _ in 0..count {
                at = prev(text, at);
                while at > 0 && char_class(text, at) == 0 {
                    at = prev(text, at);
                }
                let c = char_class(text, at);
                while at > 0 && char_class(text, prev(text, at)) == c {
                    at = prev(text, at);
                }
            }
        }
        "e" => {
            for _ in 0..count {
                at = next(text, at);
                while at < text.len() && char_class(text, at) == 0 {
                    at = next(text, at);
                }
                let c = char_class(text, at);
                while next(text, at) < text.len() && char_class(text, next(text, at)) == c {
                    at = next(text, at);
                }
            }
        }
        _ => return None,
    }
    if !matches!(key, "j" | "k" | "up" | "down") {
        *desired_column = None;
    }
    Some(normal_cursor(text, at))
}

pub(super) fn count_prefix(command: &str) -> (usize, &str, bool) {
    let digits = command.chars().take_while(|c| c.is_ascii_digit()).count();
    if command.starts_with('0') || digits == 0 {
        (1, command, false)
    } else {
        (
            command[..digits]
                .parse::<usize>()
                .unwrap_or(100_000)
                .clamp(1, 100_000),
            &command[digits..],
            true,
        )
    }
}
