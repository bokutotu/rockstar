//! Drive the production window through public UI events. Only the agent is mocked.
use std::sync::MutexGuard;

use async_channel::{Receiver, Sender};
use gpui_kit::{
    AnyWindowHandle, App, HeadlessAppContext, Window,
    test::{TestQueryExt as _, TestWindowExt as _},
};

use crate::{
    agent::{AgentEvent, AgentRequest, Message},
    app::open_chat_window,
    test_support::{RENDER_LOCK, headless_app, mock_agent, reply_with_messages},
};

struct Driver {
    cx: HeadlessAppContext,
    window: AnyWindowHandle,
    requests: Receiver<AgentRequest>,
    events: Sender<AgentEvent>,
    // Drop the App (and its renderer) before releasing the lock.
    _render_guard: MutexGuard<'static, ()>,
}

impl Driver {
    fn open() -> Self {
        let render_guard = RENDER_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut cx = headless_app();
        let (agent, requests, events) = mock_agent();
        let (window, _) = cx.update(|cx| open_chat_window(agent, cx).unwrap());
        cx.update_window(window, |_, window, _| window.activate_window())
            .unwrap();
        Self {
            cx,
            window,
            requests,
            events,
            _render_guard: render_guard,
        }
    }

    fn ui(&mut self, f: impl FnOnce(&mut Window, &mut App)) {
        self.cx.run_until_parked();
        self.cx
            .update_window(self.window, |_, window, cx| {
                window.render_frame(cx);
                f(window, cx);
            })
            .unwrap();
        self.cx.run_until_parked();
    }

    fn press(&mut self, key: &str) {
        self.ui(|window, cx| window.press(key, cx));
    }

    fn type_text(&mut self, text: &str) {
        self.ui(|window, cx| window.input(text, cx));
    }

    fn click_input(&mut self) {
        self.ui(|window, cx| {
            let field = window.find_by_label("Message to pi");
            window
                .within("chat")
                .click(field.path().last().unwrap().clone(), cx);
        });
    }

    fn assert_draft(&mut self, expected: &str) {
        self.ui(|window, _| {
            assert_eq!(
                window.find_by_label("Message to pi").value(),
                Some(expected)
            );
        });
    }

    fn assert_popup(&mut self, open: bool) {
        self.ui(|window, _| {
            assert_eq!(
                window
                    .try_find("command-popup")
                    .is_some_and(|popup| popup.visible()),
                open
            );
        });
    }

    fn assert_conversation(&mut self, present: bool) {
        self.ui(|window, _| {
            for index in 0..2_usize {
                assert_eq!(window.try_find(("message", index)).is_some(), present);
            }
        });
    }
}

#[test]
fn i_and_escape_switch_input_mode() {
    let mut app = Driver::open();
    app.type_text("abc");
    app.assert_draft("");
    app.press("i");
    app.type_text("Hello");
    app.assert_draft("Hello");
    app.press("escape");
    app.type_text("abc");
    app.assert_draft("Hello");
}

#[test]
fn input_click_enables_typing() {
    let mut app = Driver::open();
    app.click_input();
    app.type_text("Hello");
    app.assert_draft("Hello");
}

#[test]
fn enter_inserts_newline_without_sending() {
    let mut app = Driver::open();
    app.press("i");
    app.type_text("First line");
    app.press("enter");
    app.type_text("Second line");
    app.assert_draft("First line\nSecond line");
    assert!(app.requests.is_empty());
}

#[test]
fn ctrl_enter_sends_once_and_clears_draft() {
    let mut app = Driver::open();
    app.press("i");
    app.type_text("Hello");
    app.press("ctrl-enter");
    assert!(matches!(
        app.requests.try_recv(),
        Ok(AgentRequest::Prompt(text)) if text == "Hello"
    ));
    assert!(app.requests.is_empty());
    app.assert_draft("");
}

#[test]
fn colon_opens_palette_and_escape_closes_it() {
    let mut app = Driver::open();
    app.press(":");
    app.assert_popup(true);
    app.press("escape");
    app.assert_popup(false);
}

#[test]
fn outside_click_closes_palette_without_sending_or_clearing_draft() {
    let mut app = Driver::open();
    app.press("i");
    app.type_text("An unsent draft");
    app.press("escape");
    app.press(":");
    app.assert_popup(true);
    // The pointer event hits the overlay, not the message input behind it.
    app.click_input();
    app.assert_popup(false);
    app.assert_draft("An unsent draft");
    assert!(app.requests.is_empty());
}

#[test]
fn new_session_clears_chat_and_draft_only_after_success() {
    let mut app = Driver::open();
    reply_with_messages(
        &app.events,
        vec![
            Message::User("Hello".into()),
            Message::Assistant("Hello from the mock agent.".into()),
        ],
    );
    app.press("i");
    app.type_text("An unsent draft");
    app.press("escape");
    app.press(":");
    app.type_text("new");
    app.press("enter");
    assert!(matches!(
        app.requests.try_recv(),
        Ok(AgentRequest::NewSession)
    ));
    app.assert_conversation(true);
    app.assert_draft("An unsent draft");

    app.events
        .try_send(AgentEvent::NewSessionFinished(Ok(())))
        .unwrap();
    app.assert_conversation(false);
    app.assert_draft("");
}
