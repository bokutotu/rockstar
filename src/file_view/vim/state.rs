use super::*;
use gpui_kit::EntityId;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub(crate) enum ReplayInput {
    Key(String),
    Text(String),
}
impl ReplayInput {
    pub fn text(&self) -> String {
        match self {
            Self::Text(s) => s.clone(),
            Self::Key(s) => match s.as_str() {
                "escape" => "\u{1b}".into(),
                "enter" => "\n".into(),
                "tab" => "\t".into(),
                "backspace" => "\u{08}".into(),
                "space" => " ".into(),
                _ if s.starts_with("ctrl-") && s.len() == 6 => {
                    char::from(s.as_bytes()[5].to_ascii_uppercase() & 31).to_string()
                }
                _ if s.chars().count() == 1 => s.clone(),
                _ => format!("<{s}>"),
            },
        }
    }
}
#[derive(Clone, Debug)]
pub(crate) struct VisualShape {
    pub kind: SelectionKind,
    pub rows: usize,
    pub columns: usize,
    pub end_column: usize,
}
#[derive(Clone, Debug)]
pub(crate) struct RepeatableChange {
    pub command: ParsedCommand,
    pub visual: Option<VisualShape>,
    pub input: Vec<ReplayInput>,
}
pub(crate) struct ReplayFrame {
    pub input: Vec<ReplayInput>,
    pub next: usize,
    pub remaining: usize,
    pub location: Option<BufferLocation>,
}
#[derive(Default)]
pub(crate) struct MacroState {
    pub recording: Option<(char, Vec<ReplayInput>)>,
    pub last_recorded: Option<char>,
    pub last_executed: Option<char>,
    pub frames: Vec<ReplayFrame>,
    pub running: bool,
    pub waiting: bool,
    pub generation: u64,
    pub steps: usize,
    pub location: Option<BufferLocation>,
}
impl MacroState {
    pub fn push(&mut self, input: Vec<ReplayInput>, count: usize) -> Result<(), &'static str> {
        if input.is_empty() {
            return Err("Register is empty");
        }
        if input.len() > 100_000 {
            return Err("Replay input limit exceeded");
        }
        if self
            .frames
            .iter()
            .filter(|frame| frame.location.is_none())
            .count()
            >= 100
        {
            return Err("Macro nesting limit exceeded");
        }
        if !self.running {
            self.generation += 1;
            self.steps = 0;
        }
        self.frames.push(ReplayFrame {
            input,
            next: 0,
            remaining: count.max(1),
            location: None,
        });
        Ok(())
    }
    pub fn next(&mut self) -> Option<ReplayInput> {
        while let Some(frame) = self.frames.last_mut() {
            if let Some(location) = frame.location.take() {
                self.location = Some(location);
                return Some(ReplayInput::Key("escape".into()));
            }
            if frame.next == frame.input.len() {
                frame.remaining -= 1;
                if frame.remaining == 0 {
                    self.frames.pop();
                    continue;
                }
                frame.next = 0;
            }
            let input = frame.input[frame.next].clone();
            frame.next += 1;
            self.steps += 1;
            return Some(input);
        }
        None
    }
    pub fn stop(&mut self) {
        self.frames.clear();
        self.location = None;
        self.waiting = false;
        self.running = false;
        self.generation += 1;
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BufferLocation {
    pub buffer: EntityId,
    pub path: PathBuf,
    pub line: usize,
    pub column: usize,
}
impl BufferLocation {
    pub fn new(buffer: EntityId, path: PathBuf, text: &str, at: usize) -> Self {
        let at = boundary(text, at);
        Self {
            buffer,
            path,
            line: row(text, at),
            column: at - start(text, at),
        }
    }
    pub fn offset(&self, text: &str) -> usize {
        let line = line_at(text, self.line);
        boundary(text, (line + self.column).min(end(text, line)))
    }
    pub fn remap(&mut self, buffer: EntityId, old: &str, new: &str) {
        if self.buffer != buffer {
            return;
        }
        let at = crate::file_buffer::map_position(old, new, self.offset(old));
        self.line = row(new, at);
        self.column = at - start(new, at);
    }
}
#[derive(Clone, Default)]
pub(crate) struct JumpList {
    pub entries: Vec<BufferLocation>,
    pub index: usize,
    pub previous: Option<BufferLocation>,
    pub tags: Vec<BufferLocation>,
    pub tag_index: usize,
}
impl JumpList {
    pub fn push_tag(&mut self, location: BufferLocation) {
        self.tags.truncate(self.tag_index);
        self.tags.push(location);
        if self.tags.len() > 20 {
            self.tags.remove(0);
        }
        self.tag_index = self.tags.len();
    }
    pub fn pop_tag(&mut self, count: usize) -> Option<BufferLocation> {
        if self.tag_index == 0 {
            return None;
        }
        self.tag_index = self.tag_index.saturating_sub(count);
        self.tags.get(self.tag_index).cloned()
    }
    pub fn push(&mut self, location: BufferLocation) {
        self.previous = Some(location.clone());
        self.entries
            .retain(|old| old.buffer != location.buffer || old.line != location.line);
        self.entries.push(location);
        if self.entries.len() > 100 {
            self.entries.remove(0);
        }
        self.index = self.entries.len();
    }
    pub fn step(
        &mut self,
        current: BufferLocation,
        newer: bool,
        count: usize,
    ) -> Option<BufferLocation> {
        if self.index == self.entries.len() {
            if newer || self.entries.is_empty() {
                return None;
            }
            self.entries.push(current);
        }
        let index = if newer {
            (self.index + count).min(self.entries.len().saturating_sub(1))
        } else {
            self.index.saturating_sub(count)
        };
        if index == self.index {
            return None;
        }
        self.index = index;
        self.entries.get(index).cloned()
    }
    pub fn remap(&mut self, buffer: EntityId, old: &str, new: &str) {
        for location in self
            .entries
            .iter_mut()
            .chain(self.previous.iter_mut())
            .chain(self.tags.iter_mut())
        {
            location.remap(buffer, old, new);
        }
    }
}
