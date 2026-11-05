use gpui_kit::{
    App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Role, StatefulInteractiveElement as _, Styled as _, WeakEntity,
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

use crate::{input_shortcuts, mode::Mode, workspace::Workspace};

pub(crate) fn bind_mode(workspace: &Entity<Workspace>, window: &mut Window, cx: &mut App) {
    workspace.update(cx, |workspace, cx| workspace.focus_active(window, cx));
    let target_window = window.window_handle();
    let weak = workspace.downgrade();
    cx.intercept_keystrokes(move |event, window, cx| {
        if window.window_handle() == target_window
            && weak
                .update(cx, |workspace, cx| {
                    workspace.key(&event.keystroke, window, cx)
                })
                .unwrap_or(false)
        {
            cx.stop_propagation();
        }
    })
    .detach();
    let workspace = workspace.downgrade();
    window
        .observe_global::<Mode>(cx, move |window, cx| {
            match *cx.global::<Mode>() {
                Mode::CommandPalette => {
                    if !window.has_active_dialog(cx) {
                        open(workspace.clone(), window, cx);
                    }
                }
                Mode::FileFinder => {
                    if !window.has_active_dialog(cx) {
                        let _ =
                            workspace.update(cx, |workspace, cx| workspace.open_finder(window, cx));
                    }
                }
                Mode::Normal | Mode::Insert | Mode::Visual | Mode::VisualLine => {
                    if window.has_active_dialog(cx) {
                        window.close_dialog(cx);
                    }
                    let _ =
                        workspace.update(cx, |workspace, cx| workspace.focus_active(window, cx));
                }
                Mode::Search | Mode::LspPopup | Mode::Confirm => {}
            }
            let _ = workspace.update(cx, |_, cx| cx.notify());
        })
        .detach();
}

fn open(workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut App) {
    let palette = cx.new(|cx| CommandPaletteView::new(workspace, window, cx));
    let focus = palette.read(cx).input.focus_handle(cx);
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
    focus.focus(window, cx);
}

/// The input and UI retained for one open/close cycle of the palette.
struct CommandPaletteView {
    input: Entity<InputState>,
    workspace: WeakEntity<Workspace>,
}

impl CommandPaletteView {
    fn new(workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Command…"));
        Self { input, workspace }
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let command = self.input.read(cx).value();
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let result = match command.trim().to_ascii_lowercase().as_str() {
            "chat" => {
                workspace.update(cx, |workspace, cx| workspace.show_chat(false, window, cx));
                Ok(())
            }
            "new" => {
                workspace.update(cx, |workspace, cx| workspace.show_chat(true, window, cx));
                Ok(())
            }
            "close" | "q" => {
                workspace.update(cx, |workspace, cx| workspace.close_active_pane(window, cx))
            }
            "w" | "write" => {
                workspace.update(cx, |workspace, cx| workspace.save_active(window, cx))
            }
            "quit" | "qa" => {
                if workspace.update(cx, |workspace, cx| workspace.request_close(window, cx)) {
                    window.remove_window();
                }
                return;
            }
            _ => {
                window.push_notification(
                    Notification::error(format!("Unknown command: {}", command.trim())),
                    cx,
                );
                return;
            }
        };
        match result {
            Ok(()) => self.dismiss(cx),
            Err(error) => {
                window.push_notification(Notification::error(error.to_string()).autohide(false), cx)
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
            .key_context(input_shortcuts::CONTEXT)
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
