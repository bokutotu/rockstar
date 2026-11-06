//! Retain terminal rows and their glyph layouts across paints. Cells stay on
//! the terminal grid: shaping a whole row would change ligatures and wide glyphs.
use super::screen::{self, Screen};
use alacritty_terminal::{
    grid::Dimensions,
    index::{Column, Line, Point as TermPoint},
    term::{
        TermMode,
        cell::{Cell, Flags},
        color::COUNT,
    },
    vte::ansi::{CursorShape, NamedColor, Rgb},
};
use gpui_kit::{
    App, Bounds, Font, FontStyle, FontWeight, Hsla, Pixels, ShapedLine, SharedString, Size,
    StrikethroughStyle, TextAlign, TextRun, UnderlineStyle, Window, fill, point, px, rgb, size,
};
use std::ops::Range;

type Selection = Option<((usize, usize), (usize, usize))>;

struct BackgroundRun {
    columns: Range<usize>,
    color: Hsla,
}

#[derive(Default)]
struct CachedRow {
    cells: Vec<Cell>,
    text: String,
    backgrounds: Vec<BackgroundRun>,
    glyphs: Vec<(usize, ShapedLine)>,
    prepared: bool,
}

pub(super) struct RenderCache {
    rows: Vec<CachedRow>,
    palette: [Rgb; COUNT],
    font: Option<(Font, Pixels)>,
    pub text: SharedString,
    pub cell: Size<Pixels>,
    pub cursor: TermPoint,
    pub cursor_shape: CursorShape,
    pub mode: TermMode,
}

impl Default for RenderCache {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            palette: [Rgb { r: 0, g: 0, b: 0 }; COUNT],
            font: None,
            text: "".into(),
            cell: size(px(8.), px(16.)),
            cursor: TermPoint::new(Line(0), Column(0)),
            cursor_shape: CursorShape::Hidden,
            mode: TermMode::empty(),
        }
    }
}

impl RenderCache {
    /// Borrow the emulator only while comparing/copying changed rows. Text
    /// shaping and painting never hold the PTY worker's screen lock.
    pub fn sync(&mut self, screen: &Screen, scroll: usize, foreground: Hsla, background: Hsla) {
        let palette = screen::palette(screen.term.colors(), foreground, background);
        if self.palette != palette {
            self.palette = palette;
            for row in &mut self.rows {
                row.prepared = false;
            }
        }
        let count = screen.term.screen_lines();
        let mut text_changed = self.rows.len() != count;
        self.rows.resize_with(count, CachedRow::default);
        for (index, row) in self.rows.iter_mut().enumerate() {
            let cells = screen.row(index, scroll);
            if row.cells != cells {
                row.cells.clear();
                row.cells.extend_from_slice(cells);
                row.text = screen::row_text(cells);
                row.prepared = false;
                text_changed = true;
            }
        }
        if text_changed {
            let capacity = self.rows.iter().map(|row| row.text.len() + 1).sum();
            let mut text = String::with_capacity(capacity);
            for (index, row) in self.rows.iter().enumerate() {
                if index != 0 {
                    text.push('\n');
                }
                text.push_str(&row.text);
            }
            self.text = text.into();
        }
        self.cursor = screen.term.grid().cursor.point;
        self.cursor_shape = screen.term.cursor_style().shape;
        self.mode = *screen.term.mode();
    }

    pub fn foreground(&self) -> Hsla {
        to_color(self.palette[NamedColor::Foreground as usize])
    }

    pub fn background(&self) -> Hsla {
        to_color(self.palette[NamedColor::Background as usize])
    }

    pub fn prepare(&mut self, font: Font, font_size: Pixels, window: &mut Window) {
        if self
            .font
            .as_ref()
            .is_none_or(|(previous_font, previous_size)| {
                previous_font != &font || *previous_size != font_size
            })
        {
            let run = text_run(1, font.clone(), self.foreground(), Flags::empty());
            self.cell = size(
                window
                    .text_system()
                    .shape_line("M".into(), font_size, &[run], None)
                    .width
                    .max(px(1.)),
                (font_size * 1.5).ceil(),
            );
            self.font = Some((font.clone(), font_size));
            for row in &mut self.rows {
                row.prepared = false;
            }
        }
        let default_background = self.palette[NamedColor::Background as usize];
        for row in &mut self.rows {
            if row.prepared {
                continue;
            }
            row.backgrounds.clear();
            row.glyphs.clear();
            for (column, cell) in row.cells.iter().enumerate() {
                let inverse = cell.flags.contains(Flags::INVERSE);
                let bg = screen::color(if inverse { cell.fg } else { cell.bg }, &self.palette);
                if bg != default_background {
                    let color = to_color(bg);
                    if let Some(previous) = row.backgrounds.last_mut()
                        && previous.columns.end == column
                        && previous.color == color
                    {
                        previous.columns.end += 1;
                    } else {
                        row.backgrounds.push(BackgroundRun {
                            columns: column..column + 1,
                            color,
                        });
                    }
                }
                if (cell.c == ' '
                    && !cell
                        .flags
                        .intersects(Flags::ALL_UNDERLINES | Flags::STRIKEOUT))
                    || cell.flags.intersects(
                        Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER | Flags::HIDDEN,
                    )
                {
                    continue;
                }
                let mut text = cell.c.to_string();
                if let Some(zero) = cell.zerowidth() {
                    text.extend(zero);
                }
                let mut fg = to_color(screen::color(
                    if inverse { cell.bg } else { cell.fg },
                    &self.palette,
                ));
                if cell.flags.contains(Flags::DIM) {
                    fg.a *= 0.65;
                }
                let run = text_run(text.len(), font.clone(), fg, cell.flags);
                let line = window
                    .text_system()
                    .shape_line(text.into(), font_size, &[run], None);
                row.glyphs.push((column, line));
            }
            row.prepared = true;
        }
    }

    pub fn paint(
        &self,
        bounds: Bounds<Pixels>,
        selection: Selection,
        selection_color: Hsla,
        window: &mut Window,
        cx: &mut App,
    ) {
        let visible_rows = (bounds.size.height / self.cell.height).ceil().max(0.) as usize;
        let visible_columns = (bounds.size.width / self.cell.width).ceil().max(0.) as usize;
        let background = self.background();
        for (index, row) in self.rows.iter().take(visible_rows).enumerate() {
            let top = bounds.top() + self.cell.height * index as f32;
            // One base rectangle replaces the per-cell fills, preserving row
            // paint order even when a glyph overhangs its line height.
            window.paint_quad(fill(
                Bounds::new(
                    point(bounds.left(), top),
                    size(self.cell.width * row.cells.len() as f32, self.cell.height),
                ),
                background,
            ));
            for run in &row.backgrounds {
                if run.columns.start >= visible_columns {
                    break;
                }
                window.paint_quad(fill(
                    Bounds::new(
                        point(
                            bounds.left() + self.cell.width * run.columns.start as f32,
                            top,
                        ),
                        size(self.cell.width * run.columns.len() as f32, self.cell.height),
                    ),
                    run.color,
                ));
            }
            if let Some((start, end)) = selection
                && index >= start.0
                && index <= end.0
            {
                let first = if index == start.0 { start.1 } else { 0 }.min(row.cells.len());
                let last = if index == end.0 {
                    end.1.saturating_add(1)
                } else {
                    row.cells.len()
                }
                .min(row.cells.len());
                if first < last {
                    window.paint_quad(fill(
                        Bounds::new(
                            point(bounds.left() + self.cell.width * first as f32, top),
                            size(self.cell.width * (last - first) as f32, self.cell.height),
                        ),
                        selection_color,
                    ));
                }
            }
            // Paint backgrounds before glyphs so spacer cells cannot erase
            // the right half of a wide glyph.
            for (column, line) in &row.glyphs {
                if *column >= visible_columns {
                    break;
                }
                let origin = point(bounds.left() + self.cell.width * *column as f32, top);
                let _ = line.paint(origin, self.cell.height, TextAlign::Left, None, window, cx);
            }
        }
    }
}

fn text_run(len: usize, mut font: Font, color: Hsla, flags: Flags) -> TextRun {
    if flags.contains(Flags::BOLD) {
        font.weight = FontWeight::BOLD;
    }
    if flags.contains(Flags::ITALIC) {
        font.style = FontStyle::Italic;
    }
    TextRun {
        len,
        font,
        color,
        background_color: None,
        underline: flags
            .intersects(Flags::ALL_UNDERLINES)
            .then_some(UnderlineStyle {
                color: Some(color),
                thickness: px(1.),
                wavy: flags.contains(Flags::UNDERCURL),
            }),
        strikethrough: flags
            .contains(Flags::STRIKEOUT)
            .then_some(StrikethroughStyle {
                color: Some(color),
                thickness: px(1.),
            }),
    }
}

fn to_color(value: Rgb) -> Hsla {
    rgb((value.r as u32) << 16 | (value.g as u32) << 8 | value.b as u32).into()
}
