use gpui_kit::{Context, Keystroke};

#[derive(Clone, Copy)]
pub(crate) enum InteractionMode {
    Normal,
    Insert,
    CommandPalette,
}

pub(crate) struct Interaction {
    mode: InteractionMode,
}

impl Interaction {
    pub(crate) fn new() -> Self {
        Self {
            mode: InteractionMode::Normal,
        }
    }

    pub(crate) fn mode(&self) -> InteractionMode {
        self.mode
    }

    pub(crate) fn set_mode(&mut self, next: InteractionMode, cx: &mut Context<Self>) {
        self.mode = next;
        // Focus listeners can request a mode during drawing, when GPUI suppresses notify.
        // Notify afterward, also for Escape while already Normal.
        let entity = cx.entity();
        cx.defer(move |cx| cx.notify(entity.entity_id()));
    }

    /// Returns whether the keystroke was handled and must not reach other handlers.
    pub(crate) fn handle_key(&mut self, key: &Keystroke, cx: &mut Context<Self>) -> bool {
        let next = if key.key == "escape" && key.modifiers == Default::default() {
            InteractionMode::Normal
        } else if matches!(self.mode, InteractionMode::Normal)
            && !key.modifiers.control
            && !key.modifiers.platform
            && !key.modifiers.alt
        {
            match key.key_char.as_deref().unwrap_or(&key.key) {
                "i" => InteractionMode::Insert,
                ":" => InteractionMode::CommandPalette,
                _ => return false,
            }
        } else {
            return false;
        };
        self.set_mode(next, cx);
        true
    }
}
