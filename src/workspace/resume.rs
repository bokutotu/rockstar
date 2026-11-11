//! The resume picker does not change a conversation until a selection is opened.
use gpui_kit::{
    Task, WeakEntity,
    component::input::{Enter, Input, InputEvent, InputState, MoveDown, MoveUp},
};

use super::*;
use crate::agent::{AgentKind, ResumeTarget, SavedSession};

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
        resume: ResumeTarget,
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
            let kind = resume.kind();
            window.push_notification(Notification::info("Resuming chat…"), cx);
            self.start_chat(
                target,
                kind,
                ChatStart::Resume(resume),
                None,
                true,
                window,
                cx,
            );
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
            let agent = session.kind().name();
            let title = session.title.to_lowercase();
            words
                .iter()
                .all(|word| title.contains(word) || agent.contains(word))
                .then_some(index)
        })
        .collect()
}

fn list_sessions_for_kind(kind: AgentKind, root: &Path) -> anyhow::Result<Vec<SavedSession>> {
    match kind {
        AgentKind::Pi => crate::pi::list_sessions(root),
        AgentKind::Agy => crate::agy::list_sessions(root),
        AgentKind::Codex => crate::codex::list_sessions(root),
    }
}

fn is_missing_binary(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io_err| io_err.kind() == std::io::ErrorKind::NotFound)
    })
}

struct ResumePicker {
    owner: WeakEntity<Tab>,
    target: EntityId,
    request: u64,
    input: Entity<InputState>,
    sessions: Vec<SavedSession>,
    matches: Vec<usize>,
    selected: usize,
    pending_scans: usize,
    error: Option<String>,
    _scans: Vec<Task<()>>,
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
        let scans = AgentKind::ALL
            .into_iter()
            .map(|kind| {
                let root = root.clone();
                let scan = cx.background_spawn(async move { list_sessions_for_kind(kind, &root) });
                cx.spawn(async move |this, cx| {
                    let result = scan.await;
                    let _ = this.update(cx, |this, cx| {
                        this.pending_scans = this.pending_scans.saturating_sub(1);
                        match result {
                            Ok(mut sessions) => {
                                let previous = this
                                    .matches
                                    .get(this.selected)
                                    .map(|&idx| this.sessions[idx].target.clone());
                                this.sessions.append(&mut sessions);
                                this.sessions.sort_by(|a, b| {
                                    b.updated_at
                                        .cmp(&a.updated_at)
                                        .then_with(|| a.detail.cmp(&b.detail))
                                });
                                this.matches =
                                    filter_sessions(&this.sessions, &this.input.read(cx).value());
                                this.selected = previous
                                    .and_then(|target| {
                                        this.matches
                                            .iter()
                                            .position(|&idx| this.sessions[idx].target == target)
                                    })
                                    .unwrap_or(0);
                                if !this.sessions.is_empty() {
                                    this.error = None;
                                }
                            }
                            Err(error) if is_missing_binary(&error) => {}
                            Err(error) => {
                                eprintln!(
                                    "rockstar: could not list {} sessions: {error:#}",
                                    kind.name()
                                );
                                if this.sessions.is_empty() && this.error.is_none() {
                                    this.error = Some(format!("{error:#}"));
                                }
                            }
                        }
                        cx.notify();
                    });
                })
            })
            .collect();
        Self {
            owner,
            target,
            request,
            input,
            sessions: Vec::new(),
            matches: Vec::new(),
            selected: 0,
            pending_scans: AgentKind::ALL.len(),
            error: None,
            _scans: scans,
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
        let resume = self.sessions[index].target.clone();
        cx.set_global(Mode::Normal);
        let _ = self.owner.update(cx, |owner, cx| {
            owner.resume_from_picker(self.target, self.request, resume, window, cx);
        });
    }
}

impl Render for ResumePicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let loading = self.pending_scans > 0 && self.sessions.is_empty();
        let status = if loading {
            "Loading sessions…".into()
        } else if self.sessions.is_empty()
            && let Some(error) = &self.error
        {
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
                !loading && self.error.is_none() && self.matches.is_empty(),
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
                        let agent = session.kind().name();
                        let updated = session
                            .updated_at
                            .with_timezone(&chrono::Local)
                            .format("%Y-%m-%d %H:%M")
                            .to_string();
                        div()
                            .id(("resume-candidate", index))
                            .role(Role::ListBoxOption)
                            .aria_label(format!("{agent} · {} · {updated}", session.title))
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
                            .child(div().flex_shrink_0().text_xs().child(agent))
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
                // The session identifier distinguishes chats with the same title/date.
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .truncate()
                        .child(self.sessions[*index].detail.clone()),
                )
            })
    }
}
