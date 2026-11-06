//! The retained-item bar and keyboard cycling use the same production UI paths.
use super::*;
use crate::{
    agent::{AgentEvent, Message},
    test_support::reply_with_messages,
};
use gpui_kit::{ScrollDelta, point, px};

impl Driver {
    fn assert_items(&mut self, labels: &[&str], selected: Option<&str>) {
        self.ui(|window, _| {
            let bar = window.find("open-items");
            assert!(bar.visible());
            let items = window.within("open-items").find_all_by_role(Role::Tab);
            assert_eq!(
                items
                    .iter()
                    .map(|item| item.label().unwrap())
                    .collect::<Vec<_>>(),
                labels
            );
            assert_eq!(
                items
                    .iter()
                    .filter(|item| item.selected() == Some(true))
                    .map(|item| item.label().unwrap())
                    .collect::<Vec<_>>(),
                selected.into_iter().collect::<Vec<_>>(),
            );
            for pane in window.find_all("pane") {
                assert!(pane.bounds().top() >= bar.bounds().bottom());
            }
        });
    }

    fn click_item(&mut self, label: &str) {
        self.ui(|window, cx| {
            let item = window.within("open-items").find_by_label(label);
            window
                .within("open-items")
                .click(item.path().last().unwrap().clone(), cx);
        });
    }

    fn assert_item_visible(&mut self, label: &str) {
        self.ui(|window, _| {
            let bar = window.find("open-items").bounds();
            let item = window.within("open-items").find_by_label(label);
            assert!(item.visible(), "{label} must be visible");
            assert!(item.bounds().left() >= bar.left());
            assert!(item.bounds().right() <= bar.right());
        });
    }
}

#[test]
fn mixed_items_cycle_in_opening_order_in_both_directions() {
    let project = Project::new(&[("a.txt", "alpha"), ("b.txt", "beta")]);
    let mut app = Driver::open_in(project.0.clone());
    app.open_file("a.txt", &project.0.join("a.txt"));
    app.command("new");
    app.wait_for_chat(2);
    let second = app.new_backends.try_recv().unwrap();
    app.open_file("b.txt", &project.0.join("b.txt"));
    let labels = ["Chat 1", "a.txt", "Chat 2", "b.txt"];
    app.assert_items(&labels, Some("b.txt"));
    for (key, order) in [
        ("ctrl-n", ["Chat 1", "a.txt", "Chat 2", "b.txt"]),
        ("ctrl-p", ["Chat 2", "a.txt", "Chat 1", "b.txt"]),
    ] {
        for label in order {
            app.press(key);
            app.assert_items(&labels, Some(label));
            match label {
                "a.txt" => app.assert_sources(&["alpha"]),
                "b.txt" => app.assert_sources(&["beta"]),
                chat => app.ui(|window, _| assert_eq!(window.find("chat").label(), Some(chat))),
            }
        }
    }
    for label in labels {
        app.click_item(label);
        app.assert_items(&labels, Some(label));
    }
    app.assert_no_requests();
    assert!(second.prompts.is_empty());
    assert!(app.new_backends.is_empty(), "switching must not reconnect");
}

#[test]
fn cycling_empty_single_and_plain_panes_has_defined_behavior() {
    let project = Project::new(&[("only.txt", "only")]);
    let mut app = Driver::open_plain_in(project.0.clone());
    for key in ["ctrl-n", "ctrl-p"] {
        app.press(key);
        app.assert_items(&[], None);
        app.ui(|window, _| assert!(window.find("plain").visible()));
    }
    app.open_file("only.txt", &project.0.join("only.txt"));
    for key in ["ctrl-n", "ctrl-p"] {
        app.press(key);
        app.assert_items(&["only.txt"], Some("only.txt"));
        app.assert_sources(&["only"]);
    }
    app.keys(&["s", "v"]);
    app.assert_items(&["only.txt"], None);
    app.press("ctrl-p");
    app.assert_sources(&["only", "only"]);
    app.command("chat");
    app.wait_for_chat(1);
    app.keys(&["s", "s"]);
    app.assert_items(&["only.txt", "Chat 1"], None);
    app.press("ctrl-n");
    app.assert_items(&["only.txt", "Chat 1"], Some("only.txt"));
    app.assert_sources(&["only", "only"]);
}

#[test]
fn item_clicks_only_replace_the_active_pane_and_follow_pane_focus() {
    let project = Project::new(&[("main.txt", "source")]);
    let mut app = Driver::open_in(project.0.clone());
    app.keys(&["s", "v"]);
    app.open_file("main.txt", &project.0.join("main.txt"));
    app.command("new");
    app.wait_for_chat(2);
    let labels = ["Chat 1", "main.txt", "Chat 2"];
    app.assert_items(&labels, Some("Chat 2"));
    app.keys(&["s", "h"]);
    app.assert_items(&labels, Some("Chat 1"));
    app.click_item("main.txt");
    app.assert_items(&labels, Some("main.txt"));
    app.assert_sources(&["source"]);
    app.assert_active_pane(0);
    app.ui(|window, _| assert_eq!(window.find("chat").label(), Some("Chat 2")));
    app.keys(&["s", "l"]);
    app.assert_items(&labels, Some("Chat 2"));
    app.click_item("Chat 1");
    app.assert_active_pane(1);
    app.assert_sources(&["source"]);
    app.ui(|window, _| assert_eq!(window.find("chat").label(), Some("Chat 1")));
    // A pointer focus change must update the selection too, without changing contents.
    app.ui(|window, cx| {
        let source = window.find_by_label("Source code");
        window
            .within("fileview")
            .click(source.path().last().unwrap().clone(), cx);
    });
    app.assert_items(&labels, Some("main.txt"));
    app.assert_active_pane(0);
}

#[test]
fn chat_draft_caret_and_viewport_survive_file_and_chat_switches() {
    let project = Project::new(&[("main.txt", "source")]);
    let mut app = Driver::open_in(project.0.clone());
    app.cx.update(|cx| cx.set_reduce_motion(true));
    reply_with_messages(
        &app.backend.events,
        (0..40)
            .map(|i| Message::User(format!("Message {i}")))
            .collect(),
    );
    app.open_file("main.txt", &project.0.join("main.txt"));
    app.command("new");
    app.wait_for_chat(2);
    let second = app.new_backends.try_recv().unwrap();
    app.click_item("Chat 1");
    app.press("i");
    app.type_text("alpha beta");
    app.keys(&["ctrl-left", "escape"]);
    app.ui(|window, cx| {
        window.scroll(
            "conversation",
            ScrollDelta::Pixels(point(px(0.), px(100_000.))),
            cx,
        )
    });
    app.ui(|window, cx| {
        window.scroll(
            "conversation",
            ScrollDelta::Pixels(point(px(0.), px(-250.))),
            cx,
        )
    });
    let mut origin = point(px(0.), px(0.));
    let mut bar = Bounds::default();
    app.ui(|window, _| {
        origin = window.find(("message", 0_usize)).bounds().origin;
        assert!(origin.y < window.find("conversation").bounds().top());
        bar = window.find("open-items").bounds();
    });
    app.click_item("main.txt");
    app.click_item("Chat 2");
    app.press("i");
    app.type_text("second draft");
    app.keys(&["escape", "ctrl-n"]);
    app.assert_draft("alpha beta");
    app.ui(|window, _| {
        assert_eq!(window.find(("message", 0_usize)).bounds().origin, origin);
        assert_eq!(window.find("open-items").bounds(), bar);
    });
    app.press("i");
    app.type_text("|");
    app.assert_draft("alpha |beta");
    app.keys(&["escape", "ctrl-p"]);
    app.assert_draft("second draft");
    app.assert_no_requests();

    // Resetting the scrolled chat clears its view without replacing or reordering items.
    app.click_item("Chat 1");
    let labels = ["Chat 1", "main.txt", "Chat 2"];
    app.assert_items(&labels, Some("Chat 1"));
    app.command("new");
    for _ in 0..200 {
        app.ui(|_, _| {});
        if app.backend.events.is_closed() {
            break;
        }
        app.cx.advance_clock(Duration::from_millis(10));
    }
    assert!(app.backend.events.is_closed());
    app.assert_items(&labels, Some("Chat 1"));
    app.assert_draft("");
    app.ui(|window, _| assert!(window.try_find(("message", 0_usize)).is_none()));
    let reset = app.new_backends.try_recv().unwrap();
    reply_with_messages(&reset.events, vec![Message::User("New conversation".into())]);
    app.ui(|window, _| {
        let message = window.find(("message", 0_usize));
        assert!(message.visible());
        assert!(message.bounds().top() >= window.find("conversation").bounds().top());
        assert_eq!(window.find("open-items").bounds(), bar);
    });
    app.click_item("Chat 2");
    app.assert_draft("second draft");
    assert!(!second.prompts.is_closed());
    assert!(!second.events.is_closed());
}

#[test]
fn hidden_and_closed_chats_keep_their_draft_history_and_live_connection() {
    let project = Project::new(&[("main.txt", "source")]);
    let mut app = Driver::open_in(project.0.clone());
    app.open_file("main.txt", &project.0.join("main.txt"));
    app.keys(&["s", "v"]);
    app.command("new");
    app.wait_for_chat(2);
    let second = app.new_backends.try_recv().unwrap();
    for event in [
        AgentEvent::UserMessage("Question".into()),
        AgentEvent::AssistantStarted,
        AgentEvent::AssistantTextDelta("Partial".into()),
    ] {
        second.events.try_send(event).unwrap();
    }
    app.press("i");
    app.type_text("kept draft");
    app.press("escape");
    let mut short_answer_height = px(0.);
    app.ui(|window, _| {
        short_answer_height = window.find(("message", 1_usize)).bounds().size.height
    });
    app.press("ctrl-n"); // Hide Chat 2 behind Chat 1.
    second
        .events
        .try_send(AgentEvent::AssistantTextDelta(
            " answer\n\nReceived while hidden\n\nAnother paragraph".into(),
        ))
        .unwrap();
    app.click_item("Chat 2");
    app.assert_draft("kept draft");
    app.ui(|window, _| {
        let answer = window.find(("message", 1_usize));
        assert!(answer.visible());
        assert!(answer.bounds().size.height > short_answer_height);
    });
    app.command("close");
    let labels = ["Chat 1", "main.txt", "Chat 2"];
    app.assert_items(&labels, Some("main.txt"));
    second
        .events
        .try_send(AgentEvent::AssistantFinished("Completed answer".into()))
        .unwrap();
    second
        .events
        .try_send(AgentEvent::UserMessage("Still connected".into()))
        .unwrap();
    app.press("ctrl-n");
    app.assert_items(&labels, Some("Chat 2"));
    app.assert_draft("kept draft");
    app.ui(|window, _| {
        assert!(window.find(("message", 2_usize)).visible());
        assert!(window.try_find(("message", 3_usize)).is_none());
        assert_eq!(
            window.find(("message", 1_usize)).bounds().size.height,
            short_answer_height
        );
    });
    app.press("i");
    app.press("ctrl-enter");
    assert_eq!(second.prompts.try_recv().unwrap().text, "kept draft");
    assert!(second.prompts.is_empty());
    app.assert_no_requests();
    assert!(app.new_backends.is_empty());
}

#[test]
fn reopening_files_keeps_one_item_and_preserves_edits_undo_and_dirty_markers() {
    let project = Project::new(&[("main.txt", "original")]);
    let mut app = Driver::open_in(project.0.clone());
    app.open_file("main.txt", &project.0.join("main.txt"));
    app.press("A");
    app.type_text(" edited");
    app.press("escape");
    app.assert_items(&["Chat 1", "main.txt ●"], Some("main.txt ●"));
    app.click_item("Chat 1");
    app.open_file("main.txt", &project.0.join("main.txt"));
    app.assert_sources(&["original edited"]);
    app.assert_items(&["Chat 1", "main.txt ●"], Some("main.txt ●"));
    app.press("u");
    app.assert_sources(&["original"]);
    app.assert_items(&["Chat 1", "main.txt"], Some("main.txt"));
    app.press("ctrl-r");
    app.command("w");
    app.wait_saved(&project, "main.txt", "original edited");
    app.assert_items(&["Chat 1", "main.txt"], Some("main.txt"));
    app.keys(&["s", "v"]);
    app.open_file("main.txt", &project.0.join("main.txt"));
    app.press("A");
    app.type_text(" twice");
    app.press("escape");
    app.assert_items(&["Chat 1", "main.txt ●"], Some("main.txt ●"));
    app.assert_sources(&["original edited twice", "original edited twice"]);
    app.command("close");
    app.assert_items(&["Chat 1", "main.txt ●"], Some("main.txt ●"));
}

#[test]
fn item_bar_scrolls_horizontally_reveals_selection_and_stays_above_the_document() {
    let names: Vec<_> = (0..8)
        .map(|i| format!("source/long-document-number-{i}.txt"))
        .collect();
    let source: String = (0..100).map(|i| format!("line {i}\n")).collect();
    let files: Vec<_> = names
        .iter()
        .map(|name| (name.as_str(), source.as_str()))
        .collect();
    let project = Project::new(&files);
    let mut app = Driver::open_plain_in(project.0.clone());
    for name in &names {
        app.open_file(name, &project.0.join(name));
    }
    app.assert_item_visible(&names[7]);
    app.press("ctrl-n");
    app.assert_item_visible(&names[0]);
    app.press("ctrl-p");
    app.assert_item_visible(&names[7]);
    // Browsing the bar must not force it back to the selected item on every render.
    app.ui(|window, cx| {
        window.scroll(
            "open-items",
            ScrollDelta::Pixels(point(px(100_000.), px(0.))),
            cx,
        )
    });
    app.assert_item_visible(&names[0]);
    app.click_item(&names[0]);
    let mut bar = Bounds::default();
    let mut first = Bounds::default();
    app.ui(|window, cx| {
        bar = window.find("open-items").bounds();
        first = window
            .within("open-items")
            .find_by_label(&names[0])
            .bounds();
        let source = window.find_by_label("Source code");
        window.scroll(
            source.path().last().unwrap().clone(),
            ScrollDelta::Pixels(point(px(0.), px(-800.))),
            cx,
        );
    });
    app.ui(|window, _| {
        assert_eq!(window.find("open-items").bounds(), bar);
        assert_eq!(
            window
                .within("open-items")
                .find_by_label(&names[0])
                .bounds(),
            first
        );
    });
}

#[test]
fn selecting_an_item_discards_a_late_chat_connection() {
    for reset in [false, true] {
        let project = Project::new(&[("main.txt", "source")]);
        let mut app = Driver::open_in(project.0.clone());
        reply_with_messages(&app.backend.events, vec![Message::User("Keep history".into())]);
        app.press("i");
        app.type_text("Keep draft");
        app.press("escape");
        app.open_file("main.txt", &project.0.join("main.txt"));
        if reset {
            app.click_item("Chat 1");
        }
        let selected = if reset { "main.txt" } else { "Chat 1" };
        // Queue the connection and select an existing item before driving background work.
        app.cx
            .update_window(app.window, |_, window, cx| window.press(":", cx))
            .unwrap();
        app.cx
            .update_window(app.window, |_, window, cx| window.input("new", cx))
            .unwrap();
        app.cx
            .update_window(app.window, |_, window, cx| window.press("enter", cx))
            .unwrap();
        app.cx
            .update_window(app.window, |_, window, cx| {
                window.render_frame(cx);
                let item = window.within("open-items").find_by_label(selected);
                window
                    .within("open-items")
                    .click(item.path().last().unwrap().clone(), cx);
            })
            .unwrap();
        app.assert_items(&["Chat 1", "main.txt"], Some(selected));
        let discarded = app.new_backends.try_recv().unwrap();
        assert!(discarded.prompts.is_closed());
        assert!(discarded.events.is_closed());
        app.click_item("Chat 1");
        app.assert_draft("Keep draft");
        app.ui(|window, _| assert!(window.find(("message", 0_usize)).visible()));
        assert!(!app.backend.prompts.is_closed());
        assert!(!app.backend.events.is_closed());
    }
}

#[test]
fn cycling_to_chat_discards_a_late_file_load() {
    let project = Project::new(&[("a.txt", "alpha"), ("b.txt", "beta")]);
    let mut app = Driver::open_in(project.0.clone());
    app.open_file("a.txt", &project.0.join("a.txt"));
    app.finder("b.txt");
    app.cx
        .update_window(app.window, |_, window, cx| window.press("enter", cx))
        .unwrap();
    app.cx
        .update_window(app.window, |_, window, cx| window.press("ctrl-p", cx))
        .unwrap();
    app.assert_items(&["Chat 1", "a.txt"], Some("Chat 1"));
    app.ui(|window, _| assert_eq!(window.find("chat").label(), Some("Chat 1")));
    app.press("ctrl-n");
    app.assert_sources(&["alpha"]);
}
