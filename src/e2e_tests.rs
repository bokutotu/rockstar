//! Drive the production dock window through public UI events. Only the agent is mocked.
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, MutexGuard},
    time::Duration,
};

use gpui_kit::{
    AnyWindowHandle, App, Bounds, HeadlessAppContext, Pixels, Rgba, Role, Window,
    component::ActiveTheme as _,
    test::{TestQueryExt as _, TestWindowExt as _},
};

use crate::{
    agent::{AgentBackend, PromptRequest},
    test_support::{RENDER_LOCK, headless_app, mock_agent, open_app_window_with_connector},
};

mod close;
mod input_shortcuts;

struct Driver {
    cx: HeadlessAppContext,
    window: AnyWindowHandle,
    backend: AgentBackend,
    new_backends: async_channel::Receiver<AgentBackend>,
    // Drop the App (and its renderer) before releasing the lock.
    _render_guard: MutexGuard<'static, ()>,
}

impl Driver {
    fn open() -> Self {
        Self::open_in(PathBuf::from(env!("CARGO_MANIFEST_DIR")))
    }

    fn open_in(workspace: PathBuf) -> Self {
        let render_guard = RENDER_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut cx = headless_app();
        let (agent, backend) = mock_agent();
        let (new_backends, receiver) = async_channel::unbounded();
        let connect = Arc::new(move |_: &std::path::Path| {
            let (agent, backend) = mock_agent();
            new_backends.try_send(backend).unwrap();
            Ok(agent)
        });
        let window =
            cx.update(|cx| open_app_window_with_connector(agent, workspace, connect, cx).unwrap());
        cx.update_window(window, |_, window, _| window.activate_window())
            .unwrap();
        Self {
            cx,
            window,
            backend,
            new_backends: receiver,
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

    fn assert_no_requests(&self) {
        assert!(self.backend.prompts.is_empty());
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

    fn assert_active_pane(&mut self, active: usize) -> Vec<Bounds<Pixels>> {
        let mut panes = Vec::new();
        let mut border = [0; 4];
        let mut scale = 1.0;
        self.ui(|window, cx| {
            panes = window
                .elements()
                .into_iter()
                .filter(|element| element.path().last() == Some(&"tab-panel".into()))
                .map(|element| element.bounds())
                .collect();
            let color = Rgba::from(cx.theme().border);
            border =
                [color.r, color.g, color.b, color.a].map(|channel| (channel * 255.0).round() as u8);
            scale = window.scale_factor();
        });
        assert!(active < panes.len());
        let image = self.cx.capture_screenshot(self.window).unwrap();
        for (index, bounds) in panes.iter().enumerate() {
            let x = (f32::from(bounds.center().x) * scale).floor() as u32;
            let y = (f32::from(bounds.center().y) * scale).floor() as u32;
            // Sample inside each edge, where a 2px outline (but not a 1px line)
            // is opaque, including when the split lands between device pixels.
            let left = ((f32::from(bounds.left()) + 1.0) * scale).floor() as u32;
            let right = ((f32::from(bounds.right()) - 1.0) * scale).ceil() as u32 - 1;
            let top = ((f32::from(bounds.top()) + 1.0) * scale).floor() as u32;
            let bottom = ((f32::from(bounds.bottom()) - 1.0) * scale).ceil() as u32 - 1;
            for (x, y) in [(left, y), (right, y), (x, top), (x, bottom)] {
                assert_eq!(
                    image.get_pixel(x, y).0 == border,
                    index == active,
                    "pane {index} border at ({x}, {y}); active pane is {active}"
                );
            }
        }
        panes
    }

    fn wait_for_chat(&mut self, number: usize) {
        for _ in 0..200 {
            let mut ready = false;
            self.ui(|window, _| {
                ready = !window
                    .find_all_by_label(&format!("Chat {number}"))
                    .is_empty();
            });
            if ready {
                return;
            }
            self.cx.advance_clock(Duration::from_millis(10));
        }
        panic!("chat {number} did not open");
    }
}

#[test]
fn insert_mode_focuses_input() {
    let mut app = Driver::open();
    app.press("i");
    app.ui(|window, _| {
        assert_eq!(window.find_by_label("Message to pi").focused(), Some(true));
    });
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
    app.click_input();
    app.type_text("First line");
    app.press("enter");
    app.type_text("Second line");
    app.assert_draft("First line\nSecond line");
    app.assert_no_requests();
}

#[test]
fn ctrl_enter_sends_once_and_clears_draft() {
    let mut app = Driver::open();
    app.click_input();
    app.type_text("Hello");
    app.press("ctrl-enter");
    assert!(matches!(
        app.backend.prompts.try_recv(),
        Ok(PromptRequest { text }) if text == "Hello"
    ));
    app.assert_no_requests();
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
    app.click_input();
    app.type_text("An unsent draft");
    app.press("escape");
    app.press(":");
    app.assert_popup(true);
    // The pointer event hits the overlay, not the message input behind it.
    app.click_input();
    app.assert_popup(false);
    app.assert_draft("An unsent draft");
    app.assert_no_requests();
}

#[test]
fn new_in_vertical_split_opens_an_independent_chat() {
    let mut app = Driver::open();
    app.press("i");
    app.type_text("Original chat draft");
    app.press("escape");

    app.press("s");
    app.press("v");
    let panes = app.assert_active_pane(1);
    app.press(":");
    app.type_text("new");
    app.press("enter");
    app.wait_for_chat(2);
    app.press("i");
    app.type_text("New chat draft");

    // The new chat in the right pane can be edited without changing the left.
    app.ui(|window, _| {
        let drafts: Vec<_> = window
            .find_all_by_label("Message to pi")
            .iter()
            .map(|input| input.value().unwrap_or_default().to_string())
            .collect();
        assert_eq!(drafts, ["Original chat draft", "New chat draft"]);
    });
    assert_eq!(app.assert_active_pane(1), panes);

    app.press("escape");
    app.press("s");
    app.press("h");
    assert_eq!(app.assert_active_pane(0), panes);
}

fn mention_workspace(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("rockstar-e2e-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("README.md"), "").unwrap();
    fs::write(root.join("src/main.rs"), "").unwrap();
    fs::write(root.join("src/mermaid.rs"), "").unwrap();
    root
}

impl Driver {
    /// Waits for the background file scan, then returns the offered paths.
    fn mention_options(&mut self) -> Option<Vec<String>> {
        for _ in 0..200 {
            let mut options = None;
            self.ui(|window, _| {
                options = window.try_find("file-mention-popup").map(|_| {
                    window
                        .within("file-mention-popup")
                        .find_all_by_role(Role::ListBoxOption)
                        .iter()
                        .filter_map(|option| option.label().map(str::to_string))
                        .collect::<Vec<_>>()
                });
            });
            match options {
                Some(options) if options.is_empty() => {
                    self.cx.advance_clock(std::time::Duration::from_millis(10));
                }
                options => return options,
            }
        }
        Some(Vec::new())
    }
}

#[test]
fn at_lists_files_and_tab_inserts_mention() {
    let root = mention_workspace("tab");
    let mut app = Driver::open_in(root.clone());
    app.click_input();
    app.type_text("see @main");
    assert_eq!(app.mention_options(), Some(vec!["src/main.rs".to_string()]));
    app.press("tab");
    app.assert_draft("see @src/main.rs ");
    assert_eq!(app.mention_options(), None);
    app.assert_no_requests();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn enter_completes_directories_then_files() {
    let root = mention_workspace("enter");
    let mut app = Driver::open_in(root.clone());
    app.click_input();
    app.type_text("@src");
    assert_eq!(
        app.mention_options().unwrap().first().map(String::as_str),
        Some("src/")
    );
    app.press("enter");
    app.assert_draft("@src/");
    app.type_text("m");
    assert_eq!(
        app.mention_options(),
        Some(vec![
            "src/main.rs".to_string(),
            "src/mermaid.rs".to_string()
        ])
    );
    app.press("down");
    app.press("enter");
    app.assert_draft("@src/mermaid.rs ");
    app.assert_no_requests();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn escape_dismisses_mentions_and_email_addresses_do_not_open_them() {
    let root = mention_workspace("escape");
    let mut app = Driver::open_in(root.clone());
    app.click_input();
    app.type_text("user@example");
    assert_eq!(app.mention_options(), None);
    app.type_text(" @");
    assert!(app.mention_options().is_some());
    app.press("escape");
    assert_eq!(app.mention_options(), None);
    app.assert_draft("user@example @");
    fs::remove_dir_all(root).unwrap();
}
