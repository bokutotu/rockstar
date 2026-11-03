use gpui_kit::{
    AppContext as _, Context, Empty, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Role, StatefulInteractiveElement as _, Styled as _, Window,
    base::TestSupportExt as _,
    component::{
        ActiveTheme as _,
        input::{Enter, Input, InputState},
    },
    div, px,
};

use crate::interaction::{Interaction, InteractionMode};

pub(crate) enum Command {
    NewSession,
}

pub(crate) struct CommandPalette {
    input: Entity<InputState>,
    interaction: Entity<Interaction>,
    visible: bool,
}

impl EventEmitter<Command> for CommandPalette {}

impl CommandPalette {
    pub(crate) fn new(
        interaction: Entity<Interaction>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Command…"));
        cx.observe_in(&interaction, window, |this, interaction, window, cx| {
            this.apply_mode(interaction.read(cx).mode(), window, cx);
        })
        .detach();
        let mode = interaction.read(cx).mode();
        let mut view = Self {
            input,
            interaction,
            visible: false,
        };
        view.apply_mode(mode, window, cx);
        view
    }

    fn apply_mode(&mut self, mode: InteractionMode, window: &mut Window, cx: &mut Context<Self>) {
        let visible = matches!(mode, InteractionMode::CommandPalette);
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible {
            self.input.update(cx, |input, cx| {
                input.set_value("", window, cx);
                input.focus(window, cx);
            });
        }
        cx.notify();
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        let command = self.input.read(cx).value();
        if command.trim().eq_ignore_ascii_case("new") {
            cx.emit(Command::NewSession);
        }
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        self.interaction.update(cx, |interaction, cx| {
            interaction.set_mode(InteractionMode::Normal, cx);
        });
    }
}

impl Render for CommandPalette {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.apply_mode(self.interaction.read(cx).mode(), window, cx);
        if !self.visible {
            return Empty.into_any_element();
        }
        div()
            .id("command-overlay")
            .capture_action(cx.listener(|this, action: &Enter, _, cx| {
                if !action.secondary {
                    this.confirm(cx);
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
