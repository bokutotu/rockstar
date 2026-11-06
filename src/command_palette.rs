use gpui_kit::{
    App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Role, StatefulInteractiveElement as _, Styled as _, WeakEntity,
    Window,
    component::{
        ActiveTheme as _, WindowExt as _,
        dialog::DialogEntrance,
        input::{Enter, Input, InputState},
        notification::Notification,
    },
    div, px,
};

use crate::{
    input_shortcuts,
    diff::{DiffRequest, IndexAction, LocalFilter},
    mode::Mode,
    workspace::{WipeMode, WorkspaceWindow},
};

pub(crate) fn bind_mode(workspace: &Entity<WorkspaceWindow>, window: &mut Window, cx: &mut App) {
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
                Mode::Search | Mode::LspPopup | Mode::Confirm | Mode::Switcher => {}
            }
            let _ = workspace.update(cx, |_, cx| cx.notify());
        })
        .detach();
}

fn open(workspace: WeakEntity<WorkspaceWindow>, window: &mut Window, cx: &mut App) {
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
    workspace: WeakEntity<WorkspaceWindow>,
}

impl CommandPaletteView {
    fn new(
        workspace: WeakEntity<WorkspaceWindow>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Command…"));
        Self { input, workspace }
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let command = self.input.read(cx).value();
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let command = command.trim();
        let (name, argument) = command
            .split_once(char::is_whitespace)
            .unwrap_or((command, ""));
        if !argument.trim().is_empty() && !name.eq_ignore_ascii_case("wsnew") {
            window.push_notification(
                Notification::error(format!("Unexpected argument: {argument}")),
                cx,
            );
            return;
        }
        let tab = workspace.read(cx).active_tab();
        let result = match name.to_ascii_lowercase().as_str() {
            "wsnew" => workspace.update(cx, |workspace, cx| {
                workspace.open_workspace(argument, window, cx)
            }),
            "tabnew" => {
                workspace.update(cx, |workspace, cx| workspace.new_tab(window, cx));
                Ok(())
            }
            "wsclose" => {
                workspace.update(cx, |workspace, cx| {
                    workspace.request_close_workspace(window, cx)
                });
                return;
            }
            "tabclose" => {
                workspace.update(cx, |workspace, cx| workspace.request_close_tab(window, cx));
                return;
            }
            "diff" | "diffstaged" | "diffunstaged" | "prdiff" => {
                let request = match name.to_ascii_lowercase().as_str() {
                    "diffstaged" => DiffRequest::Local(LocalFilter::Staged),
                    "diffunstaged" => DiffRequest::Local(LocalFilter::Unstaged),
                    "prdiff" => DiffRequest::PullRequest,
                    _ => DiffRequest::Local(LocalFilter::All),
                };
                workspace.update(cx, |workspace, cx| workspace.open_diff(request, window, cx))
            }
            "stage" => tab.update(cx, |tab, cx| tab.update_diff_index(IndexAction::Stage, cx)),
            "unstage" => tab.update(cx, |tab, cx| tab.update_diff_index(IndexAction::Unstage, cx)),
            "e!" => tab.update(cx, |tab, cx| tab.reload_active_file(window, cx)),
            "chat" => {
                tab.update(cx, |workspace, cx| workspace.show_chat(false, window, cx));
                Ok(())
            }
            "term" => {
                // Dismiss before opening: an existing terminal can enter Insert
                // immediately without the palette resetting it to Normal.
                self.dismiss(cx);
                tab.update(cx, |workspace, cx| workspace.show_terminal(window, cx));
                return;
            }
            "new" => {
                tab.update(cx, |workspace, cx| workspace.show_chat(true, window, cx));
                Ok(())
            }
            "close" | "q" => {
                tab.update(cx, |workspace, cx| workspace.close_active_pane(window, cx))
            }
            "bw" => tab.update(cx, |workspace, cx| {
                workspace.wipe_active_item(WipeMode::Safe, window, cx)
            }),
            "bw!" => tab.update(cx, |workspace, cx| {
                workspace.wipe_active_item(WipeMode::Force, window, cx)
            }),
            "w" | "write" => tab.update(cx, |workspace, cx| workspace.save_active(window, cx)),
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
