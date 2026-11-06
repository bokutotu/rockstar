use alacritty_terminal::{
    Term,
    event::{Event, EventListener},
    grid::Dimensions,
    index::Line,
    term::{
        cell::{Cell, Flags},
        color::{COUNT, Colors},
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
    pub fn row(&self, row: usize, offset: usize) -> &[Cell] {
        let offset = offset.min(self.term.history_size());
        &self.term.grid()[Line(row as i32 - offset as i32)][..]
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
    text.truncate(text.trim_end().len());
    text
}

/// Resolve the app's theme and OSC overrides once, rather than for every cell.
pub(super) fn palette(
    overrides: &Colors,
    foreground: gpui_kit::Hsla,
    background: gpui_kit::Hsla,
) -> [Rgb; COUNT] {
    let to_rgb = |color: gpui_kit::Hsla| {
        let color = color.to_rgb();
        Rgb {
            r: (color.r * 255.).round() as u8,
            g: (color.g * 255.).round() as u8,
            b: (color.b * 255.).round() as u8,
        }
    };
    let foreground = to_rgb(foreground);
    let background = to_rgb(background);
    std::array::from_fn(|index| {
        overrides[index].unwrap_or_else(|| {
            if index == NamedColor::Background as usize {
                background
            } else if index >= 256 {
                foreground
            } else {
                indexed_color(index)
            }
        })
    })
}

// ANSI colors for a light terminal surface. The color cube, grayscale, and RGB colors are unchanged.
pub(super) fn indexed_color(index: usize) -> Rgb {
    const ANSI: [u32; 16] = [
        0x4c4f69, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0x8839ef, 0x179299, 0xbcc0cc, 0x5c5f77,
        0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x04a5e5, 0xeff1f5,
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
        0xffffff
    } else {
        0x0a0a0a
    };
    Rgb {
        r: (hex >> 16) as u8,
        g: (hex >> 8) as u8,
        b: hex as u8,
    }
}
pub(super) fn color(color: Color, palette: &[Rgb; COUNT]) -> Rgb {
    match color {
        Color::Spec(rgb) => rgb,
        Color::Indexed(i) => palette[i as usize],
        Color::Named(name) => palette[name as usize],
    }
}
