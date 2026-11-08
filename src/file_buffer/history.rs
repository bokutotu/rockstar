//! Undo branches and buffer-local positions. No editor or filesystem effects.
use std::collections::{BTreeMap, HashMap};

#[derive(Clone)]
struct UndoNode {
    parent: Option<u64>,
    redo: Option<u64>,
    text: String,
    before: usize,
    after: usize,
    removed_marks: HashMap<char, usize>,
}
pub(super) struct UndoTree {
    nodes: BTreeMap<u64, UndoNode>,
    current: u64,
    next: u64,
    restored_marks: HashMap<char, usize>,
}
impl UndoTree {
    pub fn new(text: &str) -> Self {
        Self {
            nodes: BTreeMap::from([(
                0,
                UndoNode {
                    parent: None,
                    redo: None,
                    text: text.into(),
                    before: 0,
                    after: 0,
                    removed_marks: HashMap::new(),
                },
            )]),
            current: 0,
            next: 1,
            restored_marks: HashMap::new(),
        }
    }
    pub fn record(&mut self, text: &str, before: usize, after: usize, join: bool) {
        if join && self.nodes[&self.current].parent.is_some() {
            let node = self.nodes.get_mut(&self.current).unwrap();
            node.text = text.into();
            node.after = after;
            return;
        }
        let id = self.next;
        self.next += 1;
        self.nodes.get_mut(&self.current).unwrap().redo = Some(id);
        self.nodes.insert(
            id,
            UndoNode {
                parent: Some(self.current),
                redo: None,
                text: text.into(),
                before,
                after,
                removed_marks: HashMap::new(),
            },
        );
        self.current = id;
        // Bound snapshots without discarding a recent alternate branch. Old
        // leaves are removed first; a linear root can then be advanced.
        while self.nodes.len() > 1001 {
            let removable = self.nodes.keys().copied().find(|id| {
                *id != self.current && !self.nodes.values().any(|n| n.parent == Some(*id))
            });
            if let Some(id) = removable {
                self.nodes.remove(&id);
                for node in self.nodes.values_mut() {
                    if node.redo == Some(id) {
                        node.redo = None;
                    }
                }
            } else {
                let root = *self.nodes.keys().next().unwrap();
                let children: Vec<_> = self
                    .nodes
                    .iter()
                    .filter(|(_, n)| n.parent == Some(root))
                    .map(|(id, _)| *id)
                    .collect();
                if root == self.current || children.len() != 1 {
                    break;
                }
                self.nodes.remove(&root);
                self.nodes.get_mut(&children[0]).unwrap().parent = None;
            }
        }
    }
    pub fn remember_removed_marks(&mut self, removed: HashMap<char, usize>) {
        let node = self.nodes.get_mut(&self.current).unwrap();
        for (name, at) in removed {
            node.removed_marks.entry(name).or_insert(at);
        }
    }
    pub fn take_restored_marks(&mut self) -> HashMap<char, usize> {
        std::mem::take(&mut self.restored_marks)
    }
    pub fn finish_cursor(&mut self, cursor: usize) {
        if let Some(node) = self.nodes.get_mut(&self.current) {
            node.after = cursor;
        }
    }
    pub fn step(&mut self, redo: bool) -> Option<(String, usize)> {
        let node = self.nodes.get(&self.current)?;
        let (id, cursor) = if redo {
            let id = node.redo?;
            (id, self.nodes.get(&id)?.after)
        } else {
            (node.parent?, node.before)
        };
        self.restored_marks = if redo {
            HashMap::new()
        } else {
            node.removed_marks.clone()
        };
        if !redo {
            self.nodes.get_mut(&id)?.redo = Some(self.current);
        }
        self.current = id;
        Some((self.nodes[&id].text.clone(), cursor))
    }
    pub fn time(&mut self, newer: bool, count: usize) -> Option<(String, usize)> {
        let old = self.current;
        for _ in 0..count {
            let next = if newer {
                self.nodes
                    .range((
                        std::ops::Bound::Excluded(self.current),
                        std::ops::Bound::Unbounded,
                    ))
                    .next()
                    .map(|(id, _)| *id)
            } else {
                self.nodes
                    .range(..self.current)
                    .next_back()
                    .map(|(id, _)| *id)
            };
            let Some(id) = next else {
                break;
            };
            self.current = id;
        }
        if self.current == old {
            return None;
        }
        self.restored_marks.clear();
        let mut ancestors = Vec::new();
        let mut ancestor = Some(self.current);
        while let Some(id) = ancestor {
            ancestors.push(id);
            ancestor = self.nodes[&id].parent;
        }
        let mut discarded = old;
        while !ancestors.contains(&discarded) {
            let node = &self.nodes[&discarded];
            let Some(parent) = node.parent else {
                break;
            };
            for (name, at) in &node.removed_marks {
                let at = map_position(
                    &self.nodes[&parent].text,
                    &self.nodes[&self.current].text,
                    *at,
                );
                self.restored_marks.entry(*name).or_insert(at);
            }
            discarded = parent;
        }
        // Make u / Ctrl-r follow the branch just visited.
        let mut child = self.current;
        while let Some(parent) = self.nodes[&child].parent {
            self.nodes.get_mut(&parent).unwrap().redo = Some(child);
            child = parent;
        }
        let node = &self.nodes[&self.current];
        Some((node.text.clone(), node.after))
    }
}

#[derive(Default)]
pub(crate) struct BufferPositions {
    pub marks: HashMap<char, usize>,
    pub changes: Vec<usize>,
    pub line_undo: Option<(usize, String)>,
}
impl BufferPositions {
    pub fn remap(&mut self, old: &str, new: &str) -> HashMap<char, usize> {
        let (first, old_end, new_end) = changed_range(old, new);
        let mut removed = HashMap::new();
        self.marks.retain(|name, at| {
            let from = line_start(old, *at);
            let to = old[from..].find('\n').map_or(old.len(), |i| from + i + 1);
            if name.is_ascii_alphabetic() && from >= first && to <= old_end && from < to {
                removed.insert(*name, *at);
                false
            } else {
                true
            }
        });
        for at in self.marks.values_mut().chain(self.changes.iter_mut()) {
            *at = mapped(new, *at, first, old_end, new_end);
        }
        if let Some((at, _)) = &mut self.line_undo {
            *at = line_start(new, mapped(new, *at, first, old_end, new_end));
        }
        removed
    }
    pub fn changed(
        &mut self,
        old: &str,
        new: &str,
        cursor: usize,
        after: usize,
        join: bool,
    ) -> HashMap<char, usize> {
        let line = line_start(old, cursor);
        if self.line_undo.as_ref().is_none_or(|(at, _)| *at != line) {
            let end = old[line..].find('\n').map_or(old.len(), |i| line + i);
            self.line_undo = Some((line, old[line..end].into()));
        }
        let removed = self.remap(old, new);
        let cursor = clamp(new, cursor);
        let close = self.changes.last().is_some_and(|last| {
            line_start(new, *last) == line_start(new, cursor) && last.abs_diff(cursor) < 79
        });
        if join || close {
            self.changes.pop();
        }
        self.changes.push(cursor);
        if self.changes.len() > 100 {
            self.changes.remove(0);
        }
        self.marks.insert('.', cursor);
        self.marks.insert('[', cursor);
        self.marks.insert(']', clamp(new, after));
        removed
    }
}
fn clamp(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}
fn line_start(text: &str, offset: usize) -> usize {
    text[..clamp(text, offset)].rfind('\n').map_or(0, |i| i + 1)
}
pub(crate) fn map_position(old: &str, new: &str, offset: usize) -> usize {
    let (first, old_end, new_end) = changed_range(old, new);
    mapped(new, offset, first, old_end, new_end)
}
fn mapped(new: &str, offset: usize, first: usize, old_end: usize, new_end: usize) -> usize {
    clamp(
        new,
        if offset < first {
            offset
        } else if offset >= old_end {
            new_end.saturating_add(offset - old_end)
        } else {
            first
        },
    )
}
fn changed_range(old: &str, new: &str) -> (usize, usize, usize) {
    let first = old
        .chars()
        .zip(new.chars())
        .take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf8())
        .sum::<usize>();
    let suffix = old[first..]
        .chars()
        .rev()
        .zip(new[first..].chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf8())
        .sum::<usize>();
    (first, old.len() - suffix, new.len() - suffix)
}
