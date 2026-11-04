use async_channel::Sender;
use gpui_kit::{
    App, AppContext as _, Context, Entity, EntityId, Focusable as _, InteractiveElement as _,
    IntoElement, ParentElement as _, Styled as _, View, Window,
    component::{
        ActiveTheme as _, WindowExt as _,
        input::{Enter, Textarea, TextareaState},
        notification::Notification,
    },
    div,
};

use crate::{
    agent::{DISCONNECTED_MESSAGE, PromptRequest},
    interaction::{Interaction, InteractionMode},
};

fn input_hint(mode: InteractionMode) -> &'static str {
    if matches!(mode, InteractionMode::Insert) {
        "Ask pi…"
    } else {
        "NORMAL — click to type · : commands"
    }
}

pub(super) struct ComposerView {
    input: Entity<TextareaState>,
    interaction: Entity<Interaction>,
    prompts: Sender<PromptRequest>,
}

impl ComposerView {
    pub(super) fn new(
        prompts: Sender<PromptRequest>,
        interaction: Entity<Interaction>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let hint = input_hint(interaction.read(cx).mode());
        let input = cx.new(|cx| TextareaState::new(window, cx).rows(3).placeholder(hint));
        cx.observe_in(&interaction, window, |this, interaction, window, cx| {
            let hint = input_hint(interaction.read(cx).mode());
            this.input
                .update(cx, |input, cx| input.set_placeholder(hint, window, cx));
            cx.notify();
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
        Self {
            input,
            interaction,
            prompts,
        }
    }

    pub(super) fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
        cx.notify();
    }

    fn handle_enter(&mut self, action: &Enter, window: &mut Window, cx: &mut Context<Self>) {
        if !action.secondary {
            return;
        }
        if matches!(self.interaction.read(cx).mode(), InteractionMode::Insert) {
            self.submit(window, cx);
        }
        cx.stop_propagation();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().to_string();
        if text.trim().is_empty() {
            return;
        }
        match self.prompts.try_send(PromptRequest { text }) {
            Ok(()) => self.clear(window, cx),
            Err(_) => window.push_notification(
                Notification::error(DISCONNECTED_MESSAGE).autohide(false),
                cx,
            ),
        }
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

    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        self.state.update(cx, |state, cx| {
            let mode = state.interaction.read(cx).mode();
            div()
                // Capture Ctrl+Enter before the textarea inserts a newline.
                .capture_action(cx.listener(|this, action: &Enter, window, cx| {
                    this.handle_enter(action, window, cx);
                }))
                .flex()
                .p_4()
                .border_t_1()
                .border_color(cx.theme().border)
                .child(
                    Textarea::new(&state.input)
                        .flex_1()
                        .readonly(!matches!(mode, InteractionMode::Insert))
                        .aria_label("Message to pi"),
                )
        })
    }
}
