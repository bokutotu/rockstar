use super::*;
use gpui_kit::{InputEvent as _, ScrollDelta, point, px, size};
use serde_json::json;

const MARKDOWN: &str = include_str!("../../../../tests/visual/documentation.md");

fn open_documentation(contents: Value) -> (Project, Driver) {
    let options = json!({"hover_result": {"contents": contents}}).to_string();
    let project = fixture(&[("mock-options.json", &options)]);
    let mut app = open(&project);
    app.open_file("main.rs", &project.0.join("main.rs"));
    ready(&mut app);
    show(&mut app);
    (project, app)
}

fn show(app: &mut Driver) {
    app.press("K");
    wait(app, |w| w.try_find("documentation-panel").is_some());
    app.ui(|_, _| {});
}

fn assert_closed(app: &mut Driver) {
    app.ui(|window, _| {
        assert!(window.try_find("documentation-panel").is_none());
        assert_eq!(window.find_by_label("Source code").focused(), Some(true));
    });
}

fn body_image(app: &mut Driver) -> image::RgbaImage {
    let mut area = Bounds::default();
    let mut scale = 1.;
    app.ui(|window, _| {
        area = window.find("documentation-scroll").bounds();
        scale = window.scale_factor();
    });
    let image = app.cx.capture_screenshot(app.window).unwrap();
    image::imageops::crop_imm(
        &image,
        ((f32::from(area.left()) + 3.) * scale) as u32,
        ((f32::from(area.top()) + 3.) * scale) as u32,
        ((f32::from(area.size.width) - 6.) * scale) as u32,
        ((f32::from(area.size.height) - 6.) * scale) as u32,
    )
    .to_image()
}

#[test]
fn keyboard_documentation_scrolls_copies_and_returns_to_the_editor() {
    let markdown = (1..=70)
        .map(|i| format!("Documentation line {i:02}: 説明文です。\n\n"))
        .collect::<String>();
    let (_project, mut app) = open_documentation(json!({"kind":"markdown", "value":markdown}));
    app.ui(|window, _| {
        assert_eq!(window.find_by_label("Source code").focused(), Some(false));
    });
    let top = body_image(&mut app);
    app.press("j");
    assert!(
        body_image(&mut app) != top,
        "j must scroll the documentation"
    );
    app.press("k");
    assert!(body_image(&mut app) == top, "k must restore the viewport");
    app.press("pagedown");
    assert!(body_image(&mut app) != top, "PageDown must scroll");
    app.press("pageup");
    assert!(
        body_image(&mut app) == top,
        "PageUp must restore the viewport"
    );
    app.ui(|window, cx| {
        window.scroll(
            "documentation-scroll",
            ScrollDelta::Pixels(point(px(0.), px(-150.))),
            cx,
        );
    });
    assert!(body_image(&mut app) != top, "wheel must scroll");
    app.keys(&["ctrl-a", "ctrl-c"]);
    app.ui(|_, cx| {
        let copied = cx.read_from_clipboard().unwrap().text().unwrap();
        assert!(copied.starts_with("Documentation line 01:"), "{copied}");
        assert!(copied.contains("Documentation line 70:"), "{copied}");
    });
    // Editing and undo keys are not forwarded through the documentation.
    app.keys(&["i", "x", "ctrl-z", "ctrl-r"]);
    app.assert_sources(&["fn main() {}\n"]);
    app.press("escape");
    assert_closed(&mut app);
    show(&mut app);
    assert!(body_image(&mut app) == top, "reopening starts at the top");
    app.keys(&["escape", "A"]);
    app.type_text(" // edited");
    app.keys(&["escape", "escape"]);
    app.assert_sources(&["fn main() {} // edited\n"]);
}

#[test]
fn documentation_outside_click_is_consumed_without_moving_the_source_cursor() {
    let (_project, mut app) = open_documentation(json!({"kind":"markdown", "value":MARKDOWN}));
    app.ui(|window, cx| {
        let overlay = window.find("documentation-overlay");
        // Over the far-right end of the source's first line, outside the panel.
        window.click_at(
            "documentation-overlay",
            point(overlay.bounds().size.width - px(4.), px(6.)),
            cx,
        );
    });
    assert_closed(&mut app);
    app.press("i");
    app.type_text("prefix ");
    app.keys(&["escape", "escape"]);
    app.assert_sources(&["prefix fn main() {}\n"]);
}

#[test]
fn documentation_displays_plain_text_and_legacy_language_strings_literally() {
    for (contents, expected) in [
        (
            json!({"kind":"plaintext", "value":"# literal **stars** <b>tag</b> &amp; $x$\n    indented"}),
            "# literal **stars** <b>tag</b> &amp; $x$",
        ),
        (
            json!([{"language":"rust", "value":"fn example() { /* ``` ~~~ */ }"}, "**Description**"]),
            "fn example() { /* ``` ~~~ */ }",
        ),
    ] {
        let (_project, mut app) = open_documentation(contents);
        app.keys(&["ctrl-a", "ctrl-c"]);
        app.ui(|_, cx| {
            let copied = cx.read_from_clipboard().unwrap().text().unwrap();
            assert!(
                copied.contains(expected),
                "{copied:?} did not contain {expected:?}"
            );
        });
        app.assert_sources(&["fn main() {}\n"]);
    }
}

#[test]
fn documentation_supports_pointer_selection_and_copy() {
    let text = "Selection target.";
    let (_project, mut app) = open_documentation(json!({"kind":"markdown", "value":text}));
    app.ui(|window, cx| {
        let bounds = window.find("documentation-scroll").bounds();
        let padding = window.rem_size() * 1.5;
        let from = bounds.origin + point(padding + px(0.5), padding + px(8.));
        window.drag(from, from + point(px(250.), px(0.)), cx);
    });
    app.press("ctrl-c");
    app.ui(|_, cx| assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), text));
    app.assert_sources(&["fn main() {}\n"]);
    app.press("escape");
    assert_closed(&mut app);
}

#[test]
fn mouse_hover_remains_native_and_does_not_leak_through_documentation() {
    let project = fixture(&[]);
    let mut app = open(&project);
    app.open_file("main.rs", &project.0.join("main.rs"));
    ready(&mut app);
    let mut source = Bounds::default();
    let mut scale = 1.;
    app.ui(|window, _| {
        source = window.find_by_label("Source code").bounds();
        scale = window.scale_factor();
    });
    let crop = |image: image::RgbaImage| {
        image::imageops::crop_imm(
            &image,
            (f32::from(source.left()) * scale) as u32,
            (f32::from(source.top() + px(20.)) * scale) as u32,
            (400. * scale) as u32,
            (100. * scale) as u32,
        )
        .to_image()
    };
    let before = crop(app.cx.capture_screenshot(app.window).unwrap());
    let hover = |window: &mut Window, cx: &mut App| {
        window.dispatch_event(
            gpui_kit::MouseMoveEvent {
                position: source.origin + point(px(25.), px(8.)),
                pressed_button: None,
                modifiers: Default::default(),
            }
            .to_platform_input(),
            cx,
        );
    };
    app.ui(hover);
    requested(&mut app, &project, "textDocument/hover");
    settle_delayed_response(&mut app);
    assert_closed(&mut app);
    assert!(
        crop(app.cx.capture_screenshot(app.window).unwrap()) != before,
        "native hover must paint below the source line, not just request data"
    );
    show(&mut app);
    let count = || {
        log(&project)
            .iter()
            .filter(|v| v["method"] == "textDocument/hover")
            .count()
    };
    let requests = count();
    app.ui(hover);
    settle_delayed_response(&mut app);
    assert_eq!(
        count(),
        requests,
        "the modal backdrop must block source hover"
    );
    app.press("escape");
    assert_closed(&mut app);
    app.ui(hover);
    wait(&mut app, |_| count() > requests);
}

fn settle_delayed_response(app: &mut Driver) {
    for _ in 0..40 {
        app.ui(|_, _| {});
        app.cx.advance_clock(Duration::from_millis(20));
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn delayed_documentation_cannot_reopen_after_escape_movement_or_editing() {
    for action in ["escape", "move", "edit"] {
        let project = fixture(&[("mock-options.json", r#"{"delay_hover":0.2}"#)]);
        let mut app = open(&project);
        app.open_file("main.rs", &project.0.join("main.rs"));
        ready(&mut app);
        app.press("K");
        requested(&mut app, &project, "textDocument/hover");
        match action {
            "escape" => app.press("escape"),
            "move" => app.keys(&["l", "h"]),
            _ => {
                app.press("A");
                app.type_text(" // changed");
                app.keys(&["escape", "escape"]);
            }
        }
        settle_delayed_response(&mut app);
        assert_closed(&mut app);
    }
}

#[test]
fn delayed_documentation_does_not_steal_focus_after_leaving_and_returning() {
    let project = fixture(&[("mock-options.json", r#"{"delay_hover":0.2}"#)]);
    let mut app = open(&project);
    app.open_file("main.rs", &project.0.join("main.rs"));
    ready(&mut app);
    app.keys(&["s", "v", "s", "h", "K"]);
    requested(&mut app, &project, "textDocument/hover");
    app.keys(&["s", "l", "s", "h"]);
    settle_delayed_response(&mut app);
    assert_closed(&mut app);
}

#[test]
fn documentation_closes_when_another_pane_gets_focus() {
    let project = fixture(&[]);
    let mut app = open(&project);
    app.open_file("main.rs", &project.0.join("main.rs"));
    ready(&mut app);
    app.keys(&["s", "v", "s", "h"]);
    show(&mut app);
    app.ui(|window, cx| window.click("plain", cx));
    app.ui(|window, cx| {
        assert!(window.try_find("documentation-panel").is_none());
        assert_eq!(window.find_all("pane")[1].focused(), Some(true));
        assert_eq!(*cx.global::<crate::mode::Mode>(), crate::mode::Mode::Normal);
    });
    app.keys(&["s", "h"]);
    assert_closed(&mut app);
}

#[test]
fn empty_and_failed_documentation_notify_without_opening_a_panel() {
    for options in [
        json!({"hover_result":null}),
        json!({"hover_result":{"contents":{"kind":"markdown","value":"  "}}}),
        json!({"hover_error":true}),
        json!({"hover_result":{"contents":42}}),
        json!({"unsupported":["hoverProvider"]}),
    ] {
        let project = fixture(&[("mock-options.json", &options.to_string())]);
        let mut app = open(&project);
        app.open_file("main.rs", &project.0.join("main.rs"));
        ready(&mut app);
        app.press("K");
        wait(&mut app, |w| {
            w.find_all("notification").iter().any(|e| e.visible())
        });
        assert_closed(&mut app);
        app.assert_sources(&["fn main() {}\n"]);
    }
}

fn capture(small: bool) -> image::RgbaImage {
    let (_project, mut app) = open_documentation(json!({"kind":"markdown", "value":MARKDOWN}));
    for dimensions in [
        size(px(1100.), px(850.)),
        if small {
            size(px(480.), px(360.))
        } else {
            size(px(960.), px(720.))
        },
    ] {
        app.ui(|window, cx| {
            window.resize(dimensions);
            window.bounds_changed(cx);
        });
        app.ui(|window, _| {
            let source = window.find("source-area").bounds();
            let panel = window.find("documentation-panel").bounds();
            assert!((panel.size.width / source.size.width - 0.9).abs() < 0.01);
            assert!((panel.size.height / source.size.height - 0.8).abs() < 0.01);
            assert!(source.contains(&panel.origin));
            assert!(source.contains(&panel.bottom_right()));
        });
    }
    // Keep the panel focused: source has no blinking caret in Normal mode.
    app.ui(|_, _| {});
    app.cx.capture_screenshot(app.window).unwrap()
}

#[test]
fn documentation_renders_code_blocks_without_overlap_at_both_sizes() {
    for (small, name) in [(false, "documentation"), (true, "documentation-small")] {
        let actual = capture(small);
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/visual/{name}.png"));
        let expected = image::open(path)
            .expect("reviewed documentation screenshot")
            .into_rgba8();
        if actual != expected {
            let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join(format!("target/visual-diffs/{name}"));
            fs::create_dir_all(&directory).unwrap();
            actual.save(directory.join("actual.png")).unwrap();
            panic!("Documentation screenshot differs: target/visual-diffs/{name}/actual.png");
        }
    }
}

#[test]
#[ignore = "writes documentation reference images; review before accepting"]
fn update_documentation_snapshots() {
    for (small, name) in [(false, "documentation"), (true, "documentation-small")] {
        capture(small)
            .save(
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/visual/{name}.png")),
            )
            .unwrap();
    }
}
