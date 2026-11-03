//! Drive the production window through public UI events. Only the agent is mocked.
use std::sync::MutexGuard;

use async_channel::{Receiver, Sender};
use gpui_kit::{
    AnyWindowHandle, App, HeadlessAppContext, Window, px, size,
    test::{TestQueryExt as _, TestWindowExt as _},
};

use crate::{
    agent::{AgentError, AgentEvent, AgentRequest, Message},
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

    fn ui<R>(&mut self, f: impl FnOnce(&mut Window, &mut App) -> R) -> R {
        self.cx.run_until_parked();
        let result = self
            .cx
            .update_window(self.window, |_, window, cx| {
                window.render_frame(cx);
                f(window, cx)
            })
            .unwrap();
        self.cx.run_until_parked();
        result
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
            assert_eq!(window.try_find("command-popup").is_some(), open);
            if open {
                assert!(window.find("command-popup").visible());
                assert_eq!(window.find_by_label("Command search").focused(), Some(true));
            }
        });
    }

    fn assert_conversation(&mut self, present: bool) {
        self.ui(|window, _| {
            for index in 0..2_usize {
                assert_eq!(window.try_find(("message", index)).is_some(), present);
            }
        });
    }

    fn no_requests(&mut self) {
        self.cx.run_until_parked();
        assert!(self.requests.is_empty(), "unexpected agent request");
    }

    fn new_session_request(&mut self) {
        self.cx.run_until_parked();
        match self.requests.try_recv().expect("new-session request") {
            AgentRequest::NewSession => {}
            AgentRequest::Prompt(text) => panic!("command incorrectly sent as a prompt: {text}"),
        }
    }

    fn finish_new_session(&mut self, result: Result<(), AgentError>) {
        self.events
            .try_send(AgentEvent::NewSessionFinished(result))
            .unwrap();
    }

    fn start_prompt(&mut self) {
        self.press("i");
        self.type_text("Hello");
        self.press("ctrl-enter");
        match self.requests.try_recv().expect("prompt request") {
            AgentRequest::Prompt(text) => assert_eq!(text, "Hello"),
            AgentRequest::NewSession => panic!("message incorrectly sent as a command"),
        }
        self.assert_draft("");
        self.no_requests();
    }

    fn finish_prompt(&mut self, messages: Vec<Message>) {
        reply_with_messages(&self.events, messages);
    }

    fn prepare_conversation_and_draft(&mut self) {
        self.start_prompt();
        self.finish_prompt(vec![
            Message::User("Hello".into()),
            Message::Assistant("Hello from the mock agent.".into()),
        ]);
        self.assert_conversation(true);
        self.click_input();
        self.type_text("An unsent draft");
        self.press("escape");
        self.assert_draft("An unsent draft");
    }
}

#[test]
fn normal_insert_and_popup_are_driven_by_keyboard_and_mouse() {
    let mut app = Driver::open();
    app.ui(|window, _| {
        let input = window.find_by_label("Message to pi");
        assert_eq!(input.focused(), Some(false));
        assert_eq!(input.placeholder(), Some("NORMAL — i to type · : commands"));
        assert!(window.try_find("send").is_none());
    });
    app.type_text("abc");
    app.assert_draft("");
    app.press("i");
    for blank in ["", " \t\n"] {
        app.type_text(blank);
        app.press("ctrl-enter");
        app.assert_draft(blank);
        app.no_requests();
    }
    app.press("ctrl-a");
    app.press("backspace");
    app.type_text("draft:new");
    app.assert_draft("draft:new");
    app.assert_popup(false);
    app.press("escape");
    app.type_text("abc");
    app.assert_draft("draft:new");
    app.press(":");
    app.assert_popup(true);
    app.ui(|window, _| {
        assert_eq!(window.find_by_label("Command search").value(), Some(""));
        assert!(window.find("command-new").visible());
    });
    app.type_text("nw");
    app.press("escape");
    app.assert_popup(false);
    app.assert_draft("draft:new");
    app.press(":");
    app.ui(|window, _| assert_eq!(window.find_by_label("Command search").value(), Some("")));
    app.press("escape");
    app.click_input();
    app.ui(|window, _| {
        let input = window.find_by_label("Message to pi");
        assert_eq!(input.focused(), Some(true), "{input:?}");
        assert_eq!(input.placeholder(), Some("Ask pi…"), "{input:?}");
    });
    app.press("ctrl-end");
    app.type_text("!");
    app.assert_draft("draft:new!");
    app.press("enter");
    app.type_text("Second line");
    app.assert_draft("draft:new!\nSecond line");
    app.no_requests();
    app.press("ctrl-enter");
    match app.requests.try_recv().expect("prompt request") {
        AgentRequest::Prompt(text) => assert_eq!(text, "draft:new!\nSecond line"),
        AgentRequest::NewSession => panic!("message incorrectly sent as a command"),
    }
    app.assert_draft("");
    app.ui(|window, _| assert_eq!(window.find_by_label("Message to pi").focused(), Some(true)));
    app.no_requests();

    app.type_text("Keep this draft");
    app.requests.close();
    app.press("ctrl-enter");
    app.assert_draft("Keep this draft");
    app.no_requests();
}

#[test]
fn input_click_enters_insert_and_popup_dismissal_does_not_send_draft() {
    let mut app = Driver::open();
    app.click_input();
    app.type_text("An unsent draft");
    app.assert_draft("An unsent draft");
    app.press("escape");
    app.press("ctrl-enter");
    app.assert_draft("An unsent draft");
    app.no_requests();
    app.press(":");
    app.press("ctrl-enter");
    app.assert_popup(true);
    app.assert_draft("An unsent draft");
    app.no_requests();
    // The pointer event hits the overlay, not the message input behind it.
    app.click_input();
    app.assert_popup(false);
    app.assert_draft("An unsent draft");
    app.no_requests();
    app.type_text("abc");
    app.assert_draft("An unsent draft");
    app.press(":");
    app.assert_popup(true);
}

#[test]
fn command_popup_fits_the_minimum_window_size() {
    let mut app = Driver::open();
    app.ui(|window, cx| {
        window.resize(size(px(360.0), px(240.0)));
        window.bounds_changed(cx);
    });
    app.press(":");
    app.type_text("nw");
    app.assert_popup(true);
    app.ui(|window, _| {
        let popup = window.find("command-popup").bounds();
        assert!(popup.origin.x >= px(0.0) && popup.origin.y >= px(0.0));
        assert!(popup.right() <= px(360.0) && popup.bottom() <= px(240.0));
        let input = window.find_by_label("Command search").bounds();
        let command = window.find("command-new").bounds();
        assert!(popup.contains(&input.origin) && popup.contains(&input.bottom_right()));
        assert!(popup.contains(&command.origin) && popup.contains(&command.bottom_right()));
    });
    app.press("enter");
    app.new_session_request();
    app.finish_new_session(Ok(()));
    app.assert_popup(false);
}

#[test]
fn fuzzy_queries_filter_commands_and_navigation_stays_in_popup() {
    let mut app = Driver::open();
    app.press(":");
    for (query, matches) in [
        ("", true),
        ("n", true),
        ("ne", true),
        ("NW", true),
        ("NeW", true),
        ("ew", true),
        ("we", false),
        ("nn", false),
        ("model", false),
        ("新", false),
    ] {
        app.press("ctrl-a");
        app.press("backspace");
        app.type_text(query);
        app.ui(|window, _| {
            assert_eq!(
                window.try_find("command-new").is_some(),
                matches,
                "query: {query}"
            );
            assert_eq!(
                window.try_find("no-commands").is_some(),
                !matches,
                "query: {query}"
            );
        });
        if !matches {
            app.press("enter");
            app.assert_popup(true);
            app.no_requests();
        }
    }
    app.press("ctrl-a");
    app.type_text("nw");
    for key in [
        "up",
        "down",
        "ctrl-p",
        "ctrl-n",
        "tab",
        "shift-tab",
        "ctrl-enter",
    ] {
        app.press(key);
        app.assert_popup(true);
        app.ui(|window, _| assert_eq!(window.find_by_label("Command search").value(), Some("nw")));
    }
    app.assert_draft("");
    app.press("escape");
    app.assert_popup(false);
    app.no_requests();
}

#[test]
fn new_session_clears_chat_only_after_success_and_can_send_again() {
    let mut app = Driver::open();
    app.prepare_conversation_and_draft();
    app.press(":");
    app.type_text("NW");
    app.press("enter");
    app.new_session_request();
    app.assert_popup(false);
    app.assert_conversation(true);
    app.assert_draft("An unsent draft");
    app.press("enter");
    app.no_requests();
    app.finish_new_session(Ok(()));
    app.assert_conversation(false);
    app.assert_draft("");
    app.ui(|window, _| assert_eq!(window.find_by_label("Message to pi").focused(), Some(false)));
    app.no_requests();
    app.start_prompt();
    app.finish_prompt(vec![
        Message::User("Hello".into()),
        Message::Assistant("A fresh conversation.".into()),
    ]);
    app.assert_conversation(true);
}

fn preserved_on_unsuccessful_reset(result: Result<(), AgentError>) {
    let mut app = Driver::open();
    app.prepare_conversation_and_draft();
    app.press(":");
    // With an empty query, Enter runs the displayed default result.
    app.press("enter");
    app.new_session_request();
    app.finish_new_session(result);
    app.assert_popup(false);
    app.assert_conversation(true);
    app.assert_draft("An unsent draft");
    // Recovery uses the same UI and the same agent, not a replacement view.
    app.press(":");
    app.ui(|window, cx| window.click("command-new", cx));
    app.new_session_request();
    app.finish_new_session(Ok(()));
    app.assert_conversation(false);
    app.assert_draft("");
    app.no_requests();
}

#[test]
fn cancelled_new_session_preserves_chat_and_draft() {
    preserved_on_unsuccessful_reset(Err(anyhow::anyhow!("New session cancelled.")));
}

#[test]
fn failed_new_session_preserves_chat_and_draft() {
    preserved_on_unsuccessful_reset(Err(anyhow::anyhow!("Mock Pi connection failure")));
}

#[test]
fn new_is_accepted_during_prompts_and_session_requests() {
    let mut app = Driver::open();
    app.start_prompt();
    app.press("escape");
    app.press(":");
    app.type_text("new");
    app.ui(|window, _| assert_ne!(window.find("command-new").disabled(), Some(true)));
    app.press("enter");
    app.new_session_request();
    app.assert_popup(false);
    app.finish_prompt(vec![
        Message::User("Hello".into()),
        Message::Assistant("Done.".into()),
    ]);
    app.assert_conversation(true);
    app.press(":");
    app.ui(|window, _| assert_ne!(window.find("command-new").disabled(), Some(true)));
    app.ui(|window, cx| window.click("command-new", cx));
    app.new_session_request();
    app.assert_popup(false);
    app.finish_new_session(Ok(()));
    app.finish_new_session(Ok(()));
    app.assert_conversation(false);
    app.assert_draft("");
    app.no_requests();
}
