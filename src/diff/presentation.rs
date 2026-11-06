//! Full-source parsing and row alignment, prepared off the UI thread.
use super::model::{DiffDocument, DiffEntry};
use crate::syntax::language_settings;
use gpui_kit::{
    HighlightStyle, SharedString, StyledText,
    component::{Rope, highlighter::SyntaxHighlighter, input::HighlightStyleResolver},
};
use similar::DiffableStr as _;
use std::{ops::Range, path::Path};

pub(super) struct HighlightedLine {
    text: SharedString,
    source: Range<usize>,
    // Source-relative byte positions; each tab becomes four display spaces.
    tabs: Vec<usize>,
}

impl HighlightedLine {
    fn display_offset(&self, source_offset: usize) -> usize {
        let offset = source_offset.clamp(self.source.start, self.source.end) - self.source.start;
        offset + self.tabs.partition_point(|&tab| tab < offset) * 3
    }
}

pub(super) struct HighlightedSource {
    lines: Vec<HighlightedLine>,
    // Retain the parsed source, not theme-specific colors. The public
    // highlighter API resolves captures against a theme when a line is drawn.
    highlighter: Option<SyntaxHighlighter>,
}

impl HighlightedSource {
    fn new(path: Option<&Path>, text: Option<&str>) -> Self {
        let text = text.unwrap_or_default();
        let highlighter = path.filter(|_| !text.is_empty()).map(|path| {
            let mut highlighter = SyntaxHighlighter::new(language_settings(path).0);
            highlighter.update(None, &Rope::from_str(text), None);
            highlighter
        });
        let mut offset = 0;
        let lines = text
            .tokenize_lines()
            .into_iter()
            .map(|line| {
                let source = line.trim_end_matches('\n').trim_end_matches('\r');
                let result = HighlightedLine {
                    text: source.replace('\t', "    ").into(),
                    source: offset..offset + source.len(),
                    tabs: source.match_indices('\t').map(|(i, _)| i).collect(),
                };
                offset += line.len();
                result
            })
            .collect();
        Self { lines, highlighter }
    }

    fn highlights(
        &self,
        index: usize,
        theme: &dyn HighlightStyleResolver,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        let Some(line) = self.lines.get(index).filter(|line| !line.source.is_empty()) else {
            return Vec::new();
        };
        let Some(highlighter) = &self.highlighter else {
            return Vec::new();
        };
        highlighter
            .styles(&line.source, theme)
            .into_iter()
            .filter(|(range, style)| !range.is_empty() && *style != HighlightStyle::default())
            .map(|(range, style)| {
                (
                    line.display_offset(range.start)..line.display_offset(range.end),
                    style,
                )
            })
            .collect()
    }

    pub(super) fn styled_line(
        &self,
        index: usize,
        theme: &dyn HighlightStyleResolver,
    ) -> StyledText {
        let text = self
            .lines
            .get(index)
            .map(|line| line.text.clone())
            .unwrap_or_default();
        StyledText::new(text).with_highlights(self.highlights(index, theme))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Row {
    pub before: Option<usize>,
    pub after: Option<usize>,
    pub changed: bool,
}

pub(super) struct PreparedDocument {
    pub document: DiffDocument,
    pub before: HighlightedSource,
    pub after: HighlightedSource,
    pub rows: Vec<Row>,
    pub max_columns: usize,
}

impl PreparedDocument {
    pub(super) fn new(entry: &DiffEntry, document: DiffDocument) -> Self {
        let before = HighlightedSource::new(entry.old_path.as_deref(), document.before.text());
        let after = HighlightedSource::new(entry.new_path.as_deref(), document.after.text());
        let mut rows = Vec::new();
        let mut append = |old: Range<usize>, new: Range<usize>, changed| {
            for offset in 0..old.len().max(new.len()) {
                rows.push(Row {
                    before: (offset < old.len()).then_some(old.start + offset),
                    after: (offset < new.len()).then_some(new.start + offset),
                    changed,
                });
            }
        };
        if document.before.text().is_some() && document.after.text().is_some() {
            let (mut old, mut new) = (0, 0);
            for hunk in &document.hunks {
                append(old..hunk.old_lines.start, new..hunk.new_lines.start, false);
                append(hunk.old_lines.clone(), hunk.new_lines.clone(), true);
                old = hunk.old_lines.end;
                new = hunk.new_lines.end;
            }
            append(old..before.lines.len(), new..after.lines.len(), false);
        }
        let max_columns = before
            .lines
            .iter()
            .chain(&after.lines)
            .map(|line| line.text.chars().count())
            .max()
            .unwrap_or(0);
        Self {
            document,
            before,
            after,
            rows,
            max_columns,
        }
    }
}
