//! Nonblocking PTY transport. Only this worker reads output or waits for the child.
use super::screen::{EventProxy, Screen, Size};
use alacritty_terminal::{Term, event::Event, vte::ansi};
use anyhow::{Context as _, Result};
use portable_pty::{Child, CommandBuilder, MasterPty};
use std::{
    collections::VecDeque,
    io::{self, Read, Write},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

pub(super) enum Command {
    Write(Vec<u8>),
    Resize(Size),
}

struct ChildControl {
    child: Box<dyn Child + Send + Sync>,
    fd: i32,
}

impl ChildControl {
    fn terminate(&mut self) {
        if !matches!(self.child.try_wait(), Ok(None)) {
            return;
        }
        // A shell's foreground job has its own process group. Kill it as well,
        // but never signal the application's group or use PID zero.
        unsafe {
            let foreground = libc::tcgetpgrp(self.fd);
            let own_group = libc::getpgrp();
            if foreground > 0 && foreground != own_group {
                libc::kill(-foreground, libc::SIGKILL);
            }
            if let Some(pid) = self.child.process_id().filter(|pid| *pid > 0)
                && pid as i32 != own_group
            {
                libc::kill(-(pid as i32), libc::SIGKILL);
                libc::kill(pid as i32, libc::SIGKILL);
            }
        }
        // portable-pty's Child::kill includes a blocking grace period; do not
        // call it here because shutdown can be initiated by the UI thread.
    }
}

pub(crate) struct Process {
    pub(super) screen: Arc<Mutex<Screen>>,
    pub(super) events: async_channel::Receiver<Event>,
    commands: mpsc::SyncSender<Command>,
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<ChildControl>>>,
    worker: Option<JoinHandle<()>>,
}

impl Process {
    pub fn spawn(root: &Path, mut command: CommandBuilder) -> Result<Self> {
        anyhow::ensure!(
            root.is_dir(),
            "Terminal directory does not exist: {}",
            root.display()
        );
        command.cwd(root);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        let size = Size::default();
        let pair = portable_pty::native_pty_system().openpty(size.pty())?;
        let fd = pair
            .master
            .as_raw_fd()
            .context("PTY has no file descriptor")?;
        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        // Duplicated descriptors share file status flags, so neither reader nor
        // writer can block the worker (including when a child stops reading).
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
                return Err(io::Error::last_os_error().into());
            }
        }
        let child = pair
            .slave
            .spawn_command(command)
            .context("Cannot start terminal shell")?;
        drop(pair.slave);
        let child = Arc::new(Mutex::new(Some(ChildControl { child, fd })));
        let (events_tx, events) = async_channel::bounded(256);
        let proxy = EventProxy(events_tx.clone());
        let screen = Arc::new(Mutex::new(Screen {
            term: Term::new(Default::default(), &size, proxy),
            size,
            exit: None,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let (commands, receiver) = mpsc::sync_channel(128);
        let worker_screen = screen.clone();
        let worker_child = child.clone();
        let worker_stop = stop.clone();
        let worker = thread::Builder::new()
            .name("rockstar-terminal".into())
            .spawn(move || {
                let result = run(
                    &*pair.master,
                    reader,
                    writer,
                    &worker_child,
                    &worker_stop,
                    receiver,
                    &worker_screen,
                    &events_tx,
                );
                // Kill and reap on the worker, never wait for a child on the UI thread.
                let child = worker_child.lock().unwrap().take();
                if let Some(mut child) = child {
                    child.terminate();
                    let _ = child.child.wait();
                }
                if let Err(error) = result {
                    worker_screen.lock().unwrap().exit = Some(format!("Terminal error: {error:#}"));
                }
                let _ = events_tx.try_send(Event::Wakeup);
            });
        let worker = match worker {
            Ok(worker) => worker,
            Err(error) => {
                if let Some(mut child) = child.lock().unwrap().take() {
                    // The failed thread spawn already dropped the PTY master.
                    child.fd = -1;
                    child.terminate();
                    let _ = child.child.wait();
                }
                return Err(error.into());
            }
        };
        Ok(Self {
            screen,
            events,
            commands,
            stop,
            child,
            worker: Some(worker),
        })
    }

    pub(super) fn send(&self, command: Command) -> Result<()> {
        self.commands
            .try_send(command)
            .map_err(|error| anyhow::anyhow!("Terminal input unavailable: {error}"))?;
        if let Some(worker) = &self.worker {
            worker.thread().unpark();
        }
        Ok(())
    }

    pub fn shutdown(&mut self) -> Option<JoinHandle<()>> {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = &self.worker {
            worker.thread().unpark();
        }
        // Also signal synchronously: even if the application exits before the
        // worker is scheduled again, the shell/foreground job cannot outlive it.
        if let Some(child) = self.child.lock().unwrap().as_mut() {
            child.terminate();
        }
        self.worker.take()
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // Dropping a JoinHandle detaches, but the worker still reaps the child.
        self.shutdown();
    }
}

#[allow(clippy::too_many_arguments)]
fn run(
    master: &dyn MasterPty,
    mut reader: Box<dyn Read + Send>,
    mut writer: Box<dyn Write + Send>,
    child: &Mutex<Option<ChildControl>>,
    stop: &AtomicBool,
    commands: mpsc::Receiver<Command>,
    screen: &Mutex<Screen>,
    events: &async_channel::Sender<Event>,
) -> Result<()> {
    let mut parser: ansi::Processor = ansi::Processor::new();
    let mut pending = VecDeque::<u8>::new();
    let mut buffer = [0; 16384];
    loop {
        if stop.load(Ordering::Acquire) {
            break;
        }
        // Bound queued bytes as well as channel length when a child ignores stdin.
        while pending.len() < 1024 * 1024 {
            match commands.try_recv() {
                Ok(Command::Write(bytes)) => pending.extend(bytes),
                Ok(Command::Resize(size)) => {
                    master.resize(size.pty())?;
                    let mut screen = screen.lock().unwrap();
                    screen.term.resize(size);
                    screen.size = size;
                    let _ = events.try_send(Event::Wakeup);
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
            }
        }
        if !pending.is_empty() {
            match writer.write(pending.make_contiguous()) {
                Ok(n) => {
                    pending.drain(..n);
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error.into()),
            }
        }
        // A noisy process must not starve input, shutdown, or the UI's screen lock.
        let mut changed = false;
        for _ in 0..16 {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    parser.advance(&mut screen.lock().unwrap().term, &buffer[..n]);
                    changed = true;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock
                        || error.raw_os_error() == Some(libc::EIO) =>
                {
                    break;
                }
                Err(error) => return Err(error.into()),
            }
        }
        if changed {
            let _ = events.try_send(Event::Wakeup);
        }
        let status = child
            .lock()
            .unwrap()
            .as_mut()
            .and_then(|child| child.child.try_wait().transpose());
        if let Some(status) = status {
            let status = status?;
            // Read the final buffered output after wait reports exit as well.
            for _ in 0..16 {
                let Ok(n) = reader.read(&mut buffer) else {
                    break;
                };
                if n == 0 {
                    break;
                }
                parser.advance(&mut screen.lock().unwrap().term, &buffer[..n]);
            }
            screen.lock().unwrap().exit = Some(format!("Exited: {status}"));
            let _ = events.try_send(Event::Wakeup);
            break;
        }
        // Input wakes this wait immediately; do not add a polling delay to every key.
        thread::park_timeout(Duration::from_millis(8));
    }
    // Keep the master alive until ChildControl is removed: it stores its fd.
    let child = child.lock().unwrap().take();
    if let Some(mut child) = child {
        child.terminate();
        let _ = child.child.wait();
    }
    Ok(())
}
