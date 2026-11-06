//! Retained PTY sessions and per-pane terminal views.
mod keys;
mod process;
mod screen;
mod view;

use alacritty_terminal::event::Event;
use gpui_kit::{
    AppContext as _, ClipboardItem, Context, Task, Window,
    component::{WindowExt as _, notification::Notification},
};
use portable_pty::CommandBuilder;
use process::Command;
pub(crate) use process::Process as TerminalProcess;
use screen::Size;
use std::path::Path;
pub(crate) use view::TerminalView;

pub(crate) struct TerminalSession {
    pub(crate) number: usize,
    pub(super) process: TerminalProcess,
    requested_size: Size,
    _events: Task<()>,
}

impl TerminalSession {
    pub(crate) fn spawn(root: &Path, command: CommandBuilder) -> anyhow::Result<TerminalProcess> {
        TerminalProcess::spawn(root, command)
    }

    pub(crate) fn new(
        number: usize,
        process: TerminalProcess,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let events = process.events.clone();
        cx.on_app_quit(|this, cx| {
            let worker = this.process.shutdown();
            cx.background_spawn(async move {
                if let Some(worker) = worker {
                    let _ = worker.join();
                }
            })
        })
        .detach();
        let task = cx.spawn_in(window, async move |this, cx| {
            while let Ok(first) = events.recv().await {
                let mut batch = vec![first];
                while batch.len() < 256 {
                    match events.try_recv() {
                        Ok(event) => batch.push(event),
                        Err(_) => break,
                    }
                }
                if this
                    .update_in(cx, |this, _, cx| {
                        for event in batch {
                            match event {
                                Event::PtyWrite(text) => {
                                    let _ = this.process.send(Command::Write(text.into_bytes()));
                                }
                                Event::TextAreaSizeRequest(format) => {
                                    let size = this.process.screen.lock().unwrap().size;
                                    let _ = this
                                        .process
                                        .send(Command::Write(format(size.window()).into_bytes()));
                                }
                                Event::ColorRequest(index, format) => {
                                    let rgb = {
                                        let screen = this.process.screen.lock().unwrap();
                                        screen.term.colors()[index]
                                            .unwrap_or_else(|| screen::indexed_color(index))
                                    };
                                    let _ =
                                        this.process.send(Command::Write(format(rgb).into_bytes()));
                                }
                                Event::ClipboardStore(_, text) => {
                                    cx.write_to_clipboard(ClipboardItem::new_string(text))
                                }
                                // Never expose the clipboard to terminal output (OSC 52 reads).
                                _ => {}
                            }
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            number,
            process,
            requested_size: Size::default(),
            _events: task,
        }
    }

    pub(crate) fn label(&self) -> String {
        format!(
            "Terminal {}{}",
            self.number,
            if self.process.screen.lock().unwrap().exit.is_some() {
                " (exited)"
            } else {
                ""
            }
        )
    }

    pub(super) fn write(&mut self, bytes: Vec<u8>, window: &mut Window, cx: &mut Context<Self>) {
        if bytes.is_empty() || self.process.screen.lock().unwrap().exit.is_some() {
            return;
        }
        let result = if bytes.len() > 1024 * 1024 {
            Err(anyhow::anyhow!("Terminal paste exceeds 1 MiB"))
        } else {
            self.process.send(Command::Write(bytes))
        };
        if let Err(error) = result {
            window.push_notification(Notification::error(error.to_string()), cx);
        }
    }

    fn resize(&mut self, size: Size) {
        if self.requested_size != size && self.process.send(Command::Resize(size)).is_ok() {
            self.requested_size = size;
        }
    }
}

pub(crate) fn shell_command() -> CommandBuilder {
    let shell = std::env::var_os("SHELL")
        .filter(|shell| !shell.is_empty())
        .unwrap_or_else(|| "/bin/sh".into());
    let mut command = CommandBuilder::new(shell);
    command.arg("-i");
    command
}

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct TestCommand(pub CommandBuilder);
#[cfg(test)]
impl gpui_kit::Global for TestCommand {}

/// Gate only process startup in UI race tests; the PTY itself remains real.
#[cfg(test)]
#[derive(Clone)]
pub(crate) struct TestStartupGate {
    pub entered: async_channel::Sender<()>,
    pub release: async_channel::Receiver<()>,
    pub finished: async_channel::Sender<()>,
}
#[cfg(test)]
impl gpui_kit::Global for TestStartupGate {}

#[cfg(test)]
mod tests;
