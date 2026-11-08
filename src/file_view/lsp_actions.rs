use super::*;
use crate::lsp::{DocumentSnapshot, language::Encoding};
use gpui_kit::base::Disableable as _;
use gpui_kit::component::{button::Button, dialog::DialogEntrance};
use serde_json::json;

impl FileView {
    pub(super) fn rename_symbol(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self
            .buffer
            .read(cx)
            .lsp
            .clone()
            .filter(|d| d.read(cx).session.supports("renameProvider"))
        else {
            self.vim_error(
                "Rename is not supported by this language server",
                window,
                cx,
            );
            return;
        };
        let Ok(snap) = doc.read(cx).snapshot(cx) else {
            return;
        };
        let cursor = self.vim.cursor;
        let encoding = doc.read(cx).session.encoding();
        let Ok(position) = encoding.position(&snap.text, cursor) else {
            return;
        };
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("New name"));
        let focus = input.focus_handle(cx);
        let weak = cx.entity().downgrade();
        cx.set_global(Mode::Confirm);
        window.open_dialog(cx, move |dialog, _, _| {
            let input = input.clone();
            let weak = weak.clone();
            let doc = doc.clone();
            let snap = snap.clone();
            dialog
                .centered()
                .w(px(440.))
                .title("Rename symbol")
                .entrance(DialogEntrance::None)
                .child(Input::new(&input))
                .on_ok(move |_, window, cx| {
                    let name = input.read(cx).value().to_string();
                    if name.trim().is_empty() {
                        return false;
                    }
                    let doc = doc.clone();
                    let snap = snap.clone();
                    let _ = weak.update(cx, |this, cx| {
                        this.set_mode(Mode::Normal, cx);
                        let response = doc.update(cx, |doc, cx| {
                            doc.request(
                                "textDocument/rename",
                                &snap,
                                json!({"position":position,"newName":name}),
                                cx,
                            )
                        });
                        match response {
                            Ok(response) => {
                                cx.spawn_in(window, async move |this, cx| {
                                    let result = response.recv().await.and_then(|v| {
                                        Ok(serde_json::from_value::<
                                            Option<lsp_types::WorkspaceEdit>,
                                        >(v)?)
                                    });
                                    let _ = this.update_in(cx, |this, window, cx| {
                                        if !doc.read(cx).is_current(&snap, cx) {
                                            this.vim_error(
                                                "Rename discarded: document changed",
                                                window,
                                                cx,
                                            );
                                            return;
                                        }
                                        match result {
                                            Ok(Some(edit)) => {
                                                cx.emit(VimEvent::ApplyEdit(edit, encoding))
                                            }
                                            Ok(None) => window.push_notification(
                                                Notification::info("Nothing to rename"),
                                                cx,
                                            ),
                                            Err(error) => this.vim_error(error, window, cx),
                                        }
                                    });
                                })
                                .detach();
                            }
                            Err(error) => this.vim_error(error, window, cx),
                        }
                    });
                    true
                })
                .on_close(|_, _, cx| cx.set_global(Mode::Normal))
        });
        focus.focus(window, cx);
    }
    pub(super) fn code_actions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self
            .buffer
            .read(cx)
            .lsp
            .clone()
            .filter(|d| d.read(cx).session.supports("codeActionProvider"))
        else {
            self.vim_error(
                "Code actions are not supported by this language server",
                window,
                cx,
            );
            return;
        };
        let Ok(snap) = doc.read(cx).snapshot(cx) else {
            return;
        };
        let encoding = doc.read(cx).session.encoding();
        let range = if self.vim.mode.is_visual() {
            let target = visual_target(&snap.text, &self.vim, self.indent);
            target.ranges[0].start..target.ranges.last().unwrap().end
        } else {
            self.vim.cursor..self.vim.cursor
        };
        let (Ok(start), Ok(end)) = (
            encoding.position(&snap.text, range.start),
            encoding.position(&snap.text, range.end),
        ) else {
            return;
        };
        let diagnostics: Vec<_> = doc
            .read(cx)
            .analysis
            .diagnostics
            .value
            .iter()
            .filter(|d| d.range.start <= end && start <= d.range.end)
            .cloned()
            .collect();
        let response = doc.update(cx, |doc, cx| doc.request("textDocument/codeAction", &snap, json!({"range":{"start":start,"end":end},"context":{"diagnostics":diagnostics,"triggerKind":1}}), cx));
        let Ok(response) = response else {
            self.vim_error("Cannot request code actions", window, cx);
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let result = response.recv().await.and_then(|v| {
                Ok(serde_json::from_value::<
                    Option<Vec<lsp_types::CodeActionOrCommand>>,
                >(v)?)
            });
            let _ = this.update_in(cx, |this, window, cx| {
                if !doc.read(cx).is_current(&snap, cx) {
                    return;
                }
                match result {
                    Ok(Some(actions)) if !actions.is_empty() => {
                        this.show_code_actions(actions, snap, encoding, window, cx)
                    }
                    Ok(_) => window.push_notification(Notification::info("No code actions"), cx),
                    Err(error) => this.vim_error(error, window, cx),
                }
            });
        })
        .detach();
    }
    fn show_code_actions(
        &mut self,
        actions: Vec<lsp_types::CodeActionOrCommand>,
        snap: DocumentSnapshot,
        encoding: Encoding,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let weak = cx.entity().downgrade();
        cx.set_global(Mode::Confirm);
        window.open_dialog(cx, move |dialog, _, _| {
            let buttons = actions.iter().enumerate().map(|(i, action)| {
                let (title, disabled) = match action {
                    lsp_types::CodeActionOrCommand::CodeAction(a) => {
                        (a.title.clone(), a.disabled.is_some())
                    }
                    lsp_types::CodeActionOrCommand::Command(a) => (a.title.clone(), false),
                };
                let weak = weak.clone();
                let action = action.clone();
                let snap = snap.clone();
                Button::new(("code-action", i))
                    .label(title)
                    .disabled(disabled)
                    .on_click(move |_, window, cx| {
                        window.close_dialog(cx);
                        let _ = weak.update(cx, |this, cx| {
                            this.set_mode(Mode::Normal, cx);
                            this.apply_code_action(
                                action.clone(),
                                snap.clone(),
                                encoding,
                                window,
                                cx,
                            );
                        });
                    })
            });
            dialog
                .centered()
                .w(px(560.))
                .title("Code actions")
                .entrance(DialogEntrance::None)
                .child(div().flex().flex_col().gap_1().children(buttons))
                .on_close(|_, _, cx| cx.set_global(Mode::Normal))
        });
    }
    fn apply_code_action(
        &mut self,
        action: lsp_types::CodeActionOrCommand,
        snap: DocumentSnapshot,
        encoding: Encoding,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(doc) = self.buffer.read(cx).lsp.clone() else {
            return;
        };
        if !doc.read(cx).is_current(&snap, cx) {
            self.vim_error("Code action discarded: document changed", window, cx);
            return;
        }
        match action {
            lsp_types::CodeActionOrCommand::CodeAction(action) if action.command.is_none() && action.edit.is_some() => cx.emit(VimEvent::ApplyEdit(action.edit.unwrap(), encoding)),
            lsp_types::CodeActionOrCommand::CodeAction(action) if action.edit.is_none() && action.command.is_none() && action.data.is_some() => {
                let response = doc.read(cx).session.request("codeAction/resolve", serde_json::to_value(action).unwrap());
                cx.spawn_in(window, async move |this, cx| {
                    let result = response.recv().await.and_then(|v| Ok(serde_json::from_value::<lsp_types::CodeAction>(v)?));
                    let _ = this.update_in(cx, |this, window, cx| match result {
                        Ok(mut action) => { action.data = None; this.apply_code_action(lsp_types::CodeActionOrCommand::CodeAction(action), snap, encoding, window, cx); }
                        Err(error) => this.vim_error(error, window, cx),
                    });
                }).detach();
            }
            _ => self.vim_error("This code action requires a server command; only document-edit actions are supported", window, cx),
        }
    }
}
