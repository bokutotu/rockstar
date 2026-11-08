//! Vim search syntax, independent of the UI. Unsupported context assertions
//! are errors, never silently interpreted as a different regular expression.
use super::*;
use anyhow::{Result, bail, ensure};
use fancy_regex::{Regex, RegexBuilder};

#[derive(Clone, Debug, Default)]
pub(crate) enum SearchOffset {
    #[default]
    None,
    Start(isize),
    End(isize),
    Line(isize),
}
#[derive(Clone, Debug)]
pub(crate) struct SearchSpec {
    pub pattern: String,
    pub forward: bool,
    pub offset: SearchOffset,
}
impl SearchSpec {
    pub fn parse(input: &str, forward: bool, previous: Option<&Self>) -> Result<Self> {
        let delimiter = if forward { '/' } else { '?' };
        let mut escaped = false;
        let split = input.char_indices().find_map(|(i, c)| {
            if escaped {
                escaped = false;
                None
            } else if c == '\\' {
                escaped = true;
                None
            } else {
                (c == delimiter).then_some(i)
            }
        });
        let (pattern, suffix) =
            split.map_or((input, None), |i| (&input[..i], Some(&input[i + 1..])));
        let offset = match suffix {
            None if pattern.is_empty() => previous.map_or(SearchOffset::None, |s| s.offset.clone()),
            None | Some("") => SearchOffset::None,
            Some(s) => {
                let (kind, value) = match s.as_bytes()[0] {
                    b'e' | b's' | b'b' => (s.as_bytes()[0], &s[1..]),
                    _ => (b'l', s),
                };
                let amount = match value {
                    "" => 0,
                    "+" => 1,
                    "-" => -1,
                    _ => value
                        .parse::<isize>()
                        .map_err(|_| anyhow::anyhow!("Invalid search offset: {s}"))?,
                };
                match kind {
                    b'e' => SearchOffset::End(amount),
                    b'l' => SearchOffset::Line(amount),
                    _ => SearchOffset::Start(amount),
                }
            }
        };
        let pattern = if pattern.is_empty() {
            previous
                .ok_or_else(|| anyhow::anyhow!("No previous search pattern"))?
                .pattern
                .clone()
        } else {
            pattern.replace(&format!("\\{delimiter}"), &delimiter.to_string())
        };
        Ok(Self {
            pattern,
            forward,
            offset,
        })
    }
    pub fn destination(&self, text: &str, range: &Range<usize>) -> usize {
        let (mut at, amount) = match self.offset {
            SearchOffset::None => (range.start, 0),
            SearchOffset::Start(n) => (range.start, n),
            SearchOffset::End(n) => (
                if range.is_empty() {
                    range.end
                } else {
                    prev(text, range.end)
                },
                n,
            ),
            SearchOffset::Line(n) => {
                let line = row(text, range.start).saturating_add_signed(n);
                return first_nonblank(text, line_at(text, line));
            }
        };
        for _ in 0..amount.unsigned_abs().min(text.len() + 1) {
            at = if amount < 0 {
                prev(text, at)
            } else {
                next(text, at)
            };
        }
        normal_cursor(text, at)
    }
}

pub(crate) fn choose_match<'a>(
    text: &str,
    ranges: &'a [Range<usize>],
    at: usize,
    forward: bool,
    count: usize,
    include: bool,
) -> Option<&'a Range<usize>> {
    if ranges.is_empty() {
        return None;
    }
    let first = if forward {
        ranges
            .iter()
            .position(|r| {
                normal_cursor(text, r.start) > at
                    || (include
                        && normal_cursor(text, r.start) <= at
                        && (at < r.end || r.start == at))
            })
            .unwrap_or(0)
    } else {
        ranges
            .iter()
            .rposition(|r| {
                normal_cursor(text, r.start) < at || (include && normal_cursor(text, r.start) == at)
            })
            .unwrap_or(ranges.len() - 1)
    };
    let steps = count.saturating_sub(1) % ranges.len();
    let index = if forward {
        (first + steps) % ranges.len()
    } else {
        (first + ranges.len() - steps) % ranges.len()
    };
    ranges.get(index)
}

pub(crate) struct SearchMatcher {
    regex: Regex,
    starts: Vec<String>,
    ends: Vec<String>,
    end_anchor: bool,
}
impl SearchMatcher {
    pub fn compile(pattern: &str) -> Result<Self> {
        ensure!(pattern.len() <= 32_768, "Search pattern is too long");
        let mut p = Translator {
            chars: pattern.chars().collect(),
            i: 0,
            out: String::new(),
            magic: 1,
            insensitive: false,
            atom: 0,
            groups: Vec::new(),
            captures: 0,
            user_groups: Vec::new(),
            starts: Vec::new(),
            ends: Vec::new(),
            end_anchor: false,
            case_sensitive_collection: false,
        };
        p.translate()?;
        ensure!(
            !(p.insensitive && p.case_sensitive_collection),
            "Case overrides in POSIX collections are not supported"
        );
        let regex = RegexBuilder::new(&format!(
            "(?m{})(?:{})(?!\\p{{M}})",
            if p.insensitive { "i" } else { "" },
            p.out
        ))
        .backtrack_limit(1_000_000)
        .build()?;
        Ok(Self {
            regex,
            starts: p.starts,
            ends: p.ends,
            end_anchor: p.end_anchor,
        })
    }
    pub fn navigation_ranges(&self, text: &str) -> Result<Vec<Range<usize>>> {
        let mut ranges = self.ranges(text)?;
        if !self.end_anchor && self.starts.is_empty() {
            ranges.retain(|r| {
                !r.is_empty() || r.start != end(text, r.start) || r.start == start(text, r.start)
            });
        }
        Ok(ranges)
    }
    pub fn ranges(&self, text: &str) -> Result<Vec<Range<usize>>> {
        let mut ranges = Vec::new();
        let text = text.strip_suffix('\n').unwrap_or(text);
        let mut position = 0;
        while position <= text.len() {
            let Some(captures) = self.regex.captures_from_pos(text, position)? else {
                break;
            };
            let whole = captures.get(0).unwrap();
            let from = self
                .starts
                .iter()
                .filter_map(|n| captures.name(n).map(|m| m.start()))
                .next_back()
                .unwrap_or(whole.start());
            let to = self
                .ends
                .iter()
                .filter_map(|n| captures.name(n).map(|m| m.start()))
                .next_back()
                .unwrap_or(whole.end());
            if from <= to {
                ranges.push(from..to);
            }
            ensure!(ranges.len() <= 100_000, "Too many search matches");
            if whole.start() == whole.end() {
                if whole.end() == text.len() {
                    break;
                }
                position = next(text, whole.end());
            } else {
                position = whole.end();
            }
        }
        Ok(ranges)
    }
}
struct Translator {
    chars: Vec<char>,
    i: usize,
    out: String,
    magic: u8,
    insensitive: bool,
    atom: usize,
    groups: Vec<usize>,
    captures: usize,
    user_groups: Vec<usize>,
    starts: Vec<String>,
    ends: Vec<String>,
    end_anchor: bool,
    case_sensitive_collection: bool,
}
impl Translator {
    fn take(&mut self) -> Result<char> {
        let c = self
            .chars
            .get(self.i)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("Incomplete search pattern"))?;
        self.i += 1;
        Ok(c)
    }
    fn literal(&mut self, c: char) {
        self.atom = self.out.len();
        if ".*+?()[]{}|^$\\".contains(c) {
            self.out.push('\\');
        }
        self.out.push(c);
    }
    fn piece(&mut self, s: &str) {
        self.atom = self.out.len();
        if s.starts_with('[') || s == "\\w" || s == "(?s:.)" {
            self.out.push_str(&format!("(?-i:{s}\\p{{M}}*)"));
        } else {
            self.out.push_str(s);
        }
    }
    fn special(&mut self, c: char) -> Result<()> {
        match c {
            '(' => {
                self.atom = self.out.len();
                self.groups.push(self.atom);
                self.captures += 1;
                self.user_groups.push(self.captures);
                self.out.push('(');
            }
            ')' => {
                self.atom = self
                    .groups
                    .pop()
                    .ok_or_else(|| anyhow::anyhow!("Unmatched closing group"))?;
                self.out.push(')');
            }
            '[' => {
                let saved = self.i;
                let mut class = String::from("[");
                let mut first = true;
                let mut newline = false;
                loop {
                    let Some(ch) = self.chars.get(self.i).copied() else {
                        self.i = saved;
                        self.literal('[');
                        return Ok(());
                    };
                    self.i += 1;
                    if ch == ']' && !first {
                        class.push(']');
                        break;
                    }
                    if ch == '[' && matches!(self.chars.get(self.i), Some('=' | '.')) {
                        bail!("Equivalence and collation collections are not supported");
                    }
                    if ch == '[' && self.chars.get(self.i) == Some(&':') {
                        self.i += 1;
                        let mut name = String::new();
                        loop {
                            let part = self.take()?;
                            if part == ':' && self.chars.get(self.i) == Some(&']') {
                                self.i += 1;
                                break;
                            }
                            name.push(part);
                        }
                        self.case_sensitive_collection = true;
                        match name.as_str() {
                            "lower" | "upper" => {
                                self.case_sensitive_collection = true;
                                class.push_str(if name == "lower" {
                                    "\\p{Lowercase}"
                                } else {
                                    "\\p{Uppercase}"
                                });
                            }
                            "return" => class.push_str("\\r"),
                            "tab" => class.push_str("\\t"),
                            "escape" => class.push_str("\\x1b"),
                            "backspace" => class.push_str("\\x08"),
                            "alnum" | "alpha" | "blank" | "cntrl" | "digit" | "graph" | "punct"
                            | "space" | "xdigit" => class.push_str(&format!("[:{name}:]")),
                            _ => bail!("Unsupported POSIX collection: {name}"),
                        }
                        first = false;
                        continue;
                    }
                    if ch == '\\' {
                        let escaped = self.take()?;
                        match escaped {
                            'n' => newline = true,
                            'e' => class.push_str("\\x1b"),
                            'b' => class.push_str("\\x08"),
                            '^' | ']' | '-' | '\\' | 't' | 'r' => {
                                class.push('\\');
                                class.push(escaped);
                            }
                            'd' | 'o' | 'x' | 'u' | 'U' => {
                                let character = self.character_code(escaped)?;
                                class.push_str(&format!("\\x{{{:x}}}", character as u32));
                            }
                            _ => {
                                class.push_str("\\\\");
                                if "[]^-&~".contains(escaped) {
                                    class.push('\\');
                                }
                                class.push(escaped);
                            }
                        }
                    } else {
                        if ch == ']' || ch == '[' || ch == '&' || ch == '~' {
                            class.push('\\');
                        }
                        class.push(ch);
                    }
                    if ch != '^' || class.len() != 2 {
                        first = false;
                    }
                }
                if class == "[]" && newline {
                    self.piece("\\n");
                    return Ok(());
                }
                if class.starts_with("[^") {
                    class.insert_str(2, "\\n");
                }
                self.atom = self.out.len();
                self.out.push_str(&format!(
                    "(?:(?!\\n){class}\\p{{M}}*{})",
                    if newline { "|\\n" } else { "" }
                ));
            }
            '{' => {
                let mut bounds = String::new();
                let mut lazy = false;
                if self.chars.get(self.i) == Some(&'-') {
                    self.i += 1;
                    lazy = true;
                }
                loop {
                    let ch = self.take()?;
                    if ch == '}' {
                        break;
                    }
                    ensure!(ch.is_ascii_digit() || ch == ',', "Invalid repetition count");
                    bounds.push(ch);
                }
                if bounds.is_empty() {
                    self.out.push('*');
                } else {
                    self.out.push('{');
                    if bounds.starts_with(',') {
                        self.out.push('0');
                    }
                    self.out.push_str(&bounds);
                    self.out.push('}');
                }
                if lazy {
                    self.out.push('?');
                }
            }
            '*' if self.out.is_empty()
                || self.out.ends_with('(')
                || self.out.ends_with("(?:")
                || self.out.ends_with('|')
                || self.out.ends_with('^') =>
            {
                self.literal(c)
            }
            '*' | '+' | '?' => self.out.push(c),
            '|' => {
                self.out.push('|');
                self.atom = self.out.len();
            }
            '.' => self.piece("[^\\n]"),
            '^' if !(self.out.is_empty()
                || self.out.ends_with('(')
                || self.out.ends_with("(?:")
                || self.out.ends_with('|')
                || self.out.ends_with("\\n")) =>
            {
                self.literal(c)
            }
            '$' if self.i < self.chars.len()
                && !(self.chars[self.i..].starts_with(&['\\', '|'])
                    || self.chars[self.i..].starts_with(&['\\', ')'])
                    || self.chars[self.i..].starts_with(&['\\', 'n'])
                    || self.magic == 2 && matches!(self.chars[self.i], '|' | ')')) =>
            {
                self.literal(c)
            }
            '^' | '$' => {
                if c == '$' {
                    self.end_anchor = true;
                }
                self.atom = self.out.len();
                self.out.push(c);
            }
            _ => self.literal(c),
        }
        Ok(())
    }
    fn character_code(&mut self, kind: char) -> Result<char> {
        let radix = if kind == 'd' {
            10
        } else if kind == 'o' {
            8
        } else {
            16
        };
        let limit = match kind {
            'x' => 2,
            'u' => 4,
            'U' => 8,
            'o' => 3,
            _ => 8,
        };
        let begin = self.i;
        while self.i < self.chars.len()
            && self.i - begin < limit
            && self.chars[self.i].is_digit(radix)
        {
            self.i += 1;
        }
        let digits: String = self.chars[begin..self.i].iter().collect();
        let value = u32::from_str_radix(&digits, radix)?;
        char::from_u32(value).ok_or_else(|| anyhow::anyhow!("Invalid character code"))
    }
    fn translate(&mut self) -> Result<()> {
        while self.i < self.chars.len() {
            let c = self.take()?;
            if c != '\\' {
                if self.magic == 2 && "%@&~".contains(c) || self.magic == 1 && c == '~' {
                    bail!("Unsupported search atom: {c}");
                }
                if self.magic == 2 && c == '=' {
                    self.special('?')?;
                    continue;
                }
                if self.magic == 2 && matches!(c, '<' | '>') {
                    self.piece(if c == '<' {
                        "(?<!\\w)(?=\\w)"
                    } else {
                        "(?<=\\w)(?!\\w)"
                    });
                    continue;
                }
                let special = self.magic == 2 && ".*+?()[]{}|^$".contains(c)
                    || self.magic == 1 && ".*[^$".contains(c)
                    || self.magic == 0 && "^$".contains(c);
                if special {
                    self.special(c)?;
                } else {
                    self.literal(c);
                }
                continue;
            }
            let c = self.take()?;
            if self.magic == 2 && c.is_ascii_punctuation() && c != '_' && c != '\\' {
                self.literal(c);
                continue;
            }
            match c {
                'v' => self.magic = 2,
                'm' => self.magic = 1,
                'M' => self.magic = 0,
                'V' => self.magic = 3,
                'c' => self.insensitive = true,
                'C' => self.insensitive = false,
                '<' | '>' if self.magic == 2 => self.literal(c),
                '<' => self.piece("(?<!\\w)(?=\\w)"),
                '>' => self.piece("(?<=\\w)(?!\\w)"),
                'n' => self.piece("\\n"),
                'r' => self.piece("\\r"),
                't' => self.piece("\\t"),
                'e' => self.piece("\\x1b"),
                'b' => self.piece("\\x08"),
                'd' => self.piece("[0-9]"),
                'D' => self.piece("[^0-9\\n]"),
                'x' => self.piece("[0-9A-Fa-f]"),
                'X' => self.piece("[^0-9A-Fa-f\\n]"),
                'o' => self.piece("[0-7]"),
                'O' => self.piece("[^0-7\\n]"),
                'w' => self.piece("[0-9A-Za-z_]"),
                'W' => self.piece("[^0-9A-Za-z_\\n]"),
                'h' => self.piece("[A-Za-z_]"),
                'H' => self.piece("[^A-Za-z_\\n]"),
                'a' => self.piece("[A-Za-z]"),
                'A' => self.piece("[^A-Za-z\\n]"),
                'l' => self.piece("[a-z]"),
                'L' => self.piece("[^a-z\\n]"),
                'u' => self.piece("[A-Z]"),
                'U' => self.piece("[^A-Z\\n]"),
                's' => self.piece("[ \\t]"),
                'S' => self.piece("[^ \\t\\n]"),
                'k' | 'i' => self.piece("\\w"),
                'K' | 'I' => self.piece("[^\\W0-9]"),
                '1'..='9' => {
                    let group = *self
                        .user_groups
                        .get(c as usize - '1' as usize)
                        .ok_or_else(|| anyhow::anyhow!("Invalid backreference"))?;
                    self.piece(&format!("\\{group}"));
                }
                'z' => {
                    let kind = self.take()?;
                    ensure!(matches!(kind, 's' | 'e'), "Unsupported \\z assertion");
                    self.captures += 1;
                    let name = format!("vim_{}_{}", kind, self.captures);
                    if kind == 's' {
                        self.starts.push(name.clone());
                    } else {
                        self.ends.push(name.clone());
                    }
                    self.piece(&format!("(?P<{name}>)"));
                }
                '_' => {
                    let c = self.take()?;
                    if c == '$' {
                        self.end_anchor = true;
                    }
                    let pattern = match c {
                        '.' => "(?s:.)",
                        's' => "[ \\t\\n]",
                        'd' => "[0-9\\n]",
                        'w' => "[0-9A-Za-z_\\n]",
                        '^' => "^",
                        '$' => "$",
                        _ => bail!("Unsupported newline-inclusive class: \\_{c}"),
                    };
                    self.piece(pattern);
                }
                '%' => match self.take()? {
                    '(' => {
                        self.atom = self.out.len();
                        self.groups.push(self.atom);
                        self.out.push_str("(?:");
                    }
                    '^' => self.piece("\\A"),
                    '$' => {
                        self.end_anchor = true;
                        self.piece("\\z");
                    }
                    kind @ ('d' | 'o' | 'x' | 'u' | 'U') => {
                        let character = self.character_code(kind)?;
                        self.literal(character);
                    }
                    c => bail!("Context-dependent \\%{c} is not supported"),
                },
                '@' => {
                    let c = self.take()?;
                    let prefix = match c {
                        '=' => "(?=",
                        '!' => "(?!",
                        '>' => "(?>",
                        '<' => match self.take()? {
                            '=' => "(?<=",
                            '!' => "(?<!",
                            _ => bail!("Invalid lookbehind"),
                        },
                        _ => bail!("Invalid lookaround"),
                    };
                    ensure!(self.atom < self.out.len(), "Lookaround needs an atom");
                    let atom = self.out.split_off(self.atom);
                    self.out.push_str(prefix);
                    self.out.push_str(&atom);
                    self.out.push(')');
                }
                '&' | 'f' | 'F' | 'p' | 'P' | 'Z' => bail!("Unsupported search atom: \\{c}"),
                '~' if self.magic == 0 || self.magic == 3 => {
                    bail!("Substitute-history search atoms are not supported")
                }
                '^' | '$' if self.magic == 3 => self.special(c)?,
                '=' if self.magic != 2 => self.special('?')?,
                c if self.magic != 2 && "+?(){}|".contains(c) => self.special(c)?,
                c if (self.magic == 0 || self.magic == 3) && ".*[".contains(c) => {
                    self.special(c)?
                }
                c => self.literal(c),
            }
        }
        ensure!(self.groups.is_empty(), "Unclosed search group");
        Ok(())
    }
}
