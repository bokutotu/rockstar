use async_channel::Sender;
use gpui_kit::{
    AppContext as _, Context, Empty, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Role, StatefulInteractiveElement as _, Styled as _, Window,
    base::TestSupportExt as _,
    component::{
        ActiveTheme as _, WindowExt as _,
        input::{Enter, Input, InputState},
        notification::Notification,
    },
    div, px,
};

use crate::{
    agent::{DISCONNECTED_MESSAGE, NewSessionRequest},
    interaction::{Interaction, InteractionMode},
};

pub(crate) struct CommandPalette {
    input: Entity<InputState>,
    interaction: Entity<Interaction>,
    new_sessions: Sender<NewSessionRequest>,
}

impl CommandPalette {
    pub(crate) fn new(
        new_sessions: Sender<NewSessionRequest>,
        interaction: Entity<Interaction>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Command…"));
        cx.observe(&interaction, |_, _, cx| cx.notify()).detach();
        Self {
            input,
            interaction,
            new_sessions,
        }
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let command = self.input.read(cx).value();
        if command.trim().eq_ignore_ascii_case("new") {
            match self.new_sessions.try_send(NewSessionRequest) {
                Ok(()) => {
                    self.input
                        .update(cx, |input, cx| input.set_value("", window, cx));
                    self.dismiss(cx);
                }
                Err(_) => window.push_notification(
                    Notification::error(DISCONNECTED_MESSAGE).autohide(false),
                    cx,
                ),
            }
        }
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        self.interaction.update(cx, |interaction, cx| {
            interaction.set_mode(InteractionMode::Normal, cx);
        });
    }
}

impl Render for CommandPalette {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !matches!(
            self.interaction.read(cx).mode(),
            InteractionMode::CommandPalette
        ) {
            return Empty.into_any_element();
        }
        div()
            .id("command-overlay")
            .capture_action(cx.listener(|this, action: &Enter, window, cx| {
                if !action.secondary {
                    this.confirm(window, cx);
                }
                cx.stop_propagation();
            }))
            .absolute()
            .inset_0()
            .occlude()
            .bg(gpui_kit::black().opacity(0.25))
            .flex()
            .items_center()
            .justify_center()
            .p_4()
            .on_mouse_down(
                gpui_kit::MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.dismiss(cx);
                    cx.stop_propagation();
                }),
            )
            .child(
                div()
                    .id("command-popup")
                    .test_support()
                    .role(Role::Dialog)
                    .aria_label("Commands")
                    .w_full()
                    .max_w(px(480.0))
                    .flex()
                    .flex_col()
                    .p_4()
                    .rounded_lg()
                    .shadow_lg()
                    .bg(cx.theme().popover)
                    .text_color(cx.theme().popover_foreground)
                    .border_1()
                    .border_color(cx.theme().border)
                    .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation()
                    })
                    .child(
                        Input::new(&self.input)
                            .prefix(div().child(":"))
                            .aria_label("Command"),
                    ),
            )
            .into_any_element()
    }
}
