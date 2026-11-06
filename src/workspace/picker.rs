//! Selection is committed only on Enter/click, never while browsing candidates.
use super::*;
use gpui_kit::{
    WeakEntity,
    component::input::{Enter, Input, InputEvent, InputState, MoveDown, MoveUp},
};

#[derive(Clone, Copy)]
pub(super) enum PickerKind {
    Workspaces,
    Tabs,
}

pub(super) struct Candidate {
    pub id: EntityId,
    pub label: String,
    pub current: bool,
}

pub(super) fn open(
    owner: WeakEntity<WorkspaceWindow>,
    kind: PickerKind,
    candidates: Vec<Candidate>,
    window: &mut Window,
    cx: &mut App,
) {
    let title = match kind {
        PickerKind::Workspaces => "Workspaces",
        PickerKind::Tabs => "Tabs",
    };
    let picker = cx.new(|cx| {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(format!("Find {title}…")));
        cx.subscribe(&input, |this: &mut Switcher, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.filter(cx);
            }
        })
        .detach();
        let selected = candidates
            .iter()
            .position(|candidate| candidate.current)
            .unwrap_or(0);
        Switcher {
            owner,
            kind,
            input,
            matches: (0..candidates.len()).collect(),
            candidates,
            selected,
        }
    });
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
            .title(title)
            .close_button(false)
            .entrance(DialogEntrance::None)
            .on_ok(|_, _, _| false)
            .on_close(|_, _, cx| cx.set_global(Mode::Normal))
            .child(picker.clone())
    });
    focus.focus(window, cx);
}

struct Switcher {
    owner: WeakEntity<WorkspaceWindow>,
    kind: PickerKind,
    input: Entity<InputState>,
    candidates: Vec<Candidate>,
    matches: Vec<usize>,
    selected: usize,
}
impl Switcher {
    fn filter(&mut self, cx: &mut Context<Self>) {
        let query = self.input.read(cx).value();
        if query.is_empty() {
            self.matches = (0..self.candidates.len()).collect();
        } else {
            let labels = self
                .candidates
                .iter()
                .map(|candidate| candidate.label.clone())
                .collect::<Vec<_>>();
            self.matches = crate::file_search::fuzzy_match(&labels, &query, labels.len())
                .iter()
                .filter_map(|label| labels.iter().position(|candidate| candidate == label))
                .collect();
        }
        self.selected = 0;
        cx.notify();
    }
    fn move_selection(&mut self, forward: bool, cx: &mut Context<Self>) {
        if !self.matches.is_empty() {
            self.selected = cycle_index(self.selected, self.matches.len(), forward);
        }
        cx.notify();
    }
    fn accept(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(candidate) = self
            .matches
            .get(index)
            .map(|index| &self.candidates[*index])
        {
            let id = candidate.id;
            window.close_dialog(cx);
            let _ = self
                .owner
                .update(cx, |owner, cx| owner.select(self.kind, id, window, cx));
        }
    }
}
impl Render for Switcher {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("workspace-switcher")
            .role(Role::Group)
            .aria_label("Switch workspace or tab")
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
            .capture_action(cx.listener(|this, _: &Enter, window, cx| {
                this.accept(this.selected, window, cx);
                cx.stop_propagation();
            }))
            .child(Input::new(&self.input).aria_label("Switch query"))
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child("Enter to switch · Esc to cancel · ● current"),
            )
            .when(self.matches.is_empty(), |this| this.child("No matches"))
            .children(
                self.matches
                    .iter()
                    .enumerate()
                    .skip(self.selected.saturating_sub(9))
                    .take(10)
                    .map(|(index, candidate)| {
                        let candidate = &self.candidates[*candidate];
                        div()
                            .id(("switch-candidate", index))
                            .role(Role::ListBoxOption)
                            .aria_label(candidate.label.clone())
                            .aria_selected(index == self.selected)
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .cursor_pointer()
                            .when(index == self.selected, |this| {
                                this.bg(cx.theme().accent)
                                    .text_color(cx.theme().accent_foreground)
                            })
                            .child(format!(
                                "{}{}",
                                if candidate.current { "● " } else { "  " },
                                candidate.label
                            ))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.accept(index, window, cx)
                            }))
                    }),
            )
    }
}
