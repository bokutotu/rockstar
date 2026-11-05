//! Exercise editing through the production window, without inspecting editor state.
use super::*;

#[derive(Clone, Copy, Debug)]
enum Field {
    Message,
    Command,
}

const FIELDS: [Field; 2] = [Field::Message, Field::Command];

impl Field {
    fn open(self) -> Driver {
        let mut app = Driver::open();
        app.press(match self {
            Self::Message => "i",
            Self::Command => ":",
        });
        self.assert_value(&mut app, "");
        app
    }

    fn assert_value(self, app: &mut Driver, expected: &str) {
        let label = match self {
            Self::Message => "Message to pi",
            Self::Command => "Command",
        };
        app.ui(|window, _| {
            let input = window.find_by_label(label);
            assert_eq!(input.value(), Some(expected), "{self:?}");
            assert_eq!(input.focused(), Some(true), "{self:?}");
        });
    }
}

/// `expected` marks the caret with `|`. Typing the marker verifies both its
/// position and the collapsed selection through a user-visible edit.
fn check_edit(field: Field, text: &str, keys: &[&str], expected: &str) {
    let mut app = field.open();
    app.type_text(text);
    for key in keys {
        app.press(key);
    }
    field.assert_value(&mut app, &expected.replace('|', ""));
    app.type_text("|");
    field.assert_value(&mut app, expected);
}

#[test]
fn line_shortcuts_place_the_caret_in_both_inputs() {
    for field in FIELDS {
        check_edit(field, "one two", &["ctrl-a"], "|one two");
        check_edit(field, "one two", &["home", "ctrl-e"], "one two|");
        check_edit(
            field,
            "abc def",
            &["home", "right", "right", "right", "ctrl-u"],
            "| def",
        );
        check_edit(
            field,
            "abc def",
            &["home", "right", "right", "right", "ctrl-k"],
            "abc|",
        );
    }
}

#[test]
fn word_motion_uses_unicode_boundaries_in_both_inputs() {
    let cases: &[(&str, &[&str], &str)] = &[
        ("one two", &["ctrl-left"], "one |two"),
        ("one two", &["home", "ctrl-right"], "one| two"),
        ("one two", &["home", "ctrl-right", "ctrl-right"], "one two|"),
        ("one  two  ", &["ctrl-left"], "one  |two  "),
        ("日本語", &["ctrl-left"], "日本|語"),
        ("日本語", &["home", "ctrl-right"], "日|本語"),
        ("カタカナ", &["ctrl-left"], "|カタカナ"),
        ("カタカナ", &["home", "ctrl-right"], "カタカナ|"),
        ("src/chat/main.rs", &["ctrl-left"], "src/chat/|main.rs"),
        (
            "src/chat/main.rs",
            &["ctrl-left", "ctrl-left"],
            "src/chat|/main.rs",
        ),
        (
            "src/chat/main.rs",
            &["home", "ctrl-right"],
            "src|/chat/main.rs",
        ),
    ];
    for field in FIELDS {
        for &(text, keys, expected) in cases {
            check_edit(field, text, keys, expected);
        }
    }
}

#[test]
fn word_deletion_uses_unicode_boundaries_in_both_inputs() {
    for field in FIELDS {
        for (text, expected) in [
            ("one two", "one |"),
            ("one two   ", "one |"),
            ("日本語", "日本|"),
            ("こんにちは", "こんにち|"),
            ("カタカナ", "|"),
            ("src/chat/main.rs", "src/chat/|"),
            ("hi e\u{301}", "hi |"),
            ("hi 👨‍👩‍👧‍👦", "hi |"),
        ] {
            check_edit(field, text, &["ctrl-w"], expected);
        }
    }
}

#[test]
fn empty_inputs_and_document_edges_have_defined_caret_positions() {
    for field in FIELDS {
        check_edit(
            field,
            "",
            &[
                "ctrl-a",
                "ctrl-e",
                "ctrl-left",
                "ctrl-right",
                "ctrl-w",
                "ctrl-u",
                "ctrl-k",
            ],
            "|",
        );
        check_edit(
            field,
            "abc",
            &["home", "ctrl-left", "ctrl-u", "ctrl-w"],
            "|abc",
        );
        check_edit(field, "abc", &["ctrl-right", "ctrl-k"], "abc|");
    }
}

#[test]
fn deletion_shortcuts_replace_the_selected_text_in_both_inputs() {
    for field in FIELDS {
        for key in ["ctrl-w", "ctrl-u", "ctrl-k"] {
            check_edit(
                field,
                "one two three",
                &[
                    "home",
                    "right",
                    "right",
                    "right",
                    "right",
                    "shift-right",
                    "shift-right",
                    "shift-right",
                    key,
                ],
                "one | three",
            );
        }
    }
}

#[test]
fn undo_restores_text_and_caret_after_each_deletion_shortcut() {
    for field in FIELDS {
        for (key, deleted, restored) in [
            ("ctrl-w", "one ", "one two|"),
            ("ctrl-u", "", "one two|"),
            ("ctrl-k", "", "|one two"),
        ] {
            let mut app = field.open();
            app.type_text("one two");
            if key == "ctrl-k" {
                app.press("home");
            }
            app.press(key);
            field.assert_value(&mut app, deleted);
            app.press("ctrl-z");
            field.assert_value(&mut app, "one two");
            app.type_text("|");
            field.assert_value(&mut app, restored);
        }
    }
}

#[test]
fn undo_restores_the_selection_after_deleting_selected_text() {
    for field in FIELDS {
        let mut app = field.open();
        app.type_text("one two three");
        for key in [
            "home",
            "right",
            "right",
            "right",
            "right",
            "shift-right",
            "shift-right",
            "shift-right",
            "ctrl-w",
        ] {
            app.press(key);
        }
        field.assert_value(&mut app, "one  three");
        app.press("ctrl-z");
        field.assert_value(&mut app, "one two three");
        app.type_text("|");
        field.assert_value(&mut app, "one | three");
    }
}

#[test]
fn multiline_shortcuts_edit_the_current_line_and_join_at_line_edges() {
    let cases: &[(&[&str], &str)] = &[
        (
            &["right", "right", "right", "ctrl-a"],
            "first\n|second\nthird",
        ),
        (
            &["right", "right", "right", "ctrl-e"],
            "first\nsecond|\nthird",
        ),
        (&["right", "right", "right", "ctrl-u"], "first\n|ond\nthird"),
        (&["right", "right", "right", "ctrl-k"], "first\nsec|\nthird"),
        (&["ctrl-u"], "first|second\nthird"),
        (&["end", "ctrl-k"], "first\nsecond|third"),
        (&["ctrl-w"], "|second\nthird"),
    ];
    for &(keys, expected) in cases {
        let keys = [vec!["ctrl-home", "down"], keys.to_vec()].concat();
        check_edit(Field::Message, "first\nsecond\nthird", &keys, expected);
    }
}

#[test]
fn soft_wrapped_text_uses_logical_line_edges() {
    let line = "word ".repeat(80);
    let text = format!("first\n{line}");
    check_edit(
        Field::Message,
        &text,
        &["ctrl-a"],
        &format!("first\n|{line}"),
    );
    check_edit(
        Field::Message,
        &text,
        &["ctrl-home", "down", "right", "ctrl-e"],
        &format!("{text}|"),
    );
    check_edit(Field::Message, &text, &["ctrl-u"], "first\n|");
    check_edit(
        Field::Message,
        &text,
        &["ctrl-home", "down", "ctrl-k"],
        "first\n|",
    );
}

#[test]
fn normal_mode_preserves_the_draft_and_insert_mode_edits_it() {
    let mut app = Field::Message.open();
    app.type_text("one two");
    app.press("escape");
    for key in [
        "ctrl-a",
        "ctrl-e",
        "ctrl-left",
        "ctrl-right",
        "ctrl-w",
        "ctrl-u",
        "ctrl-k",
    ] {
        app.press(key);
    }
    app.assert_draft("one two");
    app.ui(|window, _| {
        assert_eq!(window.find_by_label("Message to pi").focused(), Some(false));
    });
    app.press("i");
    app.press("ctrl-w");
    Field::Message.assert_value(&mut app, "one ");
    app.type_text("three");
    Field::Message.assert_value(&mut app, "one three");
}

#[test]
fn edited_drafts_are_shared_between_panes_and_submit_once() {
    let mut app = Field::Message.open();
    app.type_text("one two");
    app.press("ctrl-w");
    app.assert_draft("one ");
    for key in ["escape", "s", "v", "ctrl-n", "i"] {
        app.press(key);
    }
    app.ui(|window, _| {
        let drafts: Vec<_> = window
            .find_all_by_label("Message to pi")
            .iter()
            .map(|input| input.value().unwrap().to_string())
            .collect();
        assert_eq!(drafts, ["one ", "one "]);
    });
    app.press("ctrl-e");
    app.type_text("three");
    app.ui(|window, _| {
        let drafts: Vec<_> = window
            .find_all_by_label("Message to pi")
            .iter()
            .map(|input| input.value().unwrap().to_string())
            .collect();
        assert_eq!(drafts, ["one three", "one three"]);
    });
    app.press("ctrl-enter");
    let mut requests = Vec::new();
    while let Ok(request) = app.backend.prompts.try_recv() {
        requests.push(request.text);
    }
    assert_eq!(requests, ["one three"]);
    app.ui(|window, _| {
        let drafts: Vec<_> = window
            .find_all_by_label("Message to pi")
            .iter()
            .map(|input| input.value().unwrap().to_string())
            .collect();
        assert_eq!(drafts, ["", ""]);
    });
}

#[test]
fn edited_command_opens_a_new_chat() {
    let mut app = Field::Command.open();
    app.type_text("new trailing");
    app.press("ctrl-w");
    Field::Command.assert_value(&mut app, "new ");
    app.press("enter");
    app.wait_for_chat(2);
    app.ui(|window, _| {
        assert_eq!(window.find("chat").label(), Some("Chat 2"));
    });
    app.press("i");
    app.type_text("New draft");
    Field::Message.assert_value(&mut app, "New draft");
}

#[test]
fn edited_mention_completes_to_the_matching_path() {
    let root = mention_workspace("input-shortcuts");
    let mut app = Driver::open_in(root.clone());
    app.press("i");
    app.type_text("see @missing");
    app.press("ctrl-w");
    app.assert_draft("see @");
    app.type_text("main");
    assert_eq!(app.mention_options(), Some(vec!["src/main.rs".to_string()]));
    app.press("tab");
    Field::Message.assert_value(&mut app, "see @src/main.rs ");
    fs::remove_dir_all(root).unwrap();
}
