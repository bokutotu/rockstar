use std::{
    fmt, fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use gpui_kit::{HeadlessAppContext, assets::Assets};
use image::{Rgba, RgbaImage};

use super::{load_source, open_markdown_window};

const CASES: [&str; 3] = ["math", "mermaid", "code_blocks"];
const TIMEOUT: Duration = Duration::from_secs(35);
const FRAME_INTERVAL: Duration = Duration::from_millis(16);
static RENDER_LOCK: Mutex<()> = Mutex::new(());

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

fn fixture(name: &str, extension: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/visual")
        .join(format!("{name}.{extension}"))
}

fn assert_snapshot(name: &str) {
    // Arrange
    let input = fixture(name, "md");
    let expected = Screenshot(
        image::open(fixture(name, "png"))
            .expect("read the committed reference image; do not generate it during a test")
            .into_rgba8(),
    );

    // Act
    let actual = capture(&input, Some(&expected));

    // Assert
    if actual != expected {
        save_difference(name, &actual, &expected);
    }
    assert_eq!(actual, expected, "visual regression in {name}");
}

fn capture(input: &std::path::Path, expected: Option<&Screenshot>) -> Screenshot {
    let _guard = RENDER_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    let source = load_source(input).expect("read the Markdown fixture");
    let window = cx
        .update(|cx| open_markdown_window(source, cx))
        .expect("open the production Markdown view in the headless platform");
    let started = Instant::now();
    let mut last_change = Instant::now();
    let mut previous = None;

    loop {
        // Drive GPUI tasks and frames, without reading application entities or caches.
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        })
        .expect("draw the Markdown window");
        let actual = Screenshot(
            cx.capture_screenshot(window)
                .expect("real offscreen rendering is required; run through test-visual"),
        );

        if let Some(expected) = expected {
            if actual == *expected || started.elapsed() >= TIMEOUT {
                return actual;
            }
        } else {
            // Only the explicit reference-update command uses visual stability.
            // Stability does not establish correctness: review every updated PNG.
            if previous.as_ref() != Some(&actual) {
                last_change = Instant::now();
            } else if last_change.elapsed() >= Duration::from_secs(1) {
                return actual;
            }
            assert!(
                started.elapsed() < TIMEOUT,
                "reference image did not settle"
            );
            previous = Some(actual);
        }
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
#[ignore = "rewrites reference PNGs; run test-visual update_snapshots -- --ignored, then review them"]
fn update_snapshots() {
    for name in CASES {
        let actual = capture(&fixture(name, "md"), None);
        let path = fixture(name, "png");
        actual.0.save(&path).expect("write the reference image");
        eprintln!("Review {}", path.display());
    }
}
