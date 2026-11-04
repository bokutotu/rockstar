use async_channel::Sender;
use gpui_kit::{
    App, AppContext as _, Context, Empty, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, RenderOnce, Role, StatefulInteractiveElement as _, Styled as _,
    Window,
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
    mode::Mode,
};

/// Mounts the palette view only while command-palette mode is active.
#[derive(IntoElement)]
pub(crate) struct CommandPaletteHost {
    new_sessions: Sender<NewSessionRequest>,
}

impl CommandPaletteHost {
    pub(crate) fn new(new_sessions: Sender<NewSessionRequest>) -> Self {
        Self { new_sessions }
    }
}

impl RenderOnce for CommandPaletteHost {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        if *cx.global::<Mode>() != Mode::CommandPalette {
            return Empty.into_any_element();
        }
        window
            .use_keyed_state("command-palette", cx, |window, cx| {
                CommandPaletteView::new(self.new_sessions, window, cx)
            })
            .into_any_element()
    }
}

/// The input and UI retained for one open/close cycle of the palette.
struct CommandPaletteView {
    input: Entity<InputState>,
    new_sessions: Sender<NewSessionRequest>,
}

impl CommandPaletteView {
    fn new(
        new_sessions: Sender<NewSessionRequest>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Command…"));
        Self {
            input,
            new_sessions,
        }
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let command = self.input.read(cx).value();
        if command.trim().eq_ignore_ascii_case("new") {
            match self.new_sessions.try_send(NewSessionRequest) {
                Ok(()) => self.dismiss(cx),
                Err(_) => window.push_notification(
                    Notification::error(DISCONNECTED_MESSAGE).autohide(false),
                    cx,
                ),
            }
        }
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        cx.set_global(Mode::Normal);
    }
}

impl Render for CommandPaletteView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
    }
}
