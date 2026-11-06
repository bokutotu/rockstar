use super::*;

#[test]
fn chat_contents_and_source_editing_survive_pi_connection_failure() {
    let project = Project::new(&[("main.rs", "hello\n")]);
    let (agent, backend) = mock_agent();
    let first = std::sync::Mutex::new(Some(agent));
    let mut app = Driver::open_using(
        project.0.clone(),
        Some(Arc::new(move |_| {
            first
                .lock()
                .unwrap()
                .take()
                .ok_or_else(|| anyhow::anyhow!("pi unavailable"))
        })),
    );
    app.cx.update(|cx| cx.set_reduce_motion(true));
    app.ui(|window, _| assert_eq!(window.find("plain").label(), Some("Plain")));
    app.command("chat");
    app.wait_for_chat(1);
    backend
        .events
        .try_send(crate::agent::AgentEvent::UserMessage("Keep history".into()))
        .unwrap();
    app.press("i");
    app.type_text("Keep draft");
    app.press("escape");
    app.command("new");
    for _ in 0..200 {
        let mut shown = false;
        app.ui(|window, _| shown = window.find_all("notification").iter().any(|e| e.visible()));
        if shown {
            break;
        }
        app.cx.advance_clock(Duration::from_millis(10));
    }
    app.assert_draft("Keep draft");
    app.ui(|window, _| {
        assert!(window.find("notification").visible());
        assert!(window.find(("message", 0_usize)).visible());
        assert_eq!(window.find("chat").label(), Some("Chat 1"));
        assert_eq!(window.within("open-items").find_all_by_role(Role::Tab).len(), 1);
    });
    assert!(!backend.events.is_closed());
    app.press("i");
    app.press("ctrl-enter");
    assert_eq!(backend.prompts.try_recv().unwrap().text, "Keep draft");
    app.press("escape");
    app.open_file("main.rs", &project.0.join("main.rs"));
    app.press("A");
    app.type_text(" world");
    app.press("escape");
    app.command("chat");
    for _ in 0..200 {
        let mut shown = false;
        app.ui(|window, _| shown = window.find_all("notification").len() == 2);
        if shown {
            break;
        }
        app.cx.advance_clock(Duration::from_millis(10));
    }
    app.assert_sources(&["hello world\n"]);
    app.ui(|window, _| {
        assert_eq!(window.find_all("notification").len(), 2);
        assert_eq!(window.find_by_label("Source code").focused(), Some(true));
    });
    app.command("w");
    app.wait_saved(&project, "main.rs", "hello world\n");
    app.assert_status("NORMAL · 1:11 · Saved");
}

#[test]
fn unsupported_file_retains_the_existing_document_and_reports_the_error() {
    let project = Project::new(&[("main.rs", "keep\n"), ("binary.dat", "a\0b")]);
    let mut app = Driver::open_plain_in(project.0.clone());
    app.cx.update(|cx| cx.set_reduce_motion(true));
    app.open_file("main.rs", &project.0.join("main.rs"));
    app.press("A");
    app.type_text(" editing");
    app.press("escape");
    app.finder("binary.dat");
    app.press("enter");
    for _ in 0..200 {
        let mut shown = false;
        app.ui(|window, _| shown = window.find_all("notification").iter().any(|e| e.visible()));
        if shown {
            break;
        }
        app.cx.advance_clock(Duration::from_millis(10));
    }
    app.assert_sources(&["keep editing\n"]);
    app.assert_status("NORMAL · 1:12 · Modified");
    app.ui(|window, _| assert!(window.find("notification").visible()));
    app.press("A");
    app.type_text("!");
    app.press("escape");
    app.assert_sources(&["keep editing!\n"]);
}

#[test]
fn a_file_load_finishing_after_its_pane_closed_leaves_the_replacement_plain() {
    let project = Project::new(&[("main.rs", "keep\n"), ("other.rs", "other\n")]);
    let mut app = Driver::open_plain_in(project.0.clone());
    app.open_file("main.rs", &project.0.join("main.rs"));
    app.keys(&["s", "v"]);
    app.finder("other.rs");
    // Deliver input without running background work between the open and close.
    app.cx
        .update_window(app.window, |_, window, cx| window.press("enter", cx))
        .unwrap();
    app.cx
        .update_window(app.window, |_, window, cx| window.press(":", cx))
        .unwrap();
    app.cx
        .update_window(app.window, |_, window, cx| window.input("close", cx))
        .unwrap();
    app.cx
        .update_window(app.window, |_, window, cx| window.press("enter", cx))
        .unwrap();
    for key in ["s", "v"] {
        app.cx
            .update_window(app.window, |_, window, cx| window.press(key, cx))
            .unwrap();
    }
    app.assert_sources(&["keep\n"]);
    app.ui(|window, _| assert_eq!(window.find("plain").label(), Some("Plain")));
    app.assert_active_pane(1);
    app.command("chat");
    app.wait_for_chat(1);
    app.assert_sources(&["keep\n"]);
}

#[test]
fn each_file_view_keeps_its_viewport_when_another_view_edits_the_document() {
    let source = (1..=100)
        .map(|i| format!("line {i:03}\n"))
        .collect::<String>();
    let project = Project::new(&[("long.txt", &source)]);
    let mut app = Driver::open_plain_in(project.0.clone());
    app.open_file("long.txt", &project.0.join("long.txt"));
    app.keys(&["s", "v"]);
    app.open_file("long.txt", &project.0.join("long.txt"));
    let mut area = Bounds::default();
    let mut scale = 1.0;
    app.ui(|window, _| {
        area = window.find_all_by_label("Source code")[0].bounds();
        scale = window.scale_factor();
    });
    let crop = |image: image::RgbaImage| {
        // Exclude borders, the caret, and scrollbars. Compare the actual visible source.
        image::imageops::crop_imm(
            &image,
            ((f32::from(area.left()) + 15.) * scale) as u32,
            ((f32::from(area.top()) + 15.) * scale) as u32,
            ((f32::from(area.size.width) - 30.) * scale) as u32,
            ((f32::from(area.size.height) - 30.) * scale) as u32,
        )
        .to_image()
    };
    let before = crop(app.cx.capture_screenshot(app.window).unwrap());
    app.keys(&["G", "A"]);
    app.type_text(" edited at bottom");
    app.press("escape");
    app.ui(|_, _| {});
    let after = crop(app.cx.capture_screenshot(app.window).unwrap());
    assert!(
        before == after,
        "the first pane must still display its original viewport"
    );
    app.keys(&["s", "h", "i"]);
    app.type_text("top ");
    app.press("escape");
    let expected = format!(
        "top {}",
        source.trim_end_matches('\n').to_string() + " edited at bottom\n"
    );
    app.assert_sources(&[&expected, &expected]);
}

#[test]
fn language_indentation_visual_indent_and_empty_buffer_paste_are_visible_edits() {
    let project = Project::new(&[
        ("config.lua", "local one = 1\nlocal two = 2\n"),
        ("empty.lua", ""),
    ]);
    let mut app = Driver::open_plain_in(project.0.clone());
    app.open_file("config.lua", &project.0.join("config.lua"));
    app.keys(&["V", "j", ">"]);
    app.assert_sources(&["  local one = 1\n  local two = 2\n"]);
    app.keys(&["g", "g", "y", "y"]);
    app.open_file("empty.lua", &project.0.join("empty.lua"));
    app.press("p");
    app.assert_sources(&["  local one = 1\n"]);
    app.keys(&["<", "<"]);
    app.assert_sources(&["local one = 1\n"]);
    app.keys(&["o", "tab"]);
    app.type_text("-- next");
    app.press("escape");
    app.assert_sources(&["local one = 1\n  -- next\n"]);
}
