use super::*;
use gpui_kit::{ScrollDelta, point, px};
use image::RgbaImage;
use std::ops::Range;

fn capture() -> RgbaImage {
    let project = Project::new(&[(
        "src/main.rs",
        "// 日本語のソースコード\nfn main() {\n    let greeting = \"Hello, Rockstar!\";\n    for number in 0..3 {\n        println!(\"{greeting} {number}\");\n    }\n}\n",
    )]);
    let mut app = Driver::open_plain_in(project.0.clone());
    app.open_file("src/main.rs", &project.0.join("src/main.rs"));
    app.keys(&["4", "j", "w", "ctrl-b"]);
    for _ in 0..100 {
        let mut ready = false;
        app.ui(|window, _| ready = window.find_all_by_label("src/").len() == 1);
        if ready {
            break;
        }
        app.cx.advance_clock(Duration::from_millis(10));
    }
    app.keys(&["l", "escape"]);
    app.ui(|window, cx| window.blur(cx));
    app.ui(|_, _| {});
    app.cx.capture_screenshot(app.window).unwrap()
}

#[test]
fn fileview_renders_source_highlights_relative_numbers_and_the_file_tree() {
    let expected = image::open(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/visual/fileview.png"
    ))
    .unwrap()
    .into_rgba8();
    let actual = capture();
    if actual != expected {
        let output = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/visual-diffs/fileview");
        fs::create_dir_all(&output).unwrap();
        actual.save(output.join("actual.png")).unwrap();
        expected.save(output.join("expected.png")).unwrap();
        panic!("fileview screenshot differs; review target/visual-diffs/fileview");
    }
}

fn ink_bands(image: &RgbaImage, x: Range<u32>, y: Range<u32>) -> Vec<Range<u32>> {
    let mut bands: Vec<Range<u32>> = Vec::new();
    for y in y {
        if x.clone()
            .any(|x| image.get_pixel(x, y).0[..3].iter().any(|&c| c < 200))
        {
            if let Some(band) = bands.last_mut().filter(|band| band.end == y) {
                band.end = y + 1;
            } else {
                bands.push(y..y + 1);
            }
        }
    }
    bands
}

fn assert_line_numbers_align(app: &mut Driver, stage: &str) -> RgbaImage {
    let mut focus = None;
    let mut source = Bounds::default();
    let mut view = Bounds::default();
    let mut scale = 1.;
    app.ui(|window, cx| {
        focus = window.focused(cx);
        window.blur(cx); // Exclude the caret from the source's ink bands.
    });
    app.ui(|window, _| {
        source = window.find_by_label("Source code").bounds();
        view = window.find("fileview").bounds();
        scale = window.scale_factor();
    });
    let image = app.cx.capture_screenshot(app.window).unwrap();
    let device = |p: Pixels| (f32::from(p) * scale).round() as u32;
    let y = device(source.top() + px(2.))..device(source.bottom() - px(2.));
    let gutter_x = device(view.left() + px(2.))..device(source.left() - px(2.));
    let source_x = device(source.left() + px(2.))..device(source.left() + px(100.));
    let numbers = ink_bands(&image, gutter_x, y.clone());
    let text = ink_bands(&image, source_x.clone(), y.clone());
    assert!(!text.is_empty(), "{stage}: source text must be visible");
    // All fixture lines contain only digits. Allow one logical pixel for the
    // different glyphs' antialiasing, but require a number beside every line.
    let tolerance = scale.ceil() as u32;
    let aligned = numbers.len() == text.len()
        && numbers.iter().zip(&text).all(|(number, text)| {
            number.start.abs_diff(text.start) <= tolerance
                && number.end.abs_diff(text.end) <= tolerance
        });
    if !aligned {
        let output =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/visual-diffs/line-numbers");
        fs::create_dir_all(&output).unwrap();
        image.save(output.join(format!("{stage}.png"))).unwrap();
    }
    assert!(
        aligned,
        "{stage}: line numbers {numbers:?} must align with source {text:?}; see target/visual-diffs/line-numbers"
    );
    app.ui(|window, cx| {
        if let Some(focus) = focus {
            focus.focus(window, cx);
        }
    });
    image::imageops::crop_imm(
        &image,
        source_x.start,
        y.start,
        source_x.len() as u32,
        y.len() as u32,
    )
    .to_image()
}

#[test]
fn line_numbers_stay_aligned_when_scrolling_a_long_file() {
    let source = (1..=1000)
        .map(|line| format!("{line:04}"))
        .collect::<Vec<_>>()
        .join("\n");
    let project = Project::new(&[("long.txt", &source)]);
    let mut app = Driver::open_plain_in(project.0.clone());
    app.cx.update(|cx| cx.set_reduce_motion(true));
    app.open_file("long.txt", &project.0.join("long.txt"));
    let top = assert_line_numbers_align(&mut app, "top");

    app.keys(&["5", "0", "0", "G"]);
    app.assert_status("NORMAL · 500:1 · Saved");
    let middle = assert_line_numbers_align(&mut app, "middle");
    assert!(middle != top, "the middle of the file must be visible");

    app.press("G");
    app.assert_status("NORMAL · 1000:1 · Saved");
    let bottom = assert_line_numbers_align(&mut app, "bottom");
    assert!(bottom != middle, "the end of the file must be visible");

    app.keys(&["g", "g"]);
    let mut previous = assert_line_numbers_align(&mut app, "back-to-top");
    assert!(previous == top, "gg must restore the first viewport");
    for (stage, delta) in [
        ("wheel-down", -187.5),
        ("wheel-further-down", -1200.25),
        ("wheel-up", 900.5),
        ("wheel-bottom", -100_000.),
        ("wheel-top", 100_000.),
    ] {
        app.ui(|window, cx| {
            let source = window.find_by_label("Source code");
            window.scroll(
                source.path().last().unwrap().clone(),
                ScrollDelta::Pixels(point(px(0.), px(delta))),
                cx,
            );
        });
        let current = assert_line_numbers_align(&mut app, stage);
        assert!(
            current != previous,
            "{stage}: scrolling must change the viewport"
        );
        previous = current;
    }
    assert!(
        previous == top,
        "scrolling back must restore the first viewport"
    );
    app.assert_sources(&[&source]);
}

#[test]
#[ignore = "writes the fileview reference image; review the PNG before accepting it"]
fn update_fileview_snapshot() {
    capture()
        .save(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/visual/fileview.png"
        ))
        .unwrap();
}
