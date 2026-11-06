use gpui_kit::{App, Global, Keystroke};

/// Keyboard state, independent of whether a pane contains plain, chat, or a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Normal,
    Insert,
    Visual,
    VisualLine,
    CommandPalette,
    FileFinder,
    Switcher,
    Search,
    LspPopup,
    Confirm,
}

impl Mode {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Normal => "NORMAL",
            Self::Insert => "INSERT",
            Self::Visual => "VISUAL",
            Self::VisualLine => "VISUAL LINE",
            Self::CommandPalette => "COMMAND",
            Self::FileFinder => "FILES",
            Self::Switcher => "SWITCH",
            Self::Search => "SEARCH",
            Self::LspPopup => "LSP",
            Self::Confirm => "CONFIRM",
        }
    }

    pub(crate) fn is_visual(self) -> bool {
        matches!(self, Self::Visual | Self::VisualLine)
    }
    pub(crate) fn is_overlay(self) -> bool {
        matches!(
            self,
            Self::CommandPalette | Self::FileFinder | Self::Switcher | Self::Search | Self::LspPopup | Self::Confirm
        )
    }
}

impl Global for Mode {}

pub(crate) fn init(cx: &mut App) {
    if !cx.has_global::<Mode>() {
        cx.set_global(Mode::Normal);
    }
}

pub(crate) fn text_key(key: &Keystroke) -> &str {
    match key.key_char.as_deref().unwrap_or(&key.key) {
        "space" => " ",
        "backslash" => "\\",
        other => other,
    }
}
