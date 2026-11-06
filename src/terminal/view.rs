use super::{TerminalSession, cache::RenderCache, keys, screen::Size};
use crate::mode::Mode;
use alacritty_terminal::{
    grid::Dimensions,
    index::{Column, Line, Point as TermPoint},
    term::TermMode,
    vte::ansi::CursorShape,
};
use gpui_kit::{
    App, Bounds, ClipboardItem, Context, ElementInputHandler, Entity, EntityInputHandler,
    FocusHandle, Focusable, Font, InteractiveElement as _, IntoElement, Keystroke, MouseButton,
    ParentElement as _, Pixels, Point, Render, Role, StatefulInteractiveElement as _, Styled as _,
    TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window, base::TestSupportExt as _, canvas,
    component::ActiveTheme as _, div, outline, point, px, size,
};
use std::ops::Range;

pub(crate) struct TerminalView {
    pub(crate) session: Entity<TerminalSession>,
    focus: FocusHandle,
    scroll: usize,
    bounds: Bounds<Pixels>,
    cell: gpui_kit::Size<Pixels>,
    cursor: Bounds<Pixels>,
    selection: Option<((usize, usize), (usize, usize))>,
    preedit: String,
    preedit_selection: Range<usize>,
    cache: RenderCache,
}

impl TerminalView {
    pub(crate) fn new(
        session: Entity<TerminalSession>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe_in(&session, window, |_, _, _, cx| cx.notify())
            .detach();
        Self {
            session,
            focus: cx.focus_handle(),
            scroll: 0,
            bounds: Bounds::default(),
            cell: size(px(8.), px(16.)),
            cursor: Bounds::default(),
            selection: None,
            preedit: String::new(),
            preedit_selection: 0..0,
            cache: RenderCache::default(),
        }
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
        self.focus.focus(window, cx);
    }

    fn send(&mut self, bytes: Vec<u8>, window: &mut Window, cx: &mut Context<Self>) {
        let view_changed = self.scroll != 0 || self.selection.is_some();
        self.scroll = 0;
        self.selection = None;
        self.session
            .update(cx, |session, cx| session.write(bytes, window, cx));
        // The PTY echo notifies us when there is new output to draw.
        if view_changed {
            cx.notify();
        }
    }

    fn terminal_mode(&self, cx: &App) -> TermMode {
        *self
            .session
            .read(cx)
            .process
            .screen
            .lock()
            .unwrap()
            .term
            .mode()
    }

    pub(crate) fn key(
        &mut self,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if key.key == "escape" {
            self.preedit.clear();
            self.preedit_selection = 0..0;
            window.notify_text_input_changed(&self.focus);
            cx.set_global(Mode::Normal);
            cx.notify();
            return true;
        }
        if key.modifiers.control && key.modifiers.shift && key.key.eq_ignore_ascii_case("c") {
            self.copy(cx);
            return true;
        }
        if *cx.global::<Mode>() != Mode::Insert {
            return false;
        }
        if (key.modifiers.control && key.modifiers.shift && key.key.eq_ignore_ascii_case("v"))
            || (key.modifiers.shift && key.key == "insert")
        {
            if let Some(item) = cx.read_from_clipboard() {
                self.paste(item, window, cx);
            }
            return true;
        }
        if key.modifiers.shift && matches!(key.key.as_str(), "pageup" | "pagedown") {
            let rows = self
                .session
                .read(cx)
                .process
                .screen
                .lock()
                .unwrap()
                .size
                .rows as i32;
            self.scroll_by(if key.key == "pageup" { rows } else { -rows }, cx);
            return true;
        }
        // Let the native IME handle editing keys until it commits its composition.
        if !self.preedit.is_empty() {
            return false;
        }
        if let Some(bytes) = keys::encode(key, self.terminal_mode(cx)) {
            self.send(bytes, window, cx);
            true
        } else {
            // Printable input goes through EntityInputHandler, not key_char;
            // otherwise IME input and keyboard-layout translation get duplicated.
            false
        }
    }

    fn scroll_by(&mut self, lines: i32, cx: &mut Context<Self>) {
        let screen = self.session.read(cx).process.screen.lock().unwrap();
        let scroll = self
            .scroll
            .saturating_add_signed(lines as isize)
            .min(screen.term.history_size());
        drop(screen);
        if scroll != self.scroll || self.selection.is_some() {
            self.scroll = scroll;
            self.selection = None;
            cx.notify();
        }
    }

    fn position(&self, position: Point<Pixels>, cx: &App) -> (usize, usize) {
        let screen = self.session.read(cx).process.screen.lock().unwrap();
        let row = ((position.y - self.bounds.top()) / self.cell.height).max(0.) as usize;
        let col = ((position.x - self.bounds.left()) / self.cell.width).max(0.) as usize;
        (
            row.min(screen.term.screen_lines() - 1),
            col.min(screen.term.columns() - 1),
        )
    }

    fn copy(&self, cx: &mut App) {
        let Some((a, b)) = self.selection else {
            return;
        };
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        let text = {
            let screen = self.session.read(cx).process.screen.lock().unwrap();
            let offset = self.scroll.min(screen.term.history_size()) as i32;
            let to_point = |(r, c): (usize, usize)| {
                TermPoint::new(
                    Line(r.min(screen.term.screen_lines() - 1) as i32 - offset),
                    Column(c.min(screen.term.columns() - 1)),
                )
            };
            screen.term.bounds_to_string(to_point(start), to_point(end))
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    fn mouse_report(
        &mut self,
        code: u8,
        released: bool,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let mode = self.terminal_mode(cx);
        if *cx.global::<Mode>() != Mode::Insert || !mode.intersects(TermMode::MOUSE_MODE) {
            return false;
        }
        let (row, col) = self.position(position, cx);
        let bytes = if mode.contains(TermMode::SGR_MOUSE) {
            format!(
                "\x1b[<{code};{};{}{}",
                col + 1,
                row + 1,
                if released { 'm' } else { 'M' }
            )
            .into_bytes()
        } else if row < 223 && col < 223 {
            vec![
                27,
                b'[',
                b'M',
                if released { 35 } else { 32 + code },
                33 + col as u8,
                33 + row as u8,
            ]
        } else {
            return true;
        };
        self.send(bytes, window, cx);
        true
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let session = self.session.read(cx);
        let label = session.label();
        let screen = session.process.screen.lock().unwrap();
        self.scroll = self.scroll.min(screen.term.history_size());
        self.cache.sync(
            &screen,
            self.scroll,
            cx.theme().foreground,
            cx.theme().background,
        );
        let text = self.cache.text.clone();
        let status = screen.exit.clone().unwrap_or_else(|| {
            if *cx.global::<Mode>() == Mode::Insert {
                "INSERT · Esc: Normal".into()
            } else {
                "NORMAL · i: Terminal input".into()
            }
        });
        drop(screen);
        let layout_view = cx.entity();
        let paint_view = cx.entity();
        div()
            .id("terminal")
            .test_support()
            .role(Role::Group)
            .aria_label(label)
            .track_focus(&self.focus)
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(self.cache.background())
            .text_color(self.cache.foreground())
            .child(
                div()
                    .id("terminal-screen")
                    .test_support()
                    .role(Role::TextInput)
                    .aria_label("Terminal screen")
                    .aria_value(text)
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .p_1()
                    .overflow_hidden()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &gpui_kit::MouseDownEvent, window, cx| {
                            this.focus.focus(window, cx);
                            if event.modifiers.shift
                                || !this.mouse_report(0, false, event.position, window, cx)
                            {
                                let p = this.position(event.position, cx);
                                this.selection = Some((p, p));
                            }
                            cx.notify();
                        }),
                    )
                    .on_mouse_move(cx.listener(
                        |this, event: &gpui_kit::MouseMoveEvent, window, cx| {
                            if event.pressed_button == Some(MouseButton::Left) {
                                if let Some((start, _)) = this.selection {
                                    let selection =
                                        Some((start, this.position(event.position, cx)));
                                    if this.selection != selection {
                                        this.selection = selection;
                                        cx.notify();
                                    }
                                } else if this
                                    .terminal_mode(cx)
                                    .intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION)
                                {
                                    this.mouse_report(32, false, event.position, window, cx);
                                }
                            }
                        },
                    ))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, event: &gpui_kit::MouseUpEvent, window, cx| {
                            if this.selection.is_none() {
                                this.mouse_report(0, true, event.position, window, cx);
                            }
                        }),
                    )
                    .on_scroll(
                        cx.listener(|this, event: &gpui_kit::ScrollEvent, window, cx| {
                            let delta = event.delta.pixel_delta(this.cell.height).y;
                            if !event.modifiers.shift
                                && this.mouse_report(
                                    if delta > px(0.) { 64 } else { 65 },
                                    false,
                                    window.mouse_position(),
                                    window,
                                    cx,
                                )
                            {
                                cx.stop_propagation();
                                return;
                            }
                            let lines =
                                (f32::from(delta) / f32::from(this.cell.height)).round() as i32;
                            if lines != 0 {
                                this.scroll_by(lines, cx);
                            }
                            cx.stop_propagation();
                        }),
                    )
                    .child(
                        canvas(
                            move |bounds, window, cx| {
                                layout_view.update(cx, |this, cx| this.layout(bounds, window, cx));
                            },
                            move |bounds, _, window, cx| {
                                paint_view.update(cx, |this, cx| this.paint(bounds, window, cx));
                            },
                        )
                        .size_full(),
                    ),
            )
            .child(
                div()
                    .id("terminal-status")
                    .test_support()
                    .role(Role::Status)
                    .aria_label(status.clone())
                    .flex_shrink_0()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .bg(cx.theme().muted)
                    .text_color(cx.theme().muted_foreground)
                    .child(status),
            )
    }
}

impl TerminalView {
    fn font(cx: &App) -> Font {
        Font {
            family: cx.theme().mono_font_family.clone(),
            ..Default::default()
        }
    }
    fn layout(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.cache
            .prepare(Self::font(cx), cx.theme().mono_font_size, window);
        self.cell = self.cache.cell;
        let size = Size {
            columns: ((bounds.size.width / self.cell.width).floor() as u16).clamp(2, 1000),
            rows: ((bounds.size.height / self.cell.height).floor() as u16).clamp(1, 1000),
            cell_width: f32::from(self.cell.width).round().max(1.) as u16,
            cell_height: f32::from(self.cell.height).round().max(1.) as u16,
        };
        self.bounds = bounds;
        if self.focus.is_focused(window) {
            self.session.update(cx, |session, _| session.resize(size));
        }
        let cursor = self.cache.cursor;
        let next = Bounds::new(
            point(
                bounds.left() + self.cell.width * cursor.column.0 as f32,
                bounds.top() + self.cell.height * (cursor.line.0 + self.scroll as i32) as f32,
            ),
            self.cell,
        );
        if self.cursor != next {
            self.cursor = next;
            if self.focus.is_focused(window) {
                window.invalidate_character_coordinates();
            }
        }
    }

    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        if *cx.global::<Mode>() == Mode::Insert {
            window.handle_input(
                &self.focus,
                ElementInputHandler::new(bounds, cx.entity()),
                cx,
            );
        }
        let selected = self
            .selection
            .map(|(a, b)| if a <= b { (a, b) } else { (b, a) });
        self.cache
            .paint(bounds, selected, cx.theme().selection, window, cx);
        let mode = self.cache.mode;
        let cursor_shape = self.cache.cursor_shape;
        if self.scroll == 0
            && mode.contains(TermMode::SHOW_CURSOR)
            && self.focus.is_focused(window)
            && cursor_shape != CursorShape::Hidden
        {
            let cursor = match cursor_shape {
                CursorShape::Beam => {
                    Bounds::new(self.cursor.origin, size(px(2.), self.cell.height))
                }
                CursorShape::Underline => Bounds::new(
                    point(self.cursor.left(), self.cursor.bottom() - px(2.)),
                    size(self.cell.width, px(2.)),
                ),
                _ => self.cursor,
            };
            window.paint_quad(
                outline(cursor, self.cache.foreground(), Default::default()).border_widths(px(1.)),
            );
        }
        if !self.preedit.is_empty() {
            let run = TextRun {
                len: self.preedit.len(),
                font: Self::font(cx),
                color: self.cache.foreground(),
                background_color: Some(cx.theme().muted),
                underline: Some(UnderlineStyle {
                    color: None,
                    thickness: px(1.),
                    wavy: false,
                }),
                strikethrough: None,
            };
            let line = window.text_system().shape_line(
                self.preedit.clone().into(),
                cx.theme().mono_font_size,
                &[run],
                None,
            );
            let _ = line.paint(
                self.cursor.origin,
                self.cell.height,
                TextAlign::Left,
                None,
                window,
                cx,
            );
        }
    }
}
impl EntityInputHandler for TerminalView {
    fn accepts_text_input(&self, _: &mut Window, cx: &mut Context<Self>) -> bool {
        *cx.global::<Mode>() == Mode::Insert
    }
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let chars = self.preedit.encode_utf16().collect::<Vec<_>>();
        let range = range.start.min(chars.len())..range.end.min(chars.len());
        *actual = Some(range.clone());
        Some(String::from_utf16_lossy(&chars[range]))
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        (*cx.global::<Mode>() == Mode::Insert).then(|| UTF16Selection {
            range: self.preedit_selection.clone(),
            reversed: false,
        })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        (!self.preedit.is_empty()).then(|| 0..self.preedit.encode_utf16().count())
    }
    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.preedit.is_empty() && self.preedit_selection == (0..0) {
            return;
        }
        self.preedit.clear();
        self.preedit_selection = 0..0;
        window.notify_text_input_changed(&self.focus);
        cx.notify();
    }
    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if *cx.global::<Mode>() != Mode::Insert {
            return;
        }
        self.unmark_text(window, cx);
        self.send(text.as_bytes().to_vec(), window, cx);
    }
    fn paste(&mut self, item: ClipboardItem, window: &mut Window, cx: &mut Context<Self>) {
        if *cx.global::<Mode>() != Mode::Insert {
            return;
        }
        if let Some(text) = item.text() {
            self.send(
                keys::paste(
                    &text,
                    self.terminal_mode(cx).contains(TermMode::BRACKETED_PASTE),
                ),
                window,
                cx,
            );
        }
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if *cx.global::<Mode>() != Mode::Insert {
            return;
        }
        self.preedit = text.to_owned();
        let len = text.encode_utf16().count();
        self.preedit_selection = selected.unwrap_or(len..len);
        window.notify_text_input_changed(&self.focus);
        window.invalidate_character_coordinates();
        cx.notify();
    }
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(self.cursor)
    }
    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(0)
    }
    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(self.preedit.encode_utf16().count())
    }
}
