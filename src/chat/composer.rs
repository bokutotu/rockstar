use gpui_kit::{
    App, AppContext as _, Context, Entity, EntityId, EventEmitter, Focusable as _,
    InteractiveElement as _, IntoElement, ParentElement as _, Styled as _, View, Window,
    component::{
        ActiveTheme as _,
        input::{Enter, Textarea, TextareaState},
    },
    div,
};

use crate::interaction::{Interaction, InteractionMode};

const NORMAL_HINT: &str = "NORMAL — i to type · : commands";

pub(super) enum ComposerEvent {
    SubmitRequested(String),
}

pub(super) struct ComposerView {
    input: Entity<TextareaState>,
    interaction: Entity<Interaction>,
    input_editable: bool,
}

impl EventEmitter<ComposerEvent> for ComposerView {}

impl ComposerView {
    pub(super) fn new(
        interaction: Entity<Interaction>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(3)
                .placeholder(NORMAL_HINT)
        });
        cx.observe_in(&interaction, window, |this, interaction, window, cx| {
            this.apply_mode(interaction.read(cx).mode(), window, cx);
        })
        .detach();
        cx.on_focus(&input.focus_handle(cx), window, |this, _, cx| {
            this.interaction.update(cx, |interaction, cx| {
                if matches!(interaction.mode(), InteractionMode::Normal) {
                    interaction.set_mode(InteractionMode::Insert, cx);
                }
            });
        })
        .detach();
        let mode = interaction.read(cx).mode();
        let mut view = Self {
            input,
            interaction,
            input_editable: false,
        };
        view.apply_mode(mode, window, cx);
        view
    }

    fn apply_mode(&mut self, mode: InteractionMode, window: &mut Window, cx: &mut Context<Self>) {
        let editable = matches!(mode, InteractionMode::Insert);
        if self.input_editable == editable {
            return;
        }
        self.input_editable = editable;
        let hint = if editable { "Ask pi…" } else { NORMAL_HINT };
        self.input
            .update(cx, |input, cx| input.set_placeholder(hint, window, cx));
        if editable {
            self.input.focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }

    pub(super) fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
        cx.notify();
    }

    fn handle_enter(&mut self, action: &Enter, cx: &mut Context<Self>) {
        if !action.secondary {
            return;
        }
        if matches!(self.interaction.read(cx).mode(), InteractionMode::Insert) {
            self.request_submit(cx);
        }
        cx.stop_propagation();
    }

    fn request_submit(&mut self, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().to_string();
        cx.emit(ComposerEvent::SubmitRequested(text));
    }
}

#[derive(IntoElement)]
pub(super) struct Composer {
    state: Entity<ComposerView>,
}

impl Composer {
    pub(super) fn new(state: &Entity<ComposerView>) -> Self {
        Self {
            state: state.clone(),
        }
    }
}

impl View for Composer {
    fn entity_id(&self) -> Option<EntityId> {
        Some(self.state.entity_id())
    }

    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        self.state.update(cx, |state, cx| {
            // A frame may be drawn before mode observers run; synchronize before accepting input.
            state.apply_mode(state.interaction.read(cx).mode(), window, cx);
            div()
                // Capture Ctrl+Enter before the textarea inserts a newline.
                .capture_action(cx.listener(|this, action: &Enter, _, cx| {
                    this.handle_enter(action, cx);
                }))
                .flex()
                .p_4()
                .border_t_1()
                .border_color(cx.theme().border)
                .child(
                    Textarea::new(&state.input)
                        .flex_1()
                        .readonly(!state.input_editable)
                        .aria_label("Message to pi"),
                )
        })
    }
}
