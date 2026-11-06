use super::*;
use alacritty_terminal::{grid::Dimensions, term::cell::Flags, vte::ansi};
use std::time::{Duration, Instant};

fn process(script: Option<&str>) -> TerminalProcess {
    let mut command = CommandBuilder::new("/bin/sh");
    if let Some(script) = script {
        command.args(["-c", script]);
    } else {
        command.arg("-i");
        command.env("PS1", "TEST> ");
        command.env("ENV", "/dev/null");
    }
    TerminalProcess::spawn(&std::env::temp_dir(), command).unwrap()
}
fn wait(process: &TerminalProcess, predicate: impl Fn(&screen::Screen) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if predicate(&process.screen.lock().unwrap()) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!(
        "terminal did not reach expected state: {}",
        process.screen.lock().unwrap().text(0)
    );
}
#[test]
fn real_pty_retains_shell_state_and_resizes() {
    let mut p = process(None);
    wait(&p, |s| s.text(0).contains("TEST>"));
    p.send(Command::Write(
        b"test -t 0 && test -t 1 && printf 'TTY_OK\\n'; cd /; printf 'DIR:%s\\n' \"$PWD\"\r"
            .to_vec(),
    ))
    .unwrap();
    wait(&p, |s| {
        s.text(0).contains("\nTTY_OK\n") && s.text(0).contains("DIR:/\n")
    });
    p.send(Command::Resize(Size {
        columns: 91,
        rows: 31,
        ..Size::default()
    }))
    .unwrap();
    p.send(Command::Write(
        b"stty size; printf 'TERM:%s\\n' \"$TERM\"\r".to_vec(),
    ))
    .unwrap();
    wait(&p, |s| {
        s.text(0).contains("31 91") && s.text(0).contains("TERM:xterm-256color")
    });
    p.send(Command::Write(vec![4])).unwrap();
    wait(&p, |s| s.exit.is_some());
    p.shutdown().unwrap().join().unwrap();
}
#[test]
fn ctrl_c_interrupts_job_but_not_shell() {
    let mut p = process(None);
    wait(&p, |s| s.text(0).contains("TEST>"));
    p.send(Command::Write(b"sleep 30\r".to_vec())).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    p.send(Command::Write(vec![3])).unwrap();
    p.send(Command::Write(b"printf 'SURVIVED\\n'\r".to_vec()))
        .unwrap();
    wait(&p, |s| s.text(0).contains("\nSURVIVED\n"));
    p.shutdown().unwrap().join().unwrap();
}
#[test]
fn shutdown_is_bounded_even_when_child_ignores_signals_and_stdin() {
    let mut p = process(Some(
        "trap '' HUP TERM; printf READY; while :; do sleep 30; done",
    ));
    wait(&p, |s| s.text(0).contains("READY"));
    p.send(Command::Write(vec![b'x'; 100_000])).unwrap();
    let start = Instant::now();
    p.shutdown().unwrap().join().unwrap();
    assert!(start.elapsed() < Duration::from_secs(2));
}
#[test]
fn dropping_a_session_terminates_and_reaps_its_shell() {
    let p = process(Some("printf 'PID:%s\\n' $$; exec sleep 30"));
    wait(&p, |s| s.text(0).starts_with("PID:"));
    let pid: i32 = p
        .screen
        .lock()
        .unwrap()
        .text(0)
        .lines()
        .next()
        .unwrap()
        .strip_prefix("PID:")
        .unwrap()
        .parse()
        .unwrap();
    assert!(pid > 1);
    drop(p);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if unsafe { libc::kill(pid, 0) } == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            break;
        }
        assert!(Instant::now() < deadline, "shell {pid} was not reaped");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn shell_failure_does_not_make_a_session() {
    let command = CommandBuilder::new("/definitely/missing/rockstar-shell");
    assert!(TerminalProcess::spawn(&std::env::temp_dir(), command).is_err());
}
#[test]
fn ansi_unicode_alternate_screen_and_history() {
    let (tx, _) = async_channel::bounded(256);
    let size = Size {
        columns: 12,
        rows: 3,
        ..Size::default()
    };
    let mut term = alacritty_terminal::Term::new(Default::default(), &size, screen::EventProxy(tx));
    let mut parser: ansi::Processor = ansi::Processor::new();
    for byte in "\x1b[31m日本語\x1b[0m".as_bytes() {
        parser.advance(&mut term, &[*byte]);
    }
    assert!(
        term.grid()[alacritty_terminal::index::Line(0)][alacritty_terminal::index::Column(0)]
            .flags
            .contains(Flags::WIDE_CHAR)
    );
    parser.advance(&mut term, b"\x1b[?1049hALT\x1b[?1049l");
    let mut screen = screen::Screen {
        term,
        size,
        exit: None,
    };
    assert!(screen.text(0).starts_with("日本語"));
    parser.advance(&mut screen.term, b"\r\n1\r\n2\r\n3\r\n4");
    assert!(screen.term.history_size() > 0);
    assert!(screen.text(0).contains('4'));
    assert!(screen.text(screen.term.history_size()).contains("日本語"));
}
