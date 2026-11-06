use gpui_kit::{
    App, Entity, Focusable as _, Subscription, Window, component::input::TextareaState,
};

/// Notify the native IME after layout, not just when the draft text changes.
/// Input notifications also cover caret movement, scrolling, and resizing.
pub(super) fn observe_ime_position(
    input: &Entity<TextareaState>,
    window: &mut Window,
    cx: &mut App,
) -> Subscription {
    window.observe(input, cx, |input, window, cx| {
        if input.focus_handle(cx).is_focused(window) {
            // GPUI queries the focused input's geometry after the next paint.
            window.invalidate_character_coordinates();
        }
    })
}
