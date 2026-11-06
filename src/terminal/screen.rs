use alacritty_terminal::{
    Term,
    event::{Event, EventListener},
    grid::Dimensions,
    index::{Column, Line, Point},
    term::{
        cell::{Cell, Flags},
        color::Colors,
    },
    vte::ansi::{Color, NamedColor, Rgb},
};

#[derive(Clone)]
pub(super) struct EventProxy(pub async_channel::Sender<Event>);
impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        // Never block the PTY parser on a stalled UI. A bounded queue also
        // prevents output containing OSC requests from exhausting memory.
        let _ = self.0.try_send(event);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Size {
    pub columns: u16,
    pub rows: u16,
    pub cell_width: u16,
    pub cell_height: u16,
}
impl Default for Size {
    fn default() -> Self {
        Self {
            columns: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
        }
    }
}
impl Size {
    pub fn pty(self) -> portable_pty::PtySize {
        portable_pty::PtySize {
            rows: self.rows,
            cols: self.columns,
            pixel_width: self.columns.saturating_mul(self.cell_width),
            pixel_height: self.rows.saturating_mul(self.cell_height),
        }
    }
    pub fn window(self) -> alacritty_terminal::event::WindowSize {
        alacritty_terminal::event::WindowSize {
            num_lines: self.rows,
            num_cols: self.columns,
            cell_width: self.cell_width,
            cell_height: self.cell_height,
        }
    }
}
impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows as usize
    }
    fn screen_lines(&self) -> usize {
        self.rows as usize
    }
    fn columns(&self) -> usize {
        self.columns as usize
    }
}

pub(super) struct Screen {
    pub term: Term<EventProxy>,
    pub size: Size,
    pub exit: Option<String>,
}
impl Screen {
    /// Scroll offsets belong to views, not the shared terminal emulator.
    pub fn rows(&self, offset: usize) -> Vec<Vec<Cell>> {
        let offset = offset.min(self.term.history_size());
        (0..self.term.screen_lines())
            .map(|row| {
                let line = Line(row as i32 - offset as i32);
                (0..self.term.columns())
                    .map(|column| self.term.grid()[Point::new(line, Column(column))].clone())
                    .collect()
            })
            .collect()
    }
    pub fn text(&self, offset: usize) -> String {
        self.rows(offset)
            .iter()
            .map(|row| row_text(row))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub(super) fn row_text(row: &[Cell]) -> String {
    let mut text = String::new();
    for cell in row {
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }
        text.push(cell.c);
        if let Some(zero) = cell.zerowidth() {
            text.extend(zero);
        }
    }
    text.trim_end().to_owned()
}

// A fixed ANSI palette, with default foreground/background matching a dark
// terminal surface. OSC palette overrides are respected by the renderer.
pub(super) fn indexed_color(index: usize) -> Rgb {
    const ANSI: [u32; 16] = [
        0x1e1e2e, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xcba6f7, 0x94e2d5, 0xcdd6f4, 0x585b70,
        0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xffffff,
    ];
    let hex = if index < 16 {
        ANSI[index]
    } else if index < 232 {
        let n = index - 16;
        let component = |n: usize| if n == 0 { 0 } else { 55 + n * 40 } as u32;
        component(n / 36) << 16 | component(n / 6 % 6) << 8 | component(n % 6)
    } else if index < 256 {
        let c = (8 + (index - 232) * 10) as u32;
        c << 16 | c << 8 | c
    } else if index == NamedColor::Background as usize {
        0x1e1e2e
    } else {
        0xcdd6f4
    };
    Rgb {
        r: (hex >> 16) as u8,
        g: (hex >> 8) as u8,
        b: hex as u8,
    }
}
pub(super) fn color(color: Color, overrides: &Colors) -> Rgb {
    match color {
        Color::Spec(rgb) => rgb,
        Color::Indexed(i) => overrides[i as usize].unwrap_or_else(|| indexed_color(i as usize)),
        Color::Named(name) => {
            overrides[name as usize].unwrap_or_else(|| indexed_color(name as usize))
        }
    }
}
