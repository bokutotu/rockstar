use gpui_kit::{MouseButton, test::TestEventExt as _};

use super::*;
use crate::{
    agent::{AgentEvent, Message},
    test_support::reply_with_messages,
};

impl Driver {
    fn command(&mut self, command: &str) {
        self.press(":");
        self.type_text(command);
        self.press("enter");
    }

    fn split_pane(&mut self, direction: &str) {
        self.press("s");
        self.press(direction);
    }

    fn click_pane(&mut self, index: usize) {
        self.ui(|window, cx| {
            let position = window.find_all("chat")[index].bounds().center();
            window.pointer_move(position, None, cx);
            window.pointer_down(position, MouseButton::Left, cx);
            window.pointer_up(position, MouseButton::Left, cx);
        });
    }

    fn assert_chats(&mut self, numbers: &[usize]) {
        self.ui(|window, _| {
            let labels: Vec<_> = window
                .find_all("chat")
                .iter()
                .map(|pane| pane.label().unwrap().to_string())
                .collect();
            let expected: Vec<_> = numbers
                .iter()
                .map(|number| format!("Chat {number}"))
                .collect();
            assert_eq!(labels, expected);
        });
    }

    fn assert_message_count(&mut self, count: usize) {
        self.ui(|window, _| {
            for index in 0..count {
                assert!(window.find(("message", index)).visible());
            }
            assert!(window.try_find(("message", count)).is_none());
        });
    }
}

#[test]
fn close_removes_active_pane_and_preserves_its_session_in_both_split_directions() {
    for direction in ["v", "s"] {
        let mut app = Driver::open();
        app.press("i");
        app.type_text("Original draft");
        app.press("escape");
        app.split_pane(direction);
        app.command("new");
        app.wait_for_chat(2);
        let second = app.new_backends.try_recv().unwrap();
        reply_with_messages(
            &second.events,
            vec![
                Message::User("Second chat question".into()),
                Message::Assistant("Second chat answer".into()),
            ],
        );
        app.press("i");
        app.type_text("Second draft");
        app.press("escape");
        app.assert_chats(&[1, 2]);

        app.command("  ClOsE  ");
        app.assert_popup(false);
        app.assert_chats(&[1]);
        app.assert_draft("Original draft");
        app.assert_active_pane(0);
        app.assert_message_count(0);
        app.press("i");
        app.ui(|window, _| {
            assert_eq!(window.find_by_label("Message to pi").focused(), Some(true));
        });
        app.type_text(" continues");
        app.press("escape");

        // The closed pane's chat is still available, including its live connection.
        app.press("ctrl-n");
        app.assert_chats(&[2]);
        app.assert_draft("Second draft");
        app.assert_message_count(2);
        second
            .events
            .try_send(AgentEvent::UserMessage("Still connected".into()))
            .unwrap();
        app.assert_message_count(3);
        app.press("ctrl-p");
        app.assert_chats(&[1]);
        app.assert_draft("Original draft continues");
        app.assert_no_requests();
        assert!(second.prompts.is_empty());
    }
}

#[test]
fn close_last_pane_reports_error_and_keeps_the_palette_and_chat() {
    let mut app = Driver::open();
    app.cx.update(|cx| cx.set_reduce_motion(true));
    reply_with_messages(
        &app.backend.events,
        vec![Message::User("Keep this history".into())],
    );
    app.press("i");
    app.type_text("Keep this draft");
    app.press("escape");
    let mut original_path = Vec::new();
    app.ui(|window, _| original_path = window.find("chat").path().to_vec());

    app.command("close");
    app.assert_popup(true);
    app.assert_chats(&[1]);
    app.assert_draft("Keep this draft");
    app.assert_message_count(1);
    app.ui(|window, _| {
        assert_eq!(window.find("chat").path(), original_path);
        assert!(window.find("notification").visible());
        let command = window.find_by_label("Command");
        assert_eq!(command.value(), Some("close"));
        assert_eq!(command.focused(), Some(true));
    });
    app.assert_no_requests();
    assert!(app.new_backends.is_empty());

    app.press("escape");
    app.assert_popup(false);
    app.press("i");
    app.type_text(" continues");
    app.assert_draft("Keep this draft continues");
}

#[test]
fn close_shared_session_keeps_draft_history_and_incoming_events() {
    let mut app = Driver::open();
    for event in [
        AgentEvent::UserMessage("A question".into()),
        AgentEvent::AssistantStarted,
        AgentEvent::AssistantTextDelta("Partial answer".into()),
    ] {
        app.backend.events.try_send(event).unwrap();
    }
    app.press("i");
    app.type_text("Shared draft");
    app.press("escape");
    app.split_pane("v");
    app.ui(|window, _| {
        assert_eq!(window.find_all(("message", 0_usize)).len(), 2);
        assert_eq!(window.find_all(("message", 1_usize)).len(), 2);
    });

    app.command("close");
    app.assert_chats(&[1]);
    app.assert_draft("Shared draft");
    app.assert_message_count(2);
    for event in [
        AgentEvent::AssistantTextDelta(" continues".into()),
        AgentEvent::AssistantFinished("Completed answer".into()),
        AgentEvent::PromptFinished,
        AgentEvent::UserMessage("Another question".into()),
    ] {
        app.backend.events.try_send(event).unwrap();
    }
    app.assert_message_count(3);
    app.assert_draft("Shared draft");
    app.assert_no_requests();
}

#[test]
fn close_first_or_middle_pane_keeps_clicks_navigation_and_later_splits_working() {
    for closed in [0, 1] {
        let mut app = Driver::open();
        for number in 2..=3 {
            app.split_pane("v");
            app.command("new");
            app.wait_for_chat(number);
        }
        app.assert_chats(&[1, 2, 3]);
        app.click_pane(closed);
        app.assert_active_pane(closed);
        app.command("close");
        let remaining: Vec<_> = (1..=3).filter(|number| *number != closed + 1).collect();
        app.assert_chats(&remaining);
        app.assert_active_pane(closed);

        app.click_pane(0);
        app.assert_active_pane(0);
        app.press("s");
        app.press("l");
        app.assert_active_pane(1);
        app.press("s");
        app.press("h");
        app.assert_active_pane(0);
        app.click_pane(1);
        app.assert_active_pane(1);

        app.split_pane("s");
        app.assert_chats(&[remaining[0], remaining[1], remaining[1]]);
        app.assert_active_pane(2);
        app.command("close");
        app.assert_chats(&remaining);
        app.assert_active_pane(1);
        app.command("close");
        app.assert_chats(&remaining[..1]);
        app.assert_active_pane(0);
    }
}

#[test]
fn close_before_new_finishes_discards_the_connection_even_if_the_pane_index_is_reused() {
    let mut app = Driver::open();
    app.press("i");
    app.type_text("Shared draft");
    app.press("escape");
    app.split_pane("v");

    // Do not drive the executor between these commands. The connection result
    // arrives only after its destination is gone and another pane has its index.
    for command in ["new", "close"] {
        app.cx
            .update_window(app.window, |_, window, cx| window.press(":", cx))
            .unwrap();
        app.cx
            .update_window(app.window, |_, window, cx| window.input(command, cx))
            .unwrap();
        app.cx
            .update_window(app.window, |_, window, cx| window.press("enter", cx))
            .unwrap();
    }
    for key in ["s", "v"] {
        app.cx
            .update_window(app.window, |_, window, cx| window.press(key, cx))
            .unwrap();
    }
    assert!(app.new_backends.is_empty());

    app.assert_chats(&[1, 1]);
    app.assert_popup(false);
    let discarded = app.new_backends.try_recv().unwrap();
    assert!(discarded.prompts.is_closed());
    assert!(discarded.events.is_closed());
    app.press("ctrl-n");
    app.assert_chats(&[1, 1]);
    app.ui(|window, _| {
        assert!(window.try_find("notification").is_none());
        for input in window.find_all_by_label("Message to pi") {
            assert_eq!(input.value(), Some("Shared draft"));
        }
    });
    app.assert_active_pane(1);
    app.assert_no_requests();
}
