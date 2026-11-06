//! Drive the real terminal UI and a real, isolated POSIX shell (no user rc files).
use super::*;
use crate::{mode::Mode, terminal::TestCommand};
use portable_pty::CommandBuilder;
use std::time::Instant;

impl Driver {
    fn open_terminal_app() -> Self {
        let mut app = Self::open_plain_in(std::env::temp_dir());
        app.cx.update(|cx| {
            let mut command = CommandBuilder::new("/bin/sh");
            command.args(["-i"]);
            command.env("PS1", "TERM_TEST> ");
            command.env("ENV", "/dev/null");
            cx.set_global(TestCommand(command));
        });
        app
    }

    fn terminal_texts(&mut self) -> Vec<String> {
        let mut texts = Vec::new();
        self.ui(|window, _| {
            texts = window
                .find_all("terminal-screen")
                .iter()
                .map(|node| node.value().unwrap_or_default().to_owned())
                .collect();
        });
        texts
    }

    fn wait_terminal(&mut self, predicate: impl Fn(&[String]) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let texts = self.terminal_texts();
            if predicate(&texts) {
                return;
            }
            assert!(Instant::now() < deadline, "terminal UI timeout: {texts:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn terminal_command(&mut self, text: &str) {
        self.type_text(text);
        self.press("enter");
    }

    fn select_terminal_item(&mut self, label: &str) {
        self.ui(|window, cx| {
            let node = window.within("open-items").find_by_label(label);
            window
                .within("open-items")
                .click(node.path().last().unwrap().clone(), cx);
        });
    }
}

#[test]
fn term_starts_in_insert_and_escape_i_switch_without_sending_escape() {
    let mut app = Driver::open_terminal_app();
    app.command("term");
    app.wait_terminal(|texts| texts.len() == 1 && texts[0].contains("TERM_TEST>"));
    app.cx
        .update(|cx| assert_eq!(*cx.global::<Mode>(), Mode::Insert));
    app.terminal_command("printf '\\110\\145\\154\\154\\157\\n'");
    app.wait_terminal(|texts| texts[0].contains("\nHello\n"));
    app.type_text("ss");
    app.ui(|window, _| assert_eq!(window.find_all("pane").len(), 1));
    app.press("ctrl-c");
    app.terminal_command("stty -echo -icanon min 1; printf '\\122\\105\\101\\104\\131\\n'; od -An -tu1 -N2; stty sane");
    app.wait_terminal(|texts| texts[0].contains("\nREADY\n"));
    app.type_text("A");
    app.press("escape");
    app.cx
        .update(|cx| assert_eq!(*cx.global::<Mode>(), Mode::Normal));
    app.press("i");
    app.type_text("B");
    app.wait_terminal(|texts| texts[0].contains("65  66"));
    app.press("escape");
    app.command("term");
    app.cx
        .update(|cx| assert_eq!(*cx.global::<Mode>(), Mode::Insert));
    app.ui(|window, _| {
        assert_eq!(
            window
                .within("open-items")
                .find_all_by_role(Role::Tab)
                .len(),
            1
        )
    });
}

#[test]
fn terminal_sessions_survive_chat_switch_and_pane_close() {
    let mut app = Driver::open_terminal_app();
    app.command("term");
    app.wait_terminal(|texts| texts.len() == 1 && texts[0].contains("TERM_TEST>"));
    app.terminal_command("cd /; retained=YES");
    app.press("escape");
    app.command("chat");
    app.wait_for_chat(1);
    app.press("ctrl-n");
    app.wait_terminal(|texts| texts.len() == 1);
    app.press("i");
    app.terminal_command("printf '\\123\\124\\101\\124\\105:%s:%s\\n' \"$retained\" \"$PWD\"");
    app.wait_terminal(|texts| texts[0].contains("\nSTATE:YES:/\n"));
    app.press("escape");
    app.press("s");
    app.press("v");
    app.command("term");
    app.wait_terminal(|texts| texts.len() == 2 && texts[1].contains("TERM_TEST>"));
    app.terminal_command("printf '\\111\\116\\104\\105\\120:%s\\n' \"${retained-unset}\"");
    app.wait_terminal(|texts| texts.iter().any(|text| text.contains("\nINDEP:unset\n")));
    app.press("escape");
    app.command("q");
    app.select_terminal_item("Terminal 2");
    app.wait_terminal(|texts| texts.len() == 1 && texts[0].contains("\nINDEP:unset\n"));
    app.ui(|window, _| {
        assert_eq!(
            window
                .within("open-items")
                .find_all_by_role(Role::Tab)
                .len(),
            3
        )
    });
}

#[test]
fn same_terminal_in_two_panes_shares_output_not_processes() {
    let mut app = Driver::open_terminal_app();
    app.command("term");
    app.wait_terminal(|texts| texts.len() == 1 && texts[0].contains("TERM_TEST>"));
    app.press("escape");
    app.press("s");
    app.press("s");
    app.press("ctrl-n");
    app.wait_terminal(|texts| texts.len() == 2);
    app.press("i");
    app.terminal_command("printf '\\123\\110\\101\\122\\105\\104\\n'");
    app.wait_terminal(|texts| {
        texts.len() == 2 && texts.iter().all(|text| text.contains("\nSHARED\n"))
    });
    app.ui(|window, _| {
        assert_eq!(
            window
                .within("open-items")
                .find_all_by_role(Role::Tab)
                .len(),
            1
        )
    });
}

#[test]
fn terminal_committed_unicode_and_clipboard_paste_reach_shell() {
    let mut app = Driver::open_terminal_app();
    app.command("term");
    app.wait_terminal(|texts| texts.len() == 1 && texts[0].contains("TERM_TEST>"));
    app.cx.update(|cx| {
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
            "printf '日本語\\n'".into(),
        ))
    });
    app.press("ctrl-shift-v");
    app.press("enter");
    app.wait_terminal(|texts| texts[0].contains("\n日本語\n"));
    app.terminal_command("exit");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut exited = false;
        app.ui(|window, _| exited = window.find("terminal").label() == Some("Terminal 1 (exited)"));
        if exited {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(app.terminal_texts()[0].contains("日本語"));
}

#[test]
fn late_terminal_start_does_not_overwrite_a_selected_chat() {
    let mut app = Driver::open_terminal_app();
    let (entered_tx, entered) = async_channel::bounded(1);
    let (release, release_rx) = async_channel::bounded(1);
    let (finished_tx, finished) = async_channel::bounded(1);
    app.cx.update(|cx| {
        cx.set_global(crate::terminal::TestStartupGate {
            entered: entered_tx,
            release: release_rx,
            finished: finished_tx,
        })
    });
    app.command("term");
    let deadline = Instant::now() + Duration::from_secs(5);
    while entered.try_recv().is_err() {
        app.ui(|_, _| {});
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    app.command("chat");
    app.wait_for_chat(1);
    release.send_blocking(()).unwrap();
    while finished.try_recv().is_err() {
        app.ui(|_, _| {});
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    for _ in 0..10 {
        app.ui(|_, _| {});
        std::thread::sleep(Duration::from_millis(5));
    }
    app.ui(|window, _| {
        assert!(window.find("chat").visible());
        assert!(window.find_all("terminal").is_empty());
        assert_eq!(
            window
                .within("open-items")
                .find_all_by_role(Role::Tab)
                .len(),
            1
        );
    });
}

fn capture_terminal() -> image::RgbaImage {
    let mut app = Driver::open_terminal_app();
    app.command("term");
    app.wait_terminal(|texts| texts.len() == 1 && texts[0].contains("TERM_TEST>"));
    app.terminal_command("PS1=; printf '\\033[2J\\033[HRockstar terminal\\n\\033[31mred  \\033[32mgreen  \\033[34mblue\\033[0m\\n\\033[1mbold\\033[0m  \\033[4munderlined\\033[0m\\n日本語・全角文字  abc\\n\\033[30;43m background \\033[0m\\nDone.\\n'");
    app.wait_terminal(|texts| {
        texts[0].starts_with("Rockstar terminal\n") && texts[0].contains("\nDone.\n")
    });
    app.ui(|window, cx| window.blur(cx));
    app.ui(|_, _| {});
    app.cx.capture_screenshot(app.window).unwrap()
}

#[test]
fn terminal_renders_ansi_styles_and_wide_characters() {
    let expected = image::open(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/visual/terminal.png"
    ))
    .unwrap()
    .into_rgba8();
    let actual = capture_terminal();
    if actual != expected {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/visual-diffs/terminal");
        fs::create_dir_all(&path).unwrap();
        actual.save(path.join("actual.png")).unwrap();
        panic!("terminal screenshot differs; review target/visual-diffs/terminal");
    }
}

#[test]
#[ignore = "explicitly update and review the terminal reference image"]
fn update_terminal_snapshot() {
    capture_terminal()
        .save(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/visual/terminal.png"
        ))
        .unwrap();
}

#[test]
fn missing_shell_keeps_original_pane() {
    let mut app = Driver::open_terminal_app();
    app.cx
        .update(|cx| cx.set_global(TestCommand(CommandBuilder::new("/missing/rockstar-shell"))));
    app.command("term");
    for _ in 0..25 {
        app.ui(|_, _| {});
        std::thread::sleep(Duration::from_millis(10));
    }
    app.ui(|window, _| {
        assert!(window.find("plain").visible());
        assert!(window.find_all("terminal").is_empty());
        assert!(
            window
                .within("open-items")
                .find_all_by_role(Role::Tab)
                .is_empty()
        );
    });
}
