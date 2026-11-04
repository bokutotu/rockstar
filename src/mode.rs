use gpui_kit::{App, Global, Keystroke};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Normal,
    Insert,
    CommandPalette,
}

impl Global for Mode {}

pub(crate) fn init(cx: &mut App) {
    if !cx.has_global::<Mode>() {
        cx.set_global(Mode::Normal);
    }
}

/// Returns whether the keystroke was handled and must not reach other handlers.
pub(crate) fn handle_key(key: &Keystroke, cx: &mut App) -> bool {
    let next = if key.key == "escape" && key.modifiers == Default::default() {
        Mode::Normal
    } else if *cx.global::<Mode>() == Mode::Normal
        && !key.modifiers.control
        && !key.modifiers.platform
        && !key.modifiers.alt
    {
        match key.key_char.as_deref().unwrap_or(&key.key) {
            "i" => Mode::Insert,
            ":" => Mode::CommandPalette,
            _ => return false,
        }
    } else {
        return false;
    };
    cx.set_global(next);
    true
}
