use super::{
    FileTree,
    creation::{CreateEntryRequest, EntryKind, create_entry},
};
use crate::mode::Mode;
use gpui_kit::{
    App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Styled as _, Task, WeakEntity, Window,
    base::Disableable as _,
    component::{
        ActiveTheme as _, WindowExt as _,
        button::{Button, ButtonVariants as _},
        dialog::DialogEntrance,
        input::{Enter, Input, InputEvent, InputState},
    },
    div,
    prelude::FluentBuilder as _,
    px,
};
use std::path::PathBuf;

pub(super) fn open(
    tree: WeakEntity<FileTree>,
    root: PathBuf,
    kind: EntryKind,
    initial_path: PathBuf,
    window: &mut Window,
    cx: &mut App,
) {
    let form = cx.new(|cx| CreationDialog::new(tree, root, kind, initial_path, window, cx));
    let focus = form.read(cx).input.focus_handle(cx);
    cx.set_global(Mode::Confirm);
    window.open_dialog(cx, move |dialog, _, cx| {
        let submitting = form.read(cx).task.is_some();
        let submit = form.clone();
        let cancel = form.clone();
        dialog
            .centered()
            .w(px(520.))
            .title(match kind {
                EntryKind::File => "New File",
                EntryKind::Directory => "New Folder",
            })
            .entrance(DialogEntrance::None)
            .close_button(!submitting)
            .overlay_closable(!submitting)
            .on_ok(move |_, window, cx| {
                submit.update(cx, |form, cx| form.submit(window, cx));
                false
            })
            // Check live state too, in case input arrives before the next paint.
            .on_cancel(move |_, _, cx| cancel.read(cx).task.is_none())
            .on_close(|_, _, cx| cx.set_global(Mode::Normal))
            .child(form.clone())
    });
    focus.focus(window, cx);
}

struct CreationDialog {
    tree: WeakEntity<FileTree>,
    root: PathBuf,
    kind: EntryKind,
    input: Entity<InputState>,
    error: Option<String>,
    task: Option<Task<()>>,
}

impl CreationDialog {
    fn new(
        tree: WeakEntity<FileTree>,
        root: PathBuf,
        kind: EntryKind,
        initial_path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            let value = initial_path.to_string_lossy().into_owned();
            let mut input = InputState::new(window, cx).placeholder("Path relative to workspace");
            input.set_value(value.clone(), window, cx);
            input.set_selected_range(value.len()..value.len(), cx);
            input
        });
        cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) && this.error.take().is_some() {
                cx.notify();
            }
        })
        .detach();
        Self {
            tree,
            root,
            kind,
            input,
            error: None,
            task: None,
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.task.is_some() || self.input.read(cx).is_composing() {
            return;
        }
        let request = CreateEntryRequest {
            kind: self.kind,
            relative_path: PathBuf::from(self.input.read(cx).value().as_ref()),
        };
        let root = self.root.clone();
        self.error = None;
        let creating = cx.background_spawn(async move { create_entry(&root, &request) });
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = creating.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.task = None;
                match result {
                    Ok(path) => {
                        // Closing restores the tree focus saved before opening the dialog.
                        window.close_dialog(cx);
                        cx.set_global(Mode::Normal);
                        let _ = this.tree.update(cx, |tree, cx| {
                            tree.finish_creation(this.kind, path, cx);
                        });
                    }
                    Err(error) => {
                        this.error = Some(format!("{error:#}"));
                        this.input.focus_handle(cx).focus(window, cx);
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn cancel(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.task.is_none() {
            window.close_dialog(cx);
            cx.set_global(Mode::Normal);
        }
    }
}

impl Render for CreationDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let submitting = self.task.is_some();
        div()
            .flex()
            .flex_col()
            .gap_3()
            .capture_action(cx.listener(|this, _: &Enter, window, cx| {
                this.submit(window, cx);
                cx.stop_propagation();
            }))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("Workspace: {}", self.root.display())),
            )
            .child(
                Input::new(&self.input)
                    .aria_label("New entry relative path")
                    .disabled(submitting),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(div().text_sm().text_color(cx.theme().danger).child(error))
            })
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(if submitting {
                        "Creating…"
                    } else {
                        "Parent folder must exist · Enter to create · Esc to cancel"
                    }),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("cancel-create-entry")
                            .label("Cancel")
                            .disabled(submitting)
                            .on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx))),
                    )
                    .child(
                        Button::new("create-entry")
                            .label("Create")
                            .primary()
                            .disabled(submitting)
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
    }
}
