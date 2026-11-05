use async_channel::Sender;
use gpui_kit::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable as _, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, Role, StatefulInteractiveElement as _, Styled as _,
    Window,
    base::TestSupportExt as _,
    component::{
        ActiveTheme as _, WindowExt as _,
        dialog::DialogEntrance,
        input::{Enter, Input, InputState},
        notification::Notification,
    },
    div, px,
};

use crate::{
    agent::{DISCONNECTED_MESSAGE, NewSessionRequest},
    mode::Mode,
};

pub(crate) fn bind_mode(
    new_sessions: Sender<NewSessionRequest>,
    normal_focus: FocusHandle,
    window: &mut Window,
    cx: &mut App,
) {
    if *cx.global::<Mode>() == Mode::Normal {
        normal_focus.focus(window, cx);
    }
    window
        .observe_global::<Mode>(cx, move |window, cx| match *cx.global::<Mode>() {
            Mode::CommandPalette => {
                if !window.has_active_dialog(cx) {
                    open(new_sessions.clone(), window, cx);
                }
            }
            Mode::Normal => {
                if window.has_active_dialog(cx) {
                    window.close_dialog(cx);
                }
                normal_focus.focus(window, cx);
            }
            Mode::Insert => {}
        })
        .detach();
}

fn open(new_sessions: Sender<NewSessionRequest>, window: &mut Window, cx: &mut App) {
    let palette = cx.new(|cx| CommandPaletteView::new(new_sessions, window, cx));
    window.open_dialog(cx, move |dialog, _, cx| {
        dialog
            .centered()
            .w(px(480.0))
            .min_h_0()
            .p_4()
            .rounded_lg()
            .bg(cx.theme().popover)
            .text_color(cx.theme().popover_foreground)
            .close_button(false)
            .entrance(DialogEntrance::None)
            .on_ok(|_, _, _| false)
            .on_close(|_, _, cx| cx.set_global(Mode::Normal))
            .child(palette.clone())
    });
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.input.focus_handle(cx).focus(window, cx);

        div()
            .id("command-popup")
            .test_support()
            .role(Role::Group)
            .aria_label("Commands")
            .w_full()
            .capture_action(cx.listener(|this, action: &Enter, window, cx| {
                if !action.secondary {
                    this.confirm(window, cx);
                }
                cx.stop_propagation();
            }))
            .child(
                Input::new(&self.input)
                    .prefix(div().child(":"))
                    .aria_label("Command"),
            )
    }
}
