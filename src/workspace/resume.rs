//! The resume picker does not change a conversation until a selection is opened.
use gpui_kit::{
    Task, WeakEntity,
    component::input::{Enter, Input, InputEvent, InputState, MoveDown, MoveUp},
};

use super::*;
use crate::agent::SavedSession;

impl Tab {
    pub(crate) fn open_resume(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = &self.panes[self.active];
        let target = pane.entity_id();
        let request = pane.update(cx, |pane, _| {
            pane.request += 1;
            pane.request
        });
        cx.set_global(Mode::Switcher);
        let owner = cx.entity().downgrade();
        let root = self.root.clone();
        let picker = cx.new(|cx| ResumePicker::new(owner, target, request, root, window, cx));
        let focus = picker.read(cx).input.focus_handle(cx);
        window.open_dialog(cx, move |dialog, _, cx| {
            dialog
                .centered()
                .w(px(640.))
                .min_h_0()
                .p_3()
                .rounded_lg()
                .bg(cx.theme().popover)
                .text_color(cx.theme().popover_foreground)
                .title("Resume chat")
                .close_button(false)
                .entrance(DialogEntrance::None)
                .on_ok(|_, _, _| false)
                .on_close(|_, _, cx| cx.set_global(Mode::Normal))
                .child(picker.clone())
        });
        focus.focus(window, cx);
    }

    fn resume_from_picker(
        &mut self,
        target: EntityId,
        request: u64,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Do not apply a stale selection to a different item or a closed pane.
        if self.visible
            && self
                .panes
                .iter()
                .any(|pane| pane.entity_id() == target && pane.read(cx).request == request)
        {
            window.push_notification(Notification::info("Resuming chat…"), cx);
            self.start_chat(target, ChatStart::Resume(path), None, window, cx);
        }
    }
}

fn filter_sessions(sessions: &[SavedSession], query: &str) -> Vec<usize> {
    let query = query.to_lowercase();
    let words: Vec<_> = query.split_whitespace().collect();
    sessions
        .iter()
        .enumerate()
        .filter_map(|(index, session)| {
            let title = session.title.to_lowercase();
            words
                .iter()
                .all(|word| title.contains(word))
                .then_some(index)
        })
        .collect()
}

struct ResumePicker {
    owner: WeakEntity<Tab>,
    target: EntityId,
    request: u64,
    input: Entity<InputState>,
    sessions: Vec<SavedSession>,
    matches: Vec<usize>,
    selected: usize,
    loading: bool,
    error: Option<String>,
    _scan: Task<()>,
}

impl ResumePicker {
    fn new(
        owner: WeakEntity<Tab>,
        target: EntityId,
        request: u64,
        root: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find saved chats…"));
        cx.subscribe(&input, |this: &mut Self, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.filter(cx);
            }
        })
        .detach();
        let scan = cx.background_spawn(async move { crate::pi::list_sessions(&root) });
        let task = cx.spawn(async move |this, cx| {
            let result = scan.await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(sessions) => {
                        this.sessions = sessions;
                        this.filter(cx);
                    }
                    Err(error) => this.error = Some(format!("{error:#}")),
                }
                cx.notify();
            });
        });
        Self {
            owner,
            target,
            request,
            input,
            sessions: Vec::new(),
            matches: Vec::new(),
            selected: 0,
            loading: true,
            error: None,
            _scan: task,
        }
    }

    fn filter(&mut self, cx: &mut Context<Self>) {
        self.matches = filter_sessions(&self.sessions, &self.input.read(cx).value());
        self.selected = 0;
        cx.notify();
    }

    fn move_selection(&mut self, forward: bool, cx: &mut Context<Self>) {
        if !self.matches.is_empty() {
            self.selected = cycle_index(self.selected, self.matches.len(), forward);
            cx.notify();
        }
    }

    fn accept(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(&index) = self.matches.get(index) else {
            return;
        };
        let path = self.sessions[index].path.clone();
        cx.set_global(Mode::Normal);
        let _ = self.owner.update(cx, |owner, cx| {
            owner.resume_from_picker(self.target, self.request, path, window, cx);
        });
    }
}

impl Render for ResumePicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let status = if self.loading {
            "Loading sessions…".into()
        } else if let Some(error) = &self.error {
            format!("Could not load sessions: {error}")
        } else {
            format!(
                "{} matches · Enter to resume · Esc to cancel",
                self.matches.len()
            )
        };
        div()
            .id("resume-picker")
            .role(Role::Group)
            .aria_label("Resume a saved chat")
            .key_context("FilePicker")
            .w_full()
            .flex()
            .flex_col()
            .gap_1()
            .capture_action(cx.listener(|this, _: &MoveDown, _, cx| {
                this.move_selection(true, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &MoveUp, _, cx| {
                this.move_selection(false, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, action: &Enter, window, cx| {
                if !action.secondary {
                    this.accept(this.selected, window, cx);
                }
                cx.stop_propagation();
            }))
            .child(Input::new(&self.input).aria_label("Session query"))
            .child(div().text_color(cx.theme().muted_foreground).child(status))
            .when(
                !self.loading && self.error.is_none() && self.matches.is_empty(),
                |this| {
                    this.child(if self.sessions.is_empty() {
                        "No saved sessions for this workspace"
                    } else {
                        "No matching sessions"
                    })
                },
            )
            .children(
                self.matches
                    .iter()
                    .enumerate()
                    .skip(self.selected.saturating_sub(9))
                    .take(10)
                    .map(|(index, candidate)| {
                        let session = &self.sessions[*candidate];
                        let updated = session
                            .updated_at
                            .with_timezone(&chrono::Local)
                            .format("%Y-%m-%d %H:%M")
                            .to_string();
                        div()
                            .id(("resume-candidate", index))
                            .role(Role::ListBoxOption)
                            .aria_label(format!("{} · {updated}", session.title))
                            .aria_selected(index == self.selected)
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .cursor_pointer()
                            .flex()
                            .items_center()
                            .gap_2()
                            .when(index == self.selected, |this| {
                                this.bg(cx.theme().accent)
                                    .text_color(cx.theme().accent_foreground)
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(session.title.clone()),
                            )
                            .child(div().flex_shrink_0().text_xs().child(updated))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.accept(index, window, cx);
                            }))
                    }),
            )
            .when_some(self.matches.get(self.selected), |this, index| {
                // The file path distinguishes sessions with the same title/date.
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .truncate()
                        .child(self.sessions[*index].path.display().to_string()),
                )
            })
    }
}
