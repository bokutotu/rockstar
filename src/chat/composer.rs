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
    mode::Mode,
};

fn input_hint(mode: Mode) -> &'static str {
    if mode == Mode::Insert {
        "Ask pi…"
    } else {
        "NORMAL — click to type · : commands"
    }
}

pub(super) struct ComposerView {
    input: Entity<TextareaState>,
    prompts: Sender<PromptRequest>,
}

impl ComposerView {
    pub(super) fn new(
        prompts: Sender<PromptRequest>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let hint = input_hint(*cx.global::<Mode>());
        let input = cx.new(|cx| TextareaState::new(window, cx).rows(3).placeholder(hint));
        cx.observe_global_in::<Mode>(window, |this, window, cx| {
            let hint = input_hint(*cx.global::<Mode>());
            this.input
                .update(cx, |input, cx| input.set_placeholder(hint, window, cx));
            cx.notify();
        })
        .detach();
        cx.on_focus(&input.focus_handle(cx), window, |_, _, cx| {
            if *cx.global::<Mode>() == Mode::Normal {
                cx.set_global(Mode::Insert);
            }
        })
        .detach();
        Self { input, prompts }
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
        if *cx.global::<Mode>() == Mode::Insert {
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
            let mode = *cx.global::<Mode>();
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
                        .readonly(mode != Mode::Insert)
                        .aria_label("Message to pi"),
                )
        })
    }
}
