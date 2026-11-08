use super::*;

#[derive(Clone, Debug)]
pub(crate) struct TextEdit {
    pub range: Range<usize>,
    pub replacement: String,
}
#[derive(Clone, Debug)]
pub(crate) struct EditTransaction {
    pub edits: Vec<TextEdit>,
    pub cursor_before: usize,
    pub cursor_after: usize,
}
impl EditTransaction {
    pub fn replace(range: Range<usize>, replacement: String, before: usize, after: usize) -> Self {
        Self {
            edits: vec![TextEdit { range, replacement }],
            cursor_before: before,
            cursor_after: after,
        }
    }
    pub fn apply(&self, text: &str) -> Option<String> {
        let mut edits = self.edits.iter().collect::<Vec<_>>();
        edits.sort_by_key(|edit| (edit.range.start, edit.range.end));
        let mut previous = 0;
        for edit in &edits {
            if edit.range.start < previous
                || edit.range.end < edit.range.start
                || edit.range.end > text.len()
                || !text.is_char_boundary(edit.range.start)
                || !text.is_char_boundary(edit.range.end)
            {
                return None;
            }
            previous = edit.range.end;
        }
        let mut result = text.to_owned();
        for edit in edits.into_iter().rev() {
            result.replace_range(edit.range.clone(), &edit.replacement);
        }
        Some(result)
    }
}

/// Splitting through a tab or wide character retains its unselected cells as
/// spaces. Positions past EOL are represented by padding only when requested.
fn split_cells(line: &str, target: usize, tab: usize, pad: bool) -> (String, String) {
    let mut col = 0;
    for (offset, g) in line.grapheme_indices(true) {
        if col == target {
            return (line[..offset].into(), line[offset..].into());
        }
        let width = cell_width(g, col, tab);
        if col + width > target {
            return (
                format!("{}{}", &line[..offset], " ".repeat(target - col)),
                format!(
                    "{}{}",
                    " ".repeat(col + width - target),
                    &line[offset + g.len()..]
                ),
            );
        }
        col += width;
    }
    (
        format!(
            "{line}{}",
            if pad {
                " ".repeat(target.saturating_sub(col))
            } else {
                String::new()
            }
        ),
        String::new(),
    )
}
fn block_parts(line: &str, columns: &Range<usize>, tab: usize) -> (String, String, String) {
    let (prefix, tail) = split_cells(line, columns.start, tab, false);
    // Widths in the tail still depend on their original screen column.
    let mut selected = String::new();
    let mut suffix = String::new();
    let mut col = columns.start;
    for g in tail.graphemes(true) {
        let width = cell_width(g, col, tab);
        if col >= columns.end {
            suffix.push_str(g);
        } else if col + width > columns.end {
            selected.push_str(&" ".repeat(columns.end - col));
            suffix.push_str(&" ".repeat(col + width - columns.end));
        } else if g == "\t" {
            selected.push_str(&" ".repeat(width));
        } else {
            selected.push_str(g);
        }
        col += width;
    }
    (prefix, selected, suffix)
}

pub(crate) fn capture_target(text: &str, target: &ResolvedTarget, tab: usize) -> RegisterValue {
    match target.kind {
        SelectionKind::Character => {
            RegisterValue::Character(target.ranges.iter().map(|r| &text[r.clone()]).collect())
        }
        SelectionKind::Line => {
            let mut value: String = target.ranges.iter().map(|r| &text[r.clone()]).collect();
            if !value.ends_with('\n') {
                value.push('\n');
            }
            RegisterValue::Line(value)
        }
        SelectionKind::Block => {
            let cols = target.block_columns.as_ref().unwrap();
            let rows = target
                .ranges
                .iter()
                .map(|range| {
                    let line = &text[start(text, range.start)..end(text, range.start)];
                    let (_, mut selected, _) = block_parts(line, cols, tab);
                    let width = selected.width();
                    selected.push_str(&" ".repeat(cols.len().saturating_sub(width)));
                    selected
                })
                .collect();
            RegisterValue::Block {
                rows,
                width: cols.len(),
            }
        }
    }
}
fn change_case(text: &str, op: Operator) -> String {
    text.chars()
        .flat_map(|c| -> Vec<char> {
            match op {
                Operator::Lower => c.to_lowercase().collect(),
                Operator::Upper => c.to_uppercase().collect(),
                Operator::Swap => {
                    if c.is_uppercase() {
                        c.to_lowercase().collect()
                    } else {
                        c.to_uppercase().collect()
                    }
                }
                Operator::Rot13 => vec![match c {
                    'a'..='z' => char::from(b'a' + (c as u8 - b'a' + 13) % 26),
                    'A'..='Z' => char::from(b'A' + (c as u8 - b'A' + 13) % 26),
                    _ => c,
                }],
                _ => vec![c],
            }
        })
        .collect()
}
pub(crate) fn plan_operator(
    text: &str,
    view: &VimViewState,
    op: Operator,
    target: &ResolvedTarget,
    tab: usize,
    shifts: usize,
) -> Option<EditTransaction> {
    if op == Operator::Yank {
        return None;
    }
    let first = target.ranges.first()?.start;
    let mut transaction = EditTransaction {
        edits: Vec::new(),
        cursor_before: view.cursor,
        cursor_after: first,
    };
    if matches!(op, Operator::Indent | Operator::Outdent) {
        let last = target.ranges.last()?;
        let lines = line_range(
            text,
            first,
            if last.is_empty() {
                last.end
            } else {
                prev(text, last.end)
            },
        );
        let mut offset = lines.start;
        for line in text[lines].split_inclusive('\n') {
            let contents = line.trim_end_matches('\n');
            if !contents.is_empty() {
                let indent_end = contents.len() - contents.trim_start_matches([' ', '\t']).len();
                let width = display_column(contents, indent_end, tab);
                let amount = tab.saturating_mul(shifts.min(100_000));
                let new_width = if op == Operator::Indent {
                    width.saturating_add(amount)
                } else {
                    width.saturating_sub(amount)
                };
                transaction.edits.push(TextEdit {
                    range: offset..offset + indent_end,
                    replacement: " ".repeat(new_width),
                });
            }
            offset += line.len();
        }
        if let Some(new) = transaction.apply(text) {
            transaction.cursor_after = first_nonblank(&new, first);
        }
        return Some(transaction);
    }
    if target.kind == SelectionKind::Block {
        let cols = target.block_columns.as_ref()?;
        for range in &target.ranges {
            let s = start(text, range.start);
            let e = end(text, s);
            let (prefix, selected, suffix) = block_parts(&text[s..e], cols, tab);
            let middle = if matches!(op, Operator::Delete | Operator::Change) {
                String::new()
            } else {
                change_case(&selected, op)
            };
            if transaction.edits.is_empty() {
                transaction.cursor_after = s + prefix.len();
            }
            transaction.edits.push(TextEdit {
                range: s..e,
                replacement: format!("{prefix}{middle}{suffix}"),
            });
        }
    } else {
        for range in &target.ranges {
            let mut range = range.clone();
            let replacement = match op {
                Operator::Delete => {
                    if target.kind == SelectionKind::Line
                        && range.end == text.len()
                        && !text.ends_with('\n')
                        && range.start > 0
                    {
                        range.start -= 1;
                    }
                    String::new()
                }
                Operator::Change if target.kind == SelectionKind::Line => {
                    let indent = &text[range.start..first_nonblank(text, range.start)];
                    transaction.cursor_after = range.start + indent.len();
                    format!(
                        "{indent}{}",
                        if text[range.clone()].ends_with('\n') {
                            "\n"
                        } else {
                            ""
                        }
                    )
                }
                Operator::Change => String::new(),
                _ => change_case(&text[range.clone()], op),
            };
            transaction.edits.push(TextEdit { range, replacement });
        }
    }
    if op == Operator::Delete && target.kind == SelectionKind::Line {
        if let Some(new) = transaction.apply(text) {
            transaction.cursor_after = first_nonblank(&new, normal_cursor(&new, first));
        }
    }
    Some(transaction)
}

pub(crate) fn plan_paste(
    text: &str,
    cursor: usize,
    value: &RegisterValue,
    before: bool,
    after: bool,
    count: usize,
    tab: usize,
) -> EditTransaction {
    let count = count.clamp(1, 10_000);
    match value {
        RegisterValue::Keys(_) => plan_paste(
            text,
            cursor,
            &RegisterValue::Character(value.text()),
            before,
            after,
            count,
            tab,
        ),
        RegisterValue::Character(value) => {
            let value = value.repeat(count);
            let pos = if before {
                cursor
            } else {
                next(text, cursor).min(end(text, cursor))
            };
            let last = if after {
                value.len()
            } else if value.contains('\n') {
                0
            } else {
                prev(&value, value.len())
            };
            EditTransaction::replace(pos..pos, value, cursor, pos + last)
        }
        RegisterValue::Line(value) => {
            let mut value = value.repeat(count);
            let pos = if before {
                start(text, cursor)
            } else {
                next(text, end(text, cursor))
            };
            let mut first = pos;
            if text.is_empty() {
                if value.ends_with('\n') {
                    value.pop();
                }
            } else if pos == text.len() && !text.ends_with('\n') {
                value.insert(0, '\n');
                value.pop();
                first += 1;
            }
            let last = if after {
                pos + value.len()
            } else {
                first
                    + value[first - pos..]
                        .chars()
                        .take_while(|c| matches!(c, ' ' | '\t'))
                        .map(char::len_utf8)
                        .sum::<usize>()
            };
            EditTransaction::replace(pos..pos, value, cursor, last)
        }
        RegisterValue::Block { rows, width } => {
            let col = display_column(text, cursor, tab)
                + if !before && cursor < end(text, cursor) {
                    cell_width(
                        &text[cursor..next(text, cursor)],
                        display_column(text, cursor, tab),
                        tab,
                    )
                } else {
                    0
                };
            plan_block_paste(text, cursor, rows, *width, col, after, count, tab)
        }
    }
}
pub(crate) fn plan_block_paste(
    text: &str,
    cursor: usize,
    rows: &[String],
    width: usize,
    col: usize,
    after: bool,
    count: usize,
    tab: usize,
) -> EditTransaction {
    let first_row = row(text, cursor);
    let mut lines = text
        .strip_suffix('\n')
        .unwrap_or(text)
        .split('\n')
        .map(str::to_owned)
        .collect::<Vec<_>>();
    while lines.len() < first_row + rows.len() {
        lines.push(String::new());
    }
    let mut cursor_col = 0;
    for (i, value) in rows.iter().enumerate() {
        let (prefix, suffix) = split_cells(&lines[first_row + i], col, tab, true);
        if i == 0 {
            cursor_col = prefix.len();
        }
        let mut cell = value.clone();
        cell.push_str(&" ".repeat(width.saturating_sub(value.width())));
        lines[first_row + i] = format!("{prefix}{}{suffix}", cell.repeat(count));
    }
    let mut new = lines.join("\n");
    if text.ends_with('\n') {
        new.push('\n');
    }
    let pos = line_at(&new, first_row) + cursor_col;
    let pos = if after {
        at_column(&new, line_at(&new, first_row), col + width * count, tab)
    } else {
        pos
    };
    EditTransaction::replace(0..text.len(), new, cursor, pos)
}

pub(crate) fn plan_visual_paste(
    text: &str,
    view: &VimViewState,
    target: &ResolvedTarget,
    value: &RegisterValue,
    count: usize,
    tab: usize,
) -> Option<EditTransaction> {
    let deletion = plan_operator(text, view, Operator::Delete, target, tab, 1)?;
    let deleted = deletion.apply(text)?;
    let mut cursor = target.ranges.first()?.start.min(deleted.len());
    let value = if target.kind == SelectionKind::Line {
        let mut value = value.text();
        if !value.ends_with('\n') {
            value.push('\n');
        }
        // Replacing whole lines needs no extra newline inserted at the old EOF.
        let range = target.ranges.first()?.clone();
        let mut value = value.repeat(count.min(10_000));
        if range.end == text.len() && !text.ends_with('\n') {
            value.pop();
        }
        return Some(EditTransaction::replace(
            range.clone(),
            value,
            view.cursor,
            range.start,
        ));
    } else {
        value.clone()
    };
    if target.kind == SelectionKind::Block {
        cursor = deletion.cursor_after.min(deleted.len());
    }
    let pasted = plan_paste(&deleted, cursor, &value, true, false, count, tab);
    Some(EditTransaction::replace(
        0..text.len(),
        pasted.apply(&deleted)?,
        view.cursor,
        pasted.cursor_after,
    ))
}

pub(crate) fn plan_input(
    text: &str,
    cursor: usize,
    inserted: &str,
    mode: Mode,
    tab: usize,
) -> EditTransaction {
    let mut to = cursor;
    if matches!(mode, Mode::Replace | Mode::VirtualReplace) {
        if mode == Mode::VirtualReplace && !inserted.contains('\n') {
            let col = display_column(text, cursor, tab);
            let width = inserted
                .graphemes(true)
                .fold(col, |col, g| col + cell_width(g, col, tab));
            to = at_column(text, start(text, cursor), width, tab);
            if to < end(text, cursor) && display_column(text, to, tab) < width {
                let next = next(text, to);
                let padding = display_column(text, next, tab).saturating_sub(width);
                return EditTransaction::replace(
                    cursor..next,
                    format!("{inserted}{}", " ".repeat(padding)),
                    cursor,
                    cursor + inserted.len(),
                );
            }
        } else {
            for g in inserted.graphemes(true) {
                if g != "\n" && to < end(text, cursor) {
                    to = next(text, to);
                }
            }
        }
    }
    EditTransaction::replace(cursor..to, inserted.into(), cursor, cursor + inserted.len())
}

pub(crate) fn plan_join(
    text: &str,
    cursor: usize,
    first: usize,
    count: usize,
    space: bool,
) -> Option<EditTransaction> {
    let range = line_range(
        text,
        first,
        line_at(text, row(text, first) + count.max(2) - 1),
    );
    let body = &text[range.clone()];
    let trailing_newline = body.ends_with('\n');
    let mut lines = body.strip_suffix('\n').unwrap_or(body).split('\n');
    let mut joined = lines.next()?.to_owned();
    let mut after = range.start + joined.len();
    let mut any = false;
    for line in lines {
        let line = if space {
            line.trim_start_matches([' ', '\t'])
        } else {
            line
        };
        after = range.start + joined.len();
        if space
            && !joined.is_empty()
            && !line.is_empty()
            && !joined.ends_with(char::is_whitespace)
            && !line.starts_with(')')
        {
            joined.push(' ');
        }
        joined.push_str(line);
        any = true;
    }
    if !any {
        return None;
    }
    if trailing_newline {
        joined.push('\n');
    }
    Some(EditTransaction::replace(range, joined, cursor, after))
}

pub(crate) fn plan_number(
    text: &str,
    cursor: usize,
    increase: bool,
    count: usize,
) -> Option<EditTransaction> {
    let line_start = start(text, cursor);
    let line_end = end(text, cursor);
    let bytes = text.as_bytes();
    let mut i = line_start;
    while i < line_end {
        let begin = i;
        let negative = bytes[i] == b'-' && i + 1 < line_end && bytes[i + 1].is_ascii_digit();
        if negative {
            i += 1;
        }
        if !bytes[i].is_ascii_digit() {
            i = next(text, i);
            continue;
        }
        let digits_start = i;
        let radix = if i + 2 < line_end
            && bytes[i] == b'0'
            && matches!(bytes[i + 1], b'x' | b'X' | b'b' | b'B')
        {
            let radix = if matches!(bytes[i + 1], b'x' | b'X') {
                16
            } else {
                2
            };
            if (bytes[i + 2] as char).is_digit(radix) {
                i += 2;
                radix
            } else {
                10
            }
        } else {
            10
        };
        let actual_digits = i;
        while i < line_end && (bytes[i] as char).is_digit(radix) {
            i += 1;
        }
        if i <= cursor {
            continue;
        }
        let value = i128::from_str_radix(&text[actual_digits..i], radix).ok()?;
        let value = if negative { -value } else { value };
        let delta = if increase {
            count as i128
        } else {
            -(count as i128)
        };
        let value = value.checked_add(delta)?;
        let width = i - actual_digits;
        let sign = if value < 0 { "-" } else { "" };
        let magnitude = value.unsigned_abs();
        let replacement = match radix {
            16 => {
                let prefix = &text[digits_start..actual_digits];
                let upper = text[actual_digits..i]
                    .chars()
                    .any(|c| matches!(c, 'A'..='F'))
                    || prefix.ends_with('X');
                if upper {
                    format!("{sign}{prefix}{magnitude:0width$X}")
                } else {
                    format!("{sign}{prefix}{magnitude:0width$x}")
                }
            }
            2 => format!(
                "{sign}{}{magnitude:0width$b}",
                &text[digits_start..actual_digits]
            ),
            _ => {
                if bytes[actual_digits] == b'0' {
                    format!("{sign}{magnitude:0width$}")
                } else {
                    value.to_string()
                }
            }
        };
        let after = begin + replacement.len() - 1;
        return Some(EditTransaction::replace(
            begin..i,
            replacement,
            cursor,
            after,
        ));
    }
    None
}
