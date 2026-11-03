use gpui_kit::{
    App, AppContext as _, Context, Empty, Entity, EventEmitter, Focusable as _,
    InteractiveElement as _, IntoElement, KeyBinding, ParentElement as _, Render, Role,
    StatefulInteractiveElement as _, Styled as _, Window,
    base::TestSupportExt as _,
    component::{
        ActiveTheme as _, Selectable as _,
        button::{Button, ButtonVariants as _},
        input::{
            Enter, IndentInline, Input, InputEvent, InputState, MoveDown, MoveUp, OutdentInline,
        },
    },
    div,
    prelude::FluentBuilder as _,
    px,
};

use crate::interaction::{Interaction, InteractionMode};

#[derive(Clone, Copy)]
pub(crate) enum Command {
    NewSession,
}

impl Command {
    fn name(self) -> &'static str {
        match self {
            Self::NewSession => "new",
        }
    }
}

fn matching_command(query: &str) -> Option<Command> {
    [Command::NewSession].into_iter().find(|command| {
        let mut name = command.name().chars();
        query
            .trim()
            .chars()
            .all(|wanted| name.any(|letter| letter.eq_ignore_ascii_case(&wanted)))
    })
}

pub(crate) enum CommandPaletteEvent {
    Confirmed(Command),
}

pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-p", MoveUp, Some("RockstarCommand")),
        KeyBinding::new("ctrl-n", MoveDown, Some("RockstarCommand")),
        KeyBinding::new("tab", IndentInline, Some("RockstarCommand")),
        KeyBinding::new("shift-tab", OutdentInline, Some("RockstarCommand")),
    ]);
}

pub(crate) struct CommandPalette {
    input: Entity<InputState>,
    interaction: Entity<Interaction>,
    visible: bool,
}

impl EventEmitter<CommandPaletteEvent> for CommandPalette {}

impl CommandPalette {
    pub(crate) fn new(
        interaction: Entity<Interaction>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search commands…"));
        cx.subscribe(&input, |_, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        })
        .detach();
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
        if let Some(command) = matching_command(&self.input.read(cx).value()) {
            cx.emit(CommandPaletteEvent::Confirmed(command));
        }
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        self.interaction.update(cx, |interaction, cx| {
            interaction.set_mode(InteractionMode::Normal, cx);
        });
    }

    fn keep_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // With one result, navigation keeps the same selection. Never let
        // Tab move to the message input behind the popup.
        self.input.focus_handle(cx).focus(window, cx);
        cx.stop_propagation();
    }
}

impl Render for CommandPalette {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.apply_mode(self.interaction.read(cx).mode(), window, cx);
        if !self.visible {
            return Empty.into_any_element();
        }
        let matched = matching_command(&self.input.read(cx).value());
        div()
            .id("command-overlay")
            .key_context("RockstarCommand")
            .capture_action(cx.listener(|this, _: &Enter, _, cx| {
                this.confirm(cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &MoveUp, window, cx| this.keep_focus(window, cx)))
            .capture_action(
                cx.listener(|this, _: &MoveDown, window, cx| this.keep_focus(window, cx)),
            )
            .capture_action(
                cx.listener(|this, _: &IndentInline, window, cx| this.keep_focus(window, cx)),
            )
            .capture_action(
                cx.listener(|this, _: &OutdentInline, window, cx| this.keep_focus(window, cx)),
            )
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
                    .gap_3()
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
                            .aria_label("Command search"),
                    )
                    .child(
                        div()
                            .when_some(matched, |this, command| {
                                this.child(
                                    Button::new("command-new")
                                        .ghost()
                                        .w_full()
                                        .justify_start()
                                        .label(command.name())
                                        .selected(true)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.confirm(cx);
                                        })),
                                )
                            })
                            .when(matched.is_none(), |this| {
                                this.child(
                                    div()
                                        .id("no-commands")
                                        .test_support()
                                        .aria_label("No matching commands")
                                        .py_2()
                                        .child("No matching commands"),
                                )
                            }),
                    )
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(cx.theme().muted_foreground)
                            .child(if matched.is_none() {
                                "Esc to close"
                            } else {
                                "new — Start a new session · Enter to run · Esc to close"
                            }),
                    ),
            )
            .into_any_element()
    }
}
