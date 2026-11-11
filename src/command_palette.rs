use std::path::PathBuf;

use gpui_kit::{
    App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Role, StatefulInteractiveElement as _, Styled as _, Task,
    WeakEntity, Window,
    component::{
        ActiveTheme as _, WindowExt as _,
        dialog::DialogEntrance,
        input::{Enter, IndentInline, Input, InputEvent, InputState, OutdentInline},
        notification::Notification,
    },
    div,
    prelude::FluentBuilder as _,
    px,
};

use crate::{
    agent::AgentKind,
    diff::{DiffRequest, IndexAction, LocalFilter},
    input_shortcuts,
    mode::Mode,
    workspace::{WipeMode, WorkspaceWindow},
};

mod completion;
mod path_completion;
use completion::{Completion, command_candidates};
use path_completion::{wsnew_argument, wsnew_candidates};

const VISIBLE_COMPLETIONS: usize = 10;

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
                Mode::Normal | Mode::Insert | Mode::Replace | Mode::VirtualReplace
                | Mode::Visual | Mode::VisualLine | Mode::VisualBlock => {
                    if window.has_active_dialog(cx) {
                        window.close_dialog(cx);
                    }
                    let _ =
                        workspace.update(cx, |workspace, cx| workspace.focus_for_mode(window, cx));
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
    completion: Option<Completion>,
    completion_task: Option<Task<()>>,
}

impl CommandPaletteView {
    fn new(
        workspace: WeakEntity<WorkspaceWindow>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Command…"));
        cx.subscribe(&input, |this, input, event: &InputEvent, cx| {
            // Our own replacements emit Change too; keep the cycle only while
            // the input still equals the candidate we just selected.
            let edited = matches!(event, InputEvent::Change)
                && this.completion.as_ref().is_none_or(|completion| {
                    completion.current() != input.read(cx).value().as_ref()
                });
            if edited || matches!(event, InputEvent::Blur) {
                this.completion = None;
                this.completion_task = None;
                cx.notify();
            }
        })
        .detach();
        Self {
            input,
            workspace,
            completion: None,
            completion_task: None,
        }
    }

    fn complete(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let input = self.input.read(cx);
        let command = input.value();
        if input.is_composing() {
            return false;
        }
        let argument = wsnew_argument(&command);
        let selection = input.selected_range();
        let before_closing_quote = argument.is_some_and(|argument| argument.starts_with('"'))
            && command.trim_end().ends_with('"')
            && input.cursor() + 1 == command.trim_end().len();
        if !selection.is_empty() || (input.cursor() != command.len() && !before_closing_quote) {
            return true;
        }
        if let Some(completion) = &mut self.completion
            && completion.current() == command.as_ref()
            && !completion.is_unique()
        {
            completion.advance(forward);
            self.apply_completion(window, cx);
            return true;
        }

        self.completion = None;
        self.completion_task = None;
        cx.notify();
        if argument.is_none() {
            self.completion = Completion::start(command_candidates(&command), forward);
            self.apply_completion(window, cx);
            return true;
        }

        // A unique path candidate already ends in '/', so the next request
        // searches inside it instead of cycling over that candidate forever.
        let Some(workspace) = self.workspace.upgrade() else {
            return true;
        };
        let root = workspace.read(cx).active_root().to_owned();
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let request = command.clone();
        let scan =
            cx.background_spawn(async move { wsnew_candidates(&request, &root, home.as_deref()) });
        self.completion_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = scan.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.completion_task = None;
                let input = this.input.read(cx);
                if *cx.global::<Mode>() != Mode::CommandPalette
                    || !input.focus_handle(cx).is_focused(window)
                    || input.is_composing()
                    || input.value() != command
                    || input.selected_range() != selection
                {
                    return;
                }
                match result {
                    Ok(candidates) => {
                        this.completion = Completion::start(candidates, forward);
                        this.apply_completion(window, cx);
                    }
                    Err(error) => window.push_notification(
                        Notification::error(format!("Path completion: {error:#}")),
                        cx,
                    ),
                }
            });
        }));
        true
    }

    fn apply_completion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.notify();
        let Some(completion) = &self.completion else {
            return;
        };
        let command = completion.current().to_owned();
        let cursor = command.len() - usize::from(command.ends_with('"'));
        self.input.update(cx, |input, cx| {
            input.replace_all(command, window, cx);
            input.set_selected_range(cursor..cursor, cx);
        });
    }

    fn render_completion(&self, cx: &Context<Self>) -> Option<impl IntoElement> {
        let completion = self.completion.as_ref().filter(|c| !c.is_unique())?;
        let first = completion
            .selected
            .saturating_sub(VISIBLE_COMPLETIONS - 1)
            .min(
                completion
                    .candidates
                    .len()
                    .saturating_sub(VISIBLE_COMPLETIONS),
            );
        let theme = cx.theme();
        let rows = completion
            .candidates
            .iter()
            .enumerate()
            .skip(first)
            .take(VISIBLE_COMPLETIONS)
            .map(|(index, candidate)| {
                let selected = index == completion.selected;
                div()
                    .id(("command-completion", index))
                    .role(Role::ListBoxOption)
                    .aria_label(candidate.clone())
                    .aria_selected(selected)
                    .px_2()
                    .py_0p5()
                    .rounded_sm()
                    .truncate()
                    .when(selected, |row| {
                        row.bg(theme.accent).text_color(theme.accent_foreground)
                    })
                    .child(candidate.clone())
            });
        Some(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .id("command-completions")
                        .role(Role::ListBox)
                        .aria_label("Command completions")
                        .p_1()
                        .rounded_md()
                        .border_1()
                        .border_color(theme.border)
                        .children(rows),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!(
                            "{} / {} · Tab / Shift+Tab to select · Enter to run",
                            completion.selected + 1,
                            completion.candidates.len(),
                        )),
                ),
        )
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.completion_task = None;
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
            "unstage" => tab.update(cx, |tab, cx| {
                tab.update_diff_index(IndexAction::Unstage, cx)
            }),
            "e!" => tab.update(cx, |tab, cx| tab.reload_active_file(window, cx)),
            "markdown" => tab.update(cx, |tab, cx| tab.toggle_markdown(window, cx)),
            "pi" | "agy" | "codex" => {
                let kind = match name.to_ascii_lowercase().as_str() {
                    "agy" => AgentKind::Agy,
                    "codex" => AgentKind::Codex,
                    _ => AgentKind::Pi,
                };
                tab.update(cx, |workspace, cx| workspace.show_chat(kind, window, cx));
                Ok(())
            }
            "resume" => {
                // Replace the palette dialog before opening the session picker.
                window.close_dialog(cx);
                tab.update(cx, |tab, cx| tab.open_resume(window, cx));
                return;
            }
            "term" => {
                // Dismiss before opening: an existing terminal can enter Insert
                // immediately without the palette resetting it to Normal.
                self.dismiss(cx);
                tab.update(cx, |workspace, cx| {
                    workspace.show_terminal(crate::terminal::TerminalKind::Shell, window, cx)
                });
                return;
            }
            "new" => {
                tab.update(cx, |workspace, cx| workspace.new_chat(window, cx));
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
                window.push_notification(Notification::error(error.to_string()), cx)
            }
        }
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        self.completion = None;
        self.completion_task = None;
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
            .flex()
            .flex_col()
            .gap_2()
            .capture_action(cx.listener(|this, _: &IndentInline, window, cx| {
                if this.complete(true, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &OutdentInline, window, cx| {
                if this.complete(false, window, cx) {
                    cx.stop_propagation();
                }
            }))
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
            .children(self.render_completion(cx))
    }
}
