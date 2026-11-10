use super::*;
use crate::terminal::{TerminalKind, TerminalSession};

impl Tab {
    pub(crate) fn show_terminal(&mut self, kind: TerminalKind, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.panes[self.active].read(cx).terminal()
            && view.read(cx).session.read(cx).kind == kind
        {
            cx.set_global(Mode::Insert);
            view.update(cx, |view, cx| view.focus(window, cx));
            return;
        }
        let pane = self.panes[self.active].clone();
        let pane_id = pane.entity_id();
        let request = pane.update(cx, |pane, _| {
            pane.request += 1;
            pane.request
        });
        let root = self.root.clone();
        let command = kind.command();
        let starting = cx.background_spawn(async move { TerminalSession::spawn(&root, command) });
        cx.spawn_in(window, async move |this, cx| {
            let result = starting.await;
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(pane) = this
                    .panes
                    .iter()
                    .find(|pane| pane.entity_id() == pane_id && pane.read(cx).request == request)
                    .cloned()
                else {
                    // Dropping a late result also stops its process.
                    return;
                };
                match result {
                    Ok(process) => {
                        let number = this
                            .items
                            .iter()
                            .filter_map(|item| match item {
                                OpenItem::Terminal(session) => Some(session.read(cx).number),
                                _ => None,
                            })
                            .max()
                            .unwrap_or(0)
                            + 1;
                        let session =
                            cx.new(|cx| TerminalSession::new(number, kind, process, window, cx));
                        let id = this.register_item(OpenItem::Terminal(session), cx);
                        this.show_item(pane_id, id, window, cx);
                        // A background completion must not steal focus from a
                        // different pane, the finder, or another command.
                        if this.visible && this.panes[this.active] == pane && *cx.global::<Mode>() == Mode::Normal {
                            cx.set_global(Mode::Insert);
                            this.focus_active(window, cx);
                        }
                    }
                    Err(error) => window.push_notification(
                        Notification::error(format!("{error:#}")),
                        cx,
                    ),
                }
            });
        })
        .detach();
    }
}
