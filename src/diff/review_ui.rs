use super::{
    DiffSession,
    review::{self, Target},
};
use crate::mode::Mode;
use gpui_kit::{
    App, AppContext as _, Entity, Focusable as _, InteractiveElement as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, Window,
    component::{
        ActiveTheme as _, WindowExt as _,
        button::Button,
        dialog::DialogEntrance,
        input::{Textarea, TextareaState},
        notification::Notification,
    },
    div, px,
};

pub(super) fn edit(
    session: Entity<DiffSession>,
    target: Target,
    id: Option<u64>,
    window: &mut Window,
    cx: &mut App,
) {
    if session.read(cx).submitting_review {
        return;
    }
    let value = id
        .and_then(|id| {
            session
                .read(cx)
                .comments
                .iter()
                .find(|comment| comment.id == id)
        })
        .map(|comment| comment.body.clone())
        .unwrap_or_default();
    let input = cx.new(|cx| {
        let mut input = TextareaState::new(window, cx)
            .rows(5)
            .placeholder("Review comment…");
        input.set_value(value, window, cx);
        input
    });
    let focus = input.focus_handle(cx);
    cx.set_global(Mode::Confirm);
    window.open_dialog(cx, move |dialog, _, _| {
        let input = input.clone();
        let save_input = input.clone();
        let session = session.clone();
        let target = target.clone();
        dialog
            .centered()
            .w(px(600.))
            .title(target.label())
            .entrance(DialogEntrance::None)
            .on_close(|_, _, cx| cx.set_global(Mode::Normal))
            .on_ok(|_, _, _| false)
            .child(Textarea::new(&input).aria_label("Review comment"))
            .child(
                Button::new("save-review-comment")
                    .label("Save comment")
                    .on_click(move |_, window, cx| {
                        let value = save_input.read(cx).value();
                        if save_input.read(cx).is_composing() {
                            return;
                        }
                        let result = session.update(cx, |session, cx| {
                            let result =
                                session.save_comment(id, target.clone(), value.trim().to_owned());
                            cx.notify();
                            result
                        });
                        match result {
                            Ok(()) => {
                                window.close_dialog(cx);
                                cx.set_global(Mode::Normal);
                            }
                            Err(error) => {
                                window.push_notification(Notification::error(error.to_string()), cx)
                            }
                        }
                    }),
            )
    });
    focus.focus(window, cx);
}

pub(super) fn confirm_submit(session: Entity<DiffSession>, window: &mut Window, cx: &mut App) {
    if session.read(cx).submitting_review || session.read(cx).comments.is_empty() {
        return;
    }
    let preview = review::markdown(&session.read(cx).comments);
    cx.set_global(Mode::Confirm);
    window.open_dialog(cx, move |dialog, _, cx| {
        let session = session.clone();
        dialog.centered().w(px(640.)).title("Submit GitHub review?")
            .entrance(DialogEntrance::None)
            .on_close(|_, _, cx| cx.set_global(Mode::Normal))
            .on_ok(|_, _, _| false)
            .child(div().text_sm().child("Posts a COMMENT review using gh. Matching PR lines become inline comments; local or outdated locations are included in the review body."))
            .child(div().id("review-preview").max_h(px(320.)).overflow_y_scroll()
                .p_2().bg(cx.theme().muted).child(preview.clone()))
            .child(Button::new("submit-github-review").label("Submit review")
                .on_click(move |_, window, cx| {
                    session.update(cx, |session, cx| session.submit_review(window, cx));
                    window.close_dialog(cx);
                    cx.set_global(Mode::Normal);
                }))
    });
}
