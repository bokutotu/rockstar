//! File-view editing core. Text positions and edit ranges use UTF-8 byte offsets.
use crate::mode::Mode;
use std::{collections::HashMap, ops::Range};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

mod edit;
mod motion;
mod object;
mod search;
mod state;
pub(super) use edit::*;
pub(super) use motion::*;
pub(crate) use search::*;
pub(crate) use state::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SelectionKind {
    Character,
    Line,
    Block,
}

#[derive(Clone, Debug)]
pub(crate) struct VisualSelection {
    pub kind: SelectionKind,
    pub anchor: usize,
    pub cursor: usize,
    pub columns: (usize, usize),
}

#[derive(Clone)]
pub(crate) struct VimViewState {
    pub mode: Mode,
    pub pending: PendingCommand,
    pub cursor: usize,
    pub anchor: usize,
    pub column: Option<usize>,
    pub block_columns: (usize, usize),
    pub last_visual: Option<VisualSelection>,
}
impl Default for VimViewState {
    fn default() -> Self {
        Self {
            mode: Mode::Normal,
            pending: PendingCommand::default(),
            cursor: 0,
            anchor: 0,
            column: None,
            block_columns: (0, 0),
            last_visual: None,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum RegisterValue {
    Character(String),
    Line(String),
    Block { rows: Vec<String>, width: usize },
    Keys(Vec<ReplayInput>),
}
impl RegisterValue {
    pub fn text(&self) -> String {
        match self {
            Self::Character(s) | Self::Line(s) => s.clone(),
            Self::Block { rows, .. } => rows.join("\n"),
            Self::Keys(keys) => keys.iter().map(ReplayInput::text).collect(),
        }
    }
    pub fn append(&mut self, other: Self) {
        *self = match (&*self, &other) {
            (Self::Keys(a), Self::Keys(b)) => Self::Keys(a.iter().chain(b).cloned().collect()),
            (Self::Keys(a), _) => Self::Keys(
                a.iter()
                    .cloned()
                    .chain([ReplayInput::Text(other.text())])
                    .collect(),
            ),
            (_, Self::Keys(b)) => Self::Keys(
                std::iter::once(ReplayInput::Text(self.text()))
                    .chain(b.iter().cloned())
                    .collect(),
            ),
            (Self::Line(a), Self::Line(b)) => Self::Line(format!("{a}{b}")),
            (Self::Block { rows: a, width: aw }, Self::Block { rows: b, width: bw }) => {
                let rows = (0..a.len().max(b.len()))
                    .map(|i| {
                        format!(
                            "{}{}",
                            a.get(i).map_or("", String::as_str),
                            b.get(i).map_or("", String::as_str)
                        )
                    })
                    .collect();
                Self::Block {
                    rows,
                    width: aw + bw,
                }
            }
            _ => Self::Character(format!("{}{}", self.text(), other.text())),
        };
    }
}

#[derive(Default)]
pub(crate) struct VimSharedState {
    pub registers: HashMap<char, RegisterValue>,
    pub last_find: Option<CharFind>,
    pub search: Option<SearchSpec>,
    pub search_history: Vec<String>,
    pub last_change: Option<RepeatableChange>,
    pub last_insert: String,
    pub macros: MacroState,
    pub marks: HashMap<char, BufferLocation>,
}
impl VimSharedState {
    /// The unnamed register aliases the result, not the system clipboard.
    pub fn store(
        &mut self,
        register: Option<char>,
        value: RegisterValue,
        yank: bool,
        large_delete: bool,
    ) {
        if register == Some('_') {
            return;
        }
        let mut unnamed = value.clone();
        if let Some(name) = register.filter(|c| *c != '"') {
            let key = name.to_ascii_lowercase();
            if name.is_ascii_uppercase() {
                self.registers
                    .entry(key)
                    .and_modify(|old| old.append(value.clone()))
                    .or_insert(value);
                unnamed = self.registers[&key].clone();
            } else {
                self.registers.insert(key, value);
            }
        } else if yank {
            self.registers.insert('0', value);
        } else if large_delete {
            for n in (2..=9).rev() {
                let key = char::from(b'0' + n);
                if let Some(previous) = self.registers.get(&char::from(b'0' + n - 1)).cloned() {
                    self.registers.insert(key, previous);
                } else {
                    self.registers.remove(&key);
                }
            }
            self.registers.insert('1', value);
        } else {
            self.registers.insert('-', value);
        }
        self.registers.insert('"', unnamed);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Operator {
    Delete,
    Change,
    Yank,
    Indent,
    Outdent,
    Lower,
    Upper,
    Swap,
    Rot13,
    Reindent,
    Format,
    FormatKeep,
    Comment,
    Fold,
}
impl Operator {
    fn key(self) -> &'static str {
        match self {
            Self::Delete => "d",
            Self::Change => "c",
            Self::Yank => "y",
            Self::Indent => ">",
            Self::Outdent => "<",
            Self::Lower => "u",
            Self::Upper => "U",
            Self::Swap => "~",
            Self::Rot13 => "?",
            Self::Reindent => "=",
            Self::Format => "q",
            Self::FormatKeep => "w",
            Self::Comment => "c",
            Self::Fold => "f",
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct CharFind {
    pub character: char,
    pub forward: bool,
    pub till: bool,
}
#[derive(Clone, Debug)]
pub(crate) enum Motion {
    Key(String),
    Find(CharFind),
    RepeatFind(bool),
}
#[derive(Clone, Debug)]
pub(crate) enum Target {
    Motion(Motion),
    TextObject {
        object: char,
        around: bool,
    },
    Search {
        spec: Option<SearchSpec>,
        reverse: bool,
        select: bool,
    },
    WordSearch {
        forward: bool,
        whole: bool,
    },
    Mark {
        name: char,
        linewise: bool,
        jump: bool,
    },
    Lines,
}
#[derive(Clone, Debug)]
pub(crate) struct ResolvedTarget {
    pub destination: usize,
    pub ranges: Vec<Range<usize>>,
    pub kind: SelectionKind,
    pub block_columns: Option<Range<usize>>,
}
impl ResolvedTarget {
    pub fn character(range: Range<usize>) -> Self {
        Self {
            destination: range.start,
            ranges: vec![range],
            kind: SelectionKind::Character,
            block_columns: None,
        }
    }
    pub fn line(range: Range<usize>) -> Self {
        Self {
            destination: range.start,
            ranges: vec![range],
            kind: SelectionKind::Line,
            block_columns: None,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Target(Target),
    Operate(Operator, Option<Target>),
    Insert(&'static str),
    Visual(SelectionKind),
    Replace(char),
    Paste { before: bool, after: bool },
    Join(bool),
    SwapChars,
    DeleteChars(bool),
    LineEnd(Operator),
    ChangeLines,
    Undo,
    Redo,
    Search(bool, Option<Operator>),
    Repeat,
    Record(char),
    Replay(char),
    ReplayRecorded,
    SetMark(char),
    Jump(bool),
    ChangeList(bool),
    InsertLast,
    UndoTime(bool),
    UndoLine,
    Integration(String),
    ExchangeEnds(bool),
    Reselect,
    Scroll(String),
    Number(bool),
    Save,
}
#[derive(Clone, Debug)]
pub(crate) struct ParsedCommand {
    pub action: Action,
    pub count: usize,
    pub explicit_count: bool,
    pub register: Option<char>,
    pub forced: Option<SelectionKind>,
}
#[derive(Clone, Debug, Default)]
enum Awaiting {
    #[default]
    Command,
    Register,
    Prefix(String),
    Find {
        forward: bool,
        till: bool,
    },
    Object(bool),
    Replace,
    Mark {
        set: bool,
        linewise: bool,
        jump: bool,
    },
    Record,
    Replay,
}
#[derive(Clone, Debug, Default)]
pub(crate) struct PendingCommand {
    count: Option<usize>,
    operator: Option<Operator>,
    motion_count: Option<usize>,
    register: Option<char>,
    forced: Option<SelectionKind>,
    awaiting: Awaiting,
}
impl PendingCommand {
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    pub fn is_empty(&self) -> bool {
        self.count.is_none()
            && self.operator.is_none()
            && self.register.is_none()
            && matches!(self.awaiting, Awaiting::Command)
    }
    pub fn count(&self) -> usize {
        self.count
            .unwrap_or(1)
            .saturating_mul(self.motion_count.unwrap_or(1))
            .clamp(1, 100_000)
    }
    fn finish(&mut self, action: Action) -> Option<ParsedCommand> {
        let command = ParsedCommand {
            action,
            count: self.count(),
            explicit_count: self.count.is_some() || self.motion_count.is_some(),
            register: self.register,
            forced: self.forced,
        };
        self.clear();
        Some(command)
    }
    fn target(&mut self, target: Target) -> Option<ParsedCommand> {
        let action = self.operator.map_or_else(
            || Action::Target(target.clone()),
            |op| Action::Operate(op, Some(target.clone())),
        );
        self.finish(action)
    }
    fn operator(&mut self, operator: Operator, visual: bool) -> Option<ParsedCommand> {
        if visual {
            self.finish(Action::Operate(operator, None))
        } else if self.operator == Some(operator) {
            self.target(Target::Lines)
        } else if self.operator.is_some() {
            self.clear();
            None
        } else {
            self.operator = Some(operator);
            None
        }
    }
    pub fn feed(&mut self, key: &str, visual: bool) -> Option<ParsedCommand> {
        let waiting = std::mem::take(&mut self.awaiting);
        match waiting {
            Awaiting::Mark {
                set,
                linewise,
                jump,
            } => {
                if let Some(name) = single_char(key) {
                    return if set {
                        self.finish(Action::SetMark(name))
                    } else {
                        self.target(Target::Mark {
                            name,
                            linewise,
                            jump,
                        })
                    };
                }
                self.clear();
                return None;
            }
            Awaiting::Record | Awaiting::Replay => {
                if let Some(name) = single_char(key) {
                    return self.finish(if matches!(waiting, Awaiting::Record) {
                        Action::Record(name)
                    } else {
                        Action::Replay(name)
                    });
                }
                self.clear();
                return None;
            }
            Awaiting::Register => {
                if let Some(c) = single_char(key)
                    .filter(|c| c.is_ascii_alphanumeric() || "\"-+*_./:%#=".contains(*c))
                {
                    self.register = Some(c);
                } else {
                    self.clear();
                }
                return None;
            }
            Awaiting::Find { forward, till } => {
                if let Some(character) = single_char(key) {
                    return self.target(Target::Motion(Motion::Find(CharFind {
                        character,
                        forward,
                        till,
                    })));
                }
                self.clear();
                return None;
            }
            Awaiting::Object(around) => {
                if let Some(object) =
                    single_char(key).filter(|c| "wWsp\"'`()[]{}<>bBt".contains(*c))
                {
                    return self.target(Target::TextObject { object, around });
                }
                self.clear();
                return None;
            }
            Awaiting::Replace => {
                if let Some(c) = single_char(key).or_else(|| (key == "enter").then_some('\n')) {
                    return self.finish(Action::Replace(c));
                }
                self.clear();
                return None;
            }
            Awaiting::Prefix(prefix) => {
                if prefix == "ctrl-w"
                    && let Some(digit) = single_char(key).and_then(|c| c.to_digit(10))
                {
                    self.count = Some(
                        self.count
                            .unwrap_or(0)
                            .saturating_mul(10)
                            .saturating_add(digit as usize)
                            .min(100_000),
                    );
                    self.awaiting = Awaiting::Prefix(prefix);
                    return None;
                }
                let joined = format!("{prefix}{key}");
                if self.operator.is_some()
                    && matches!(
                        joined.as_str(),
                        "gI" | "gR" | "gJ" | "gp" | "gP" | "gv" | " w"
                    )
                {
                    self.clear();
                    return None;
                }
                match joined.as_str() {
                    "g'" | "g`" => {
                        self.awaiting = Awaiting::Mark {
                            set: false,
                            linewise: key == "'",
                            jump: false,
                        };
                        return None;
                    }
                    "gr" => {
                        self.awaiting = Awaiting::Prefix(joined);
                        return None;
                    }
                    "g*" | "g#" => {
                        return self.target(Target::WordSearch {
                            forward: key == "*",
                            whole: false,
                        });
                    }
                    "gn" | "gN" => {
                        return self.target(Target::Search {
                            spec: None,
                            reverse: key == "N",
                            select: true,
                        });
                    }
                    "g-" | "g+" => return self.finish(Action::UndoTime(key == "+")),
                    "g;" | "g," => return self.finish(Action::ChangeList(key == ",")),
                    "gi" => return self.finish(Action::InsertLast),
                    "gq" => return self.operator(Operator::Format, visual),
                    "gw" => return self.operator(Operator::FormatKeep, visual),
                    "gc" => return self.operator(Operator::Comment, visual),
                    "zf" => return self.operator(Operator::Fold, visual),
                    "zF" => {
                        return self.finish(Action::Operate(Operator::Fold, Some(Target::Lines)));
                    }
                    "zj" | "zk" => return self.finish(Action::Integration(joined)),
                    "gra" | "gri" | "grn" | "grr" | "grt" | "gO" | "gd" | "gD" | "gf" | "gF"
                    | "gt" | "gT" | "[d" | "]d" | "[D" | "]D" | "za" | "zA" | "zo" | "zO"
                    | "zc" | "zC" | "zd" | "zD" | "zE" | "zM" | "zR" | "zm" | "zr" | "zn"
                    | "zN" | "zi" => return self.finish(Action::Integration(joined)),
                    _ if prefix == "ctrl-w" => return self.finish(Action::Integration(joined)),
                    " w" => return self.finish(Action::Save),
                    "gu" => return self.operator(Operator::Lower, visual),
                    "gU" => return self.operator(Operator::Upper, visual),
                    "g~" => return self.operator(Operator::Swap, visual),
                    "g?" => return self.operator(Operator::Rot13, visual),
                    "gI" => return self.finish(Action::Insert("gI")),
                    "gR" => return self.finish(Action::Insert("gR")),
                    "gJ" => return self.finish(Action::Join(false)),
                    "gp" | "gP" => {
                        return self.finish(Action::Paste {
                            before: key == "P",
                            after: true,
                        });
                    }
                    "gv" => return self.finish(Action::Reselect),
                    "gg" | "ge" | "gE" | "g_" | "g0" | "g^" | "g$" | "gj" | "gk" | "gm" | "gM"
                    | "go" | "[[" | "]]" | "[]" | "][" | "[(" | "])" | "[{" | "]}" | "[m"
                    | "]m" | "[M" | "]M" | "[/" | "]/" | "[*" | "]*" | "[#" | "]#" => {
                        return self.target(Target::Motion(Motion::Key(joined)));
                    }
                    "zt" | "zz" | "zb" | "zenter" | "z." | "z-" | "zh" | "zl" | "zH" | "zL"
                    | "zs" | "ze" => return self.finish(Action::Scroll(joined)),
                    _ => {
                        self.clear();
                        return None;
                    }
                }
            }
            Awaiting::Command => {}
        }
        if let Some(digit) = single_char(key)
            .and_then(|c| c.to_digit(10))
            .filter(|_| key.is_ascii())
        {
            let count = if self.operator.is_some() {
                &mut self.motion_count
            } else {
                &mut self.count
            };
            if digit != 0 || count.is_some() {
                *count = Some(
                    count
                        .unwrap_or(0)
                        .saturating_mul(10)
                        .saturating_add(digit as usize)
                        .min(100_000),
                );
                return None;
            }
        }
        if self.operator.is_some_and(|op| key == op.key()) {
            return self.target(Target::Lines);
        }
        match key {
            "\"" => {
                self.awaiting = Awaiting::Register;
                None
            }
            "g" | "[" | "]" | "z" | "ctrl-w" => {
                self.awaiting = Awaiting::Prefix(key.into());
                None
            }
            " " if self.operator.is_none() && !visual => {
                self.awaiting = Awaiting::Prefix(" ".into());
                None
            }
            "f" | "F" | "t" | "T" => {
                self.awaiting = Awaiting::Find {
                    forward: matches!(key, "f" | "t"),
                    till: matches!(key, "t" | "T"),
                };
                None
            }
            ";" | "," => self.target(Target::Motion(Motion::RepeatFind(key == ";"))),
            "'" | "`" | "m" => {
                self.awaiting = Awaiting::Mark {
                    set: key == "m",
                    linewise: key == "'",
                    jump: true,
                };
                None
            }
            "/" | "?" => self.finish(Action::Search(key == "/", self.operator)),
            "n" | "N" => self.target(Target::Search {
                spec: None,
                reverse: key == "N",
                select: false,
            }),
            "*" | "#" => self.target(Target::WordSearch {
                forward: key == "*",
                whole: true,
            }),
            "i" | "a" if visual || self.operator.is_some() => {
                self.awaiting = Awaiting::Object(key == "a");
                None
            }
            "h" | "j" | "k" | "l" | "left" | "down" | "up" | "right" | "home" | "end" | "0"
            | "^" | "$" | "w" | "W" | "b" | "B" | "e" | "E" | "G" | "%" | "{" | "}" | "(" | ")"
            | "+" | "-" | "_" | "|" | "H" | "M" | "L" | "enter" | "backspace" | " " => {
                self.target(Target::Motion(Motion::Key(key.into())))
            }
            "v" | "V" | "ctrl-v" => {
                let kind = match key {
                    "v" => SelectionKind::Character,
                    "V" => SelectionKind::Line,
                    _ => SelectionKind::Block,
                };
                if self.operator.is_some() {
                    self.forced = Some(kind);
                    None
                } else {
                    self.finish(Action::Visual(kind))
                }
            }
            "d" => self.operator(Operator::Delete, visual),
            "c" => self.operator(Operator::Change, visual),
            "y" => self.operator(Operator::Yank, visual),
            ">" => self.operator(Operator::Indent, visual),
            "<" => self.operator(Operator::Outdent, visual),
            "=" => self.operator(Operator::Reindent, visual),
            _ if self.operator.is_some() => {
                self.clear();
                None
            }
            "i" | "insert" => self.finish(Action::Insert("i")),
            "a" => self.finish(Action::Insert("a")),
            "I" => self.finish(Action::Insert("I")),
            "A" => self.finish(Action::Insert("A")),
            "R" if !visual => self.finish(Action::Insert("R")),
            "o" | "O" if visual => self.finish(Action::ExchangeEnds(key == "O")),
            "o" => self.finish(Action::Insert("o")),
            "O" => self.finish(Action::Insert("O")),
            "r" => {
                self.awaiting = Awaiting::Replace;
                None
            }
            "x" | "delete" if visual => self.finish(Action::Operate(Operator::Delete, None)),
            "s" if visual => self.finish(Action::Operate(Operator::Change, None)),
            "x" | "delete" | "X" => self.finish(Action::DeleteChars(key == "X")),
            "D" => self.finish(Action::LineEnd(Operator::Delete)),
            "C" => self.finish(Action::LineEnd(Operator::Change)),
            "Y" => self.finish(Action::LineEnd(Operator::Yank)),
            "S" | "R" => self.finish(Action::ChangeLines),
            "p" | "P" => self.finish(Action::Paste {
                before: key == "P",
                after: false,
            }),
            "J" => self.finish(Action::Join(true)),
            "~" if visual => self.finish(Action::Operate(Operator::Swap, None)),
            "~" => self.finish(Action::SwapChars),
            "u" if visual => self.finish(Action::Operate(Operator::Lower, None)),
            "U" if visual => self.finish(Action::Operate(Operator::Upper, None)),
            "u" => self.finish(Action::Undo),
            "ctrl-r" => self.finish(Action::Redo),
            "." => self.finish(Action::Repeat),
            "q" => {
                self.awaiting = Awaiting::Record;
                None
            }
            "@" => {
                self.awaiting = Awaiting::Replay;
                None
            }
            "Q" => self.finish(Action::ReplayRecorded),
            "U" => self.finish(Action::UndoLine),
            "ctrl-o" | "ctrl-i" | "tab" => self.finish(Action::Jump(key != "ctrl-o")),
            "ctrl-]" | "ctrl-t" | "ctrl-^" | "ctrl-6" => {
                self.finish(Action::Integration(key.into()))
            }
            "ctrl-a" | "ctrl-x" => self.finish(Action::Number(key == "ctrl-a")),
            "ctrl-d" | "ctrl-u" | "ctrl-f" | "ctrl-b" | "ctrl-e" | "ctrl-y" | "pageup"
            | "pagedown" => self.finish(Action::Scroll(key.into())),
            _ => {
                self.clear();
                None
            }
        }
    }
}
fn single_char(s: &str) -> Option<char> {
    let mut chars = s.chars();
    let c = chars.next()?;
    chars.next().is_none().then_some(c)
}

pub(crate) fn boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}
pub(crate) fn prev(text: &str, at: usize) -> usize {
    text[..boundary(text, at)]
        .grapheme_indices(true)
        .next_back()
        .map_or(0, |(i, _)| i)
}
pub(crate) fn next(text: &str, at: usize) -> usize {
    let at = boundary(text, at);
    at + text[at..].graphemes(true).next().map_or(0, str::len)
}
pub(crate) fn start(text: &str, at: usize) -> usize {
    text[..boundary(text, at)].rfind('\n').map_or(0, |i| i + 1)
}
pub(crate) fn end(text: &str, at: usize) -> usize {
    let at = boundary(text, at);
    text[at..].find('\n').map_or(text.len(), |i| at + i)
}
pub(crate) fn normal_cursor(text: &str, at: usize) -> usize {
    let mut at = boundary(text, at);
    if at == text.len() && text.ends_with('\n') {
        at = prev(text, at);
    }
    if end(text, at) > start(text, at) && at == end(text, at) {
        prev(text, at)
    } else {
        at
    }
}
pub(crate) fn first_nonblank(text: &str, at: usize) -> usize {
    let s = start(text, at);
    text[s..end(text, at)]
        .char_indices()
        .find(|(_, c)| !c.is_whitespace())
        .map_or(s, |(i, _)| s + i)
}
pub(crate) fn line_range(text: &str, a: usize, b: usize) -> Range<usize> {
    start(text, a.min(b))..next(text, end(text, a.max(b)))
}
pub(crate) fn line_at(text: &str, line: usize) -> usize {
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
pub(crate) fn row(text: &str, at: usize) -> usize {
    text[..boundary(text, at)]
        .bytes()
        .filter(|b| *b == b'\n')
        .count()
}
pub(crate) fn column(text: &str, at: usize) -> usize {
    text[start(text, at)..boundary(text, at)]
        .graphemes(true)
        .count()
}
pub(crate) fn cell_width(grapheme: &str, col: usize, tab: usize) -> usize {
    if grapheme == "\t" {
        tab - col % tab
    } else {
        grapheme.width().max(1)
    }
}
pub(crate) fn display_column(text: &str, at: usize, tab: usize) -> usize {
    text[start(text, at)..boundary(text, at)]
        .graphemes(true)
        .fold(0, |col, g| col + cell_width(g, col, tab))
}
pub(crate) fn at_column(text: &str, line: usize, target: usize, tab: usize) -> usize {
    let mut col = 0;
    for (i, g) in text[line..end(text, line)].grapheme_indices(true) {
        let width = cell_width(g, col, tab);
        if col + width > target {
            return line + i;
        }
        col += width;
    }
    end(text, line)
}
fn class(text: &str, at: usize, big: bool) -> u8 {
    match text[boundary(text, at)..].chars().next() {
        None => 0,
        Some(c) if c.is_whitespace() => 0,
        Some(_) if big => 1,
        Some('\u{3040}'..='\u{309f}') => 3,
        Some('\u{30a0}'..='\u{30ff}' | '\u{ff66}'..='\u{ff9f}') => 4,
        Some(
            '\u{3400}'..='\u{4dbf}'
            | '\u{4e00}'..='\u{9fff}'
            | '\u{f900}'..='\u{faff}'
            | '\u{20000}'..='\u{323af}',
        ) => 5,
        Some('\u{ac00}'..='\u{d7af}') => 6,
        Some(c) if c.is_alphanumeric() || c == '_' => 1,
        _ => 2,
    }
}
