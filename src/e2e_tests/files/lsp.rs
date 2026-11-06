use super::*;
use crate::lsp::{CommandSpec, TestConfig, language::ServerKind};
use serde_json::Value;

mod documentation;

fn config() -> TestConfig {
    TestConfig {
        enabled: true,
        commands: [(
            ServerKind::Rust,
            CommandSpec {
                program: "python3".into(),
                args: vec![format!("{}/tests/mock_lsp.py", env!("CARGO_MANIFEST_DIR"))],
            },
        )]
        .into_iter()
        .collect(),
    }
}
fn open(project: &Project) -> Driver {
    Driver::open_config(project.0.clone(), None, Some(config()))
}
fn log(project: &Project) -> Vec<Value> {
    fs::read_to_string(project.0.join("lsp-log.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}
fn wait(app: &mut Driver, mut condition: impl FnMut(&mut Window) -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    loop {
        let mut ready = false;
        app.ui(|window, _| ready = condition(window));
        if ready {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "LSP UI did not reach expected state"
        );
        app.cx.advance_clock(Duration::from_millis(20));
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn ready(app: &mut Driver) {
    wait(app, |w| {
        w.find_all("file-status").iter().any(|e| {
            e.label()
                .is_some_and(|s| s.contains("rust-analyzer: ready"))
        })
    });
}
fn requested(app: &mut Driver, project: &Project, method: &str) {
    wait(app, |_| log(project).iter().any(|v| v["method"] == method));
}
fn fixture(extra: &[(&str, &str)]) -> Project {
    let mut files = vec![
        ("main.rs", "fn main() {}\n"),
        ("definition.rs", "fn found() {}\n"),
        ("second.rs", "fn second() {}\n"),
    ];
    files.extend_from_slice(extra);
    Project::new(&files)
}
#[test]
fn f12_opens_the_definition_and_f11_browses_both_call_directions() {
    let project = fixture(&[]);
    let mut app = open(&project);
    app.open_file("main.rs", &project.0.join("main.rs"));
    ready(&mut app);
    app.press("f11");
    wait(&mut app, |w| {
        w.find_all_by_label("caller · 1 call site(s)").len() == 1
    });
    app.press("tab");
    wait(&mut app, |w| {
        w.find_all_by_label("callee · 1 call site(s)").len() == 1
    });
    app.keys(&["down", "right"]);
    requested(&mut app, &project, "callHierarchy/outgoingCalls");
    app.press("enter");
    app.wait_source(&project.0.join("definition.rs"));
    app.assert_sources(&["fn found() {}\n"]);
    app.press("ctrl-p");
    app.wait_source(&project.0.join("main.rs"));
    app.press("f12");
    app.wait_source(&project.0.join("definition.rs"));
    assert!(
        log(&project)
            .iter()
            .any(|v| v["method"] == "textDocument/definition")
    );
}
#[test]
fn completion_uses_the_native_menu_and_applies_additional_edits_atomically() {
    let project = fixture(&[
        ("main.rs", "// prefix\ncom"),
        ("mock-options.json", r#"{"additional_edit":true}"#),
    ]);
    let mut app = open(&project);
    app.open_file("main.rs", &project.0.join("main.rs"));
    ready(&mut app);
    app.keys(&["G", "A", "ctrl-space"]);
    requested(&mut app, &project, "textDocument/completion");
    wait(&mut app, |w| {
        w.elements()
            .iter()
            .any(|e| e.label().is_some_and(|s| s.contains("completed")))
            || w.try_find("completion-menu").is_some()
    });
    app.press("enter");
    wait(&mut app, |w| {
        w.find_by_label("Source code").value() == Some("// imported\n// prefix\ncompleted")
    });
    app.keys(&["escape", "escape", "u"]);
    app.assert_sources(&["// prefix\ncom"]);
}
#[test]
fn two_views_share_one_connection_and_sync_unicode_undo_and_successful_saves() {
    let project = fixture(&[("main.rs", "// 日本\n")]);
    let mut app = open(&project);
    app.open_file("main.rs", &project.0.join("main.rs"));
    ready(&mut app);
    app.keys(&["s", "v"]);
    app.open_file("main.rs", &project.0.join("main.rs"));
    app.press("A");
    app.type_text("🎸");
    app.keys(&["escape", "escape"]);
    app.assert_sources(&["// 日本🎸\n", "// 日本🎸\n"]);
    app.command("w");
    app.wait_saved(&project, "main.rs", "// 日本🎸\n");
    requested(&mut app, &project, "textDocument/didSave");
    let messages = log(&project);
    assert_eq!(
        messages
            .iter()
            .filter(|v| v["method"] == "initialize")
            .count(),
        1
    );
    assert_eq!(
        messages
            .iter()
            .filter(|v| v["method"] == "textDocument/didOpen")
            .count(),
        1
    );
    let changes = messages
        .iter()
        .filter(|v| v["method"] == "textDocument/didChange")
        .collect::<Vec<_>>();
    assert_eq!(changes.len(), 1);
    assert_eq!(
        changes[0]["params"]["contentChanges"][0]["range"]["start"]["character"],
        5
    );
    assert_eq!(
        messages
            .iter()
            .find(|v| v["method"] == "textDocument/didSave")
            .unwrap()["params"]["text"],
        "// 日本🎸\n"
    );
    app.press("u");
    app.assert_sources(&["// 日本\n", "// 日本\n"]);
}
#[test]
fn delayed_definition_does_not_steal_a_view_after_its_document_changes() {
    let project = fixture(&[("mock-options.json", r#"{"delay_definition":0.4}"#)]);
    let mut app = open(&project);
    app.open_file("main.rs", &project.0.join("main.rs"));
    ready(&mut app);
    app.press("f12");
    requested(&mut app, &project, "textDocument/definition");
    app.press("A");
    app.type_text(" // changed");
    app.keys(&["escape", "escape"]);
    for _ in 0..30 {
        app.ui(|_, _| {});
        app.cx.advance_clock(Duration::from_millis(20));
        std::thread::sleep(Duration::from_millis(20));
    }
    app.assert_sources(&["fn main() {} // changed\n"]);
}
#[test]
fn missing_language_server_is_visible_and_does_not_prevent_editing() {
    let project = fixture(&[]);
    let mut config = config();
    config.commands.get_mut(&ServerKind::Rust).unwrap().program =
        "/nonexistent/rockstar-language-server".into();
    let mut app = Driver::open_config(project.0.clone(), None, Some(config));
    app.open_file("main.rs", &project.0.join("main.rs"));
    wait(&mut app, |w| {
        w.find_all("notification").iter().any(|e| e.visible())
    });
    app.press("A");
    app.type_text(" // offline");
    app.keys(&["escape", "escape"]);
    app.command("w");
    app.wait_saved(&project, "main.rs", "fn main() {} // offline\n");
}

const ANNOTATED: &str =
    "fn main() {\n    let answer = 42;\n    let doubled = answer * 2;\n}\nBROKEN\n";

fn annotated() -> (Project, Driver) {
    let project = fixture(&[
        ("main.rs", ANNOTATED),
        ("mock-options.json", r#"{"annotations":true}"#),
    ]);
    let mut app = open(&project);
    app.open_file("main.rs", &project.0.join("main.rs"));
    ready(&mut app);
    requested(&mut app, &project, "textDocument/codeLens");
    requested(&mut app, &project, "textDocument/inlayHint");
    for _ in 0..10 {
        app.ui(|_, _| {});
        app.cx.advance_clock(Duration::from_millis(50));
        std::thread::sleep(Duration::from_millis(10));
    }
    (project, app)
}

#[test]
fn annotation_hit_testing_navigation_copy_and_save_use_source_coordinates() {
    let (project, mut app) = annotated();
    // Offsets within the editor's lens row / after `42`, as checked by lsp.png.
    // Keep these relative to the editor, independent of workspace chrome.
    app.ui(|window, cx| {
        let source = window.find_by_label("Source code");
        let offset = gpui_kit::point(gpui_kit::px(32.), gpui_kit::px(17.));
        window.click_at(source.path().last().unwrap().clone(), offset, cx);
    });
    app.wait_source(&project.0.join("definition.rs"));
    requested(&mut app, &project, "workspace/executeCommand");
    app.press("ctrl-p");
    app.wait_source(&project.0.join("main.rs"));
    app.keys(&["g", "g", "V", "G", "y"]);
    app.ui(|_, cx| assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), ANNOTATED));
    app.keys(&["g", "g", "i"]);
    app.ui(|window, cx| {
        let source = window.find_by_label("Source code");
        let offset = gpui_kit::point(gpui_kit::px(171.), gpui_kit::px(50.));
        window.click_at(source.path().last().unwrap().clone(), offset, cx);
    });
    app.type_text("0");
    app.keys(&["escape", "escape"]);
    let expected = ANNOTATED.replace("42;", "420;");
    app.assert_sources(&[&expected]);
    app.command("w");
    app.wait_saved(&project, "main.rs", &expected);
}

#[test]
fn multiple_definitions_choose_a_target_and_unsupported_features_remain_hidden() {
    let project = fixture(&[(
        "mock-options.json",
        r#"{"multiple_definitions":true,"unsupported":["inlayHintProvider","codeLensProvider","callHierarchyProvider"]}"#,
    )]);
    let mut app = open(&project);
    app.open_file("main.rs", &project.0.join("main.rs"));
    ready(&mut app);
    app.press("f12");
    wait(&mut app, |w| w.try_find("lsp-popup").is_some());
    app.keys(&["down", "enter"]);
    app.wait_source(&project.0.join("second.rs"));
    app.press("f11");
    wait(&mut app, |w| {
        w.find_all("notification").iter().any(|e| e.visible())
    });
    assert!(!log(&project).iter().any(|v| matches!(
        v["method"].as_str(),
        Some(
            "textDocument/codeLens"
                | "textDocument/inlayHint"
                | "textDocument/prepareCallHierarchy"
        )
    )));
}

#[test]
fn full_sync_and_keyboard_hover_use_the_advertised_protocol() {
    let project = fixture(&[("mock-options.json", r#"{"sync":1}"#)]);
    let mut app = open(&project);
    app.open_file("main.rs", &project.0.join("main.rs"));
    ready(&mut app);
    app.press("K");
    requested(&mut app, &project, "textDocument/hover");
    wait(&mut app, |w| w.try_find("documentation-panel").is_some());
    app.press("escape");
    app.press("A");
    app.type_text(" // modified");
    app.keys(&["escape", "escape"]);
    requested(&mut app, &project, "textDocument/didChange");
    let messages = log(&project);
    let change = messages
        .iter()
        .find(|v| v["method"] == "textDocument/didChange")
        .unwrap();
    assert!(change["params"]["contentChanges"][0].get("range").is_none());
    assert_eq!(
        change["params"]["contentChanges"][0]["text"],
        "fn main() {} // modified\n"
    );
}

fn annotation_capture() -> image::RgbaImage {
    let project = fixture(&[
        ("main.rs", ANNOTATED),
        ("mock-options.json", r#"{"annotations":true}"#),
    ]);
    let mut app = open(&project);
    app.open_file("main.rs", &project.0.join("main.rs"));
    ready(&mut app);
    requested(&mut app, &project, "textDocument/codeLens");
    requested(&mut app, &project, "textDocument/inlayHint");
    app.keys(&["j", "w"]);
    requested(&mut app, &project, "textDocument/documentHighlight");
    for _ in 0..10 {
        app.ui(|_, _| {});
        app.cx.advance_clock(Duration::from_millis(50));
        std::thread::sleep(Duration::from_millis(10));
    }
    app.assert_sources(&[ANNOTATED]);
    app.ui(|window, cx| window.blur(cx));
    app.ui(|_, _| {});
    app.cx.capture_screenshot(app.window).unwrap()
}
#[test]
fn native_annotations_render_without_changing_the_document() {
    let actual = annotation_capture();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/visual/lsp.png");
    let expected = image::open(path)
        .expect("reviewed LSP screenshot")
        .into_rgba8();
    if actual != expected {
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/visual-diffs/lsp");
        fs::create_dir_all(&directory).unwrap();
        actual.save(directory.join("actual.png")).unwrap();
        panic!("LSP screenshot differs: target/visual-diffs/lsp/actual.png");
    }
}
#[test]
#[ignore = "writes the LSP reference image; review it before accepting"]
fn update_lsp_snapshot() {
    annotation_capture()
        .save(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/visual/lsp.png"))
        .unwrap();
}
