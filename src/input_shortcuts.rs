use gpui_kit::{
    App, KeyBinding,
    component::input::{
        DeleteToBeginningOfLine, DeleteToEndOfLine, DeleteToPreviousWordStart, MoveEnd, MoveHome,
        MoveToNextWord, MoveToPreviousWord,
    },
};

/// Applied only to the message composer and command palette, not other inputs.
pub(crate) const CONTEXT: &str = "TerminalInput";

/// Register after the component defaults so Ctrl+A replaces SelectAll.
pub(crate) fn init(cx: &mut App) {
    // Match at the focused Input's depth so these override its default bindings.
    let context = Some("TerminalInput > Input");
    cx.bind_keys([
        KeyBinding::new("ctrl-a", MoveHome, context),
        KeyBinding::new("ctrl-e", MoveEnd, context),
        KeyBinding::new("ctrl-left", MoveToPreviousWord, context),
        KeyBinding::new("ctrl-right", MoveToNextWord, context),
        KeyBinding::new("ctrl-w", DeleteToPreviousWordStart, context),
        KeyBinding::new("ctrl-u", DeleteToBeginningOfLine, context),
        KeyBinding::new("ctrl-k", DeleteToEndOfLine, context),
    ]);
}
