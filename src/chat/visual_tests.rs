use std::{
    fmt, fs,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

use gpui_kit::{
    ScrollDelta, point, px, size,
    test::{TestQueryExt as _, TestWindowExt as _},
};
use image::{Rgba, RgbaImage};

use crate::{
    agent::{Message, PromptRequest},
    test_support::{RENDER_LOCK, headless_app, mock_agent, open_app_window, reply_with_messages},
};

const CASES: [&str; 4] = ["conversation", "math", "mermaid", "code_blocks"];
const SNAPSHOT_UPDATE_TIMEOUT: Duration = Duration::from_secs(35);
const FRAME_INTERVAL: Duration = Duration::from_millis(16);

// Compare every pixel, but do not dump megabytes of pixels on assertion failure.
#[derive(PartialEq)]
struct Screenshot(RgbaImage);

impl fmt::Debug for Screenshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Screenshot")
            .field("dimensions", &self.0.dimensions())
            .finish_non_exhaustive()
    }
}

fn sample(name: &str) -> (String, Vec<Message>) {
    let request = match name {
        "conversation" => "Markdown の表示例を見せてください。",
        "math" => "数式を含む説明をお願いします。",
        "mermaid" => "処理の流れを図にしてください。",
        "code_blocks" => "Rust、Python、TypeScript のコード例を見せてください。",
        _ => unreachable!(),
    };
    let response = fs::read_to_string(fixture(name, "md")).expect("read the response fixture");
    let messages = vec![Message::User(request.into()), Message::Assistant(response)];
    (request.into(), messages)
}

#[test]
fn conversation() {
    assert_snapshot("conversation");
}

#[test]
fn math() {
    assert_snapshot("math");
}

#[test]
fn mermaid() {
    assert_snapshot("mermaid");
}

#[test]
fn code_blocks() {
    assert_snapshot("code_blocks");
}

#[test]
fn command_palette() {
    let name = "command_palette";
    let (input, messages) = sample("conversation");
    let expected = Screenshot(
        image::open(fixture(name, "png"))
            .expect("read the reviewed command popup reference image")
            .into_rgba8(),
    );
    let actual = capture(messages, &input, "An unsent draft", Some("new"), false);
    if actual != expected {
        save_difference(name, &actual, &expected);
    }
    assert_eq!(actual, expected, "visual regression in {name}");
}

fn fixture(name: &str, extension: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/visual")
        .join(format!("{name}.{extension}"))
}

fn assert_snapshot(name: &str) {
    // Arrange
    let (input, messages) = sample(name);
    let draft = "次は別の例をお願いします。";
    let expected = Screenshot(
        image::open(fixture(name, "png"))
            .expect("read the committed reference image; do not generate it during a test")
            .into_rgba8(),
    );

    // Act
    let actual = capture(messages, &input, draft, None, false);

    // Assert
    if actual != expected {
        save_difference(name, &actual, &expected);
    }
    assert_eq!(actual, expected, "visual regression in {name}");
}

fn capture(
    messages: Vec<Message>,
    input: &str,
    draft: &str,
    command: Option<&str>,
    wait_for_stability: bool,
) -> Screenshot {
    let _guard = RENDER_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut cx = headless_app();
    let (agent, backend) = mock_agent();
    let window = cx
        .update(|cx| open_app_window(agent, cx))
        .expect("open the production app view with a mock agent");
    cx.update_window(window, |_, window, cx| {
        window.resize(size(px(960.0), px(1100.0)));
        window.bounds_changed(cx);
        window.activate_window();
        window.render_frame(cx);
        let field = window.find_by_label("Message to pi");
        window.click(field.path().last().unwrap().clone(), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.input(input, cx);
        window.press("ctrl-enter", cx);
    })
    .unwrap();
    assert!(matches!(
        backend.prompts.try_recv(),
        Ok(PromptRequest { text }) if text == input
    ));
    reply_with_messages(&backend.events, messages);
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let field = window.find_by_label("Message to pi");
        window.click(field.path().last().unwrap().clone(), cx);
        window.input(draft, cx);
        window.scroll(
            "conversation",
            ScrollDelta::Pixels(point(px(0.0), px(100_000.0))),
            cx,
        );
        if let Some(command) = command {
            window.press("escape", cx);
            window.press(":", cx);
            let mut popup = window.within("command-popup");
            let field = popup.find_by_label("Command");
            popup.click(field.path().last().unwrap().clone(), cx);
            window.input(command, cx);
        }
        // A stable, unfocused editor avoids cursor-blink differences in snapshots.
        window.blur(cx);
    })
    .unwrap();
    let started = Instant::now();
    let mut last_change = Instant::now();
    let mut previous = None;

    loop {
        // Correctness is determined only by rendered pixels, not private view state.
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        })
        .expect("draw the chat window");
        let actual = Screenshot(
            cx.capture_screenshot(window)
                .expect("capture the offscreen-rendered window"),
        );

        // Regression tests capture once; a mismatch must fail without retrying.
        if !wait_for_stability {
            return actual;
        }

        // Only the explicit reference-update command uses visual stability.
        // Stability does not establish correctness: review every updated PNG.
        if previous.as_ref() != Some(&actual) {
            last_change = Instant::now();
        } else if last_change.elapsed() >= Duration::from_secs(1) {
            return actual;
        }
        assert!(
            started.elapsed() < SNAPSHOT_UPDATE_TIMEOUT,
            "reference image did not settle"
        );
        previous = Some(actual);
        cx.advance_clock(FRAME_INTERVAL);
        thread::sleep(FRAME_INTERVAL);
    }
}

fn save_difference(name: &str, actual: &Screenshot, expected: &Screenshot) {
    let directory =
        PathBuf::from(std::env::var_os("CARGO_TARGET_DIR").unwrap_or_else(|| "target".into()))
            .join("visual-diffs")
            .join(name);
    fs::create_dir_all(&directory).expect("create the visual regression artifact directory");
    actual.0.save(directory.join("actual.png")).unwrap();
    expected.0.save(directory.join("expected.png")).unwrap();
    let difference = RgbaImage::from_fn(
        actual.0.width().max(expected.0.width()),
        actual.0.height().max(expected.0.height()),
        |x, y| {
            if actual.0.get_pixel_checked(x, y) == expected.0.get_pixel_checked(x, y) {
                Rgba([0, 0, 0, 255])
            } else {
                Rgba([255, 0, 255, 255])
            }
        },
    );
    difference.save(directory.join("difference.png")).unwrap();
    eprintln!("Visual regression artifacts: {}", directory.display());
}

#[test]
#[ignore = "rewrites reference PNGs; run cargo test update_snapshots -- --ignored, then review them"]
fn update_snapshots() {
    for name in CASES {
        let (input, messages) = sample(name);
        let actual = capture(messages, &input, "次は別の例をお願いします。", None, true);
        let path = fixture(name, "png");
        actual.0.save(&path).expect("write the reference image");
        eprintln!("Review {}", path.display());
    }
}

#[test]
#[ignore = "rewrites the command popup PNG; run cargo test update_command_snapshots -- --ignored, then review it"]
fn update_command_snapshots() {
    let (input, messages) = sample("conversation");
    let actual = capture(messages, &input, "An unsent draft", Some("new"), true);
    let path = fixture("command_palette", "png");
    actual
        .0
        .save(&path)
        .expect("write the command popup reference");
    eprintln!("Review {}", path.display());
}
