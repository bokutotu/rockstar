//! Separate the short-lived CLI from the GUI without releasing the instance
//! reservation. Pass the listener and a one-shot startup channel through exec.
use std::{
    env,
    ffi::OsStr,
    fs::{DirBuilder, File, OpenOptions},
    io::{self, Read, Write},
    net::Shutdown,
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd},
        unix::{
            fs::{DirBuilderExt as _, OpenOptionsExt as _},
            net::{UnixListener, UnixStream},
            process::CommandExt as _,
        },
    },
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail, ensure};

use crate::instance::{self, InstanceServer, OpenWorkspaceRequest};

pub(crate) const GUI_ARGUMENT: &str = "--internal-gui";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) enum LaunchMode {
    Cli,
    Gui,
}

pub(crate) enum StartupStatus {
    Ready,
    Failed(String),
}

pub(crate) fn run_cli(request: OpenWorkspaceRequest) -> Result<()> {
    let Some(server) = instance::start_or_forward(&request)? else {
        return Ok(());
    };
    match spawn_gui(&request, server)? {
        StartupStatus::Ready => Ok(()),
        StartupStatus::Failed(message) => bail!("{message}"),
    }
}

fn spawn_gui(request: &OpenWorkspaceRequest, server: InstanceServer) -> Result<StartupStatus> {
    let (log, log_path) = open_gui_log()?;
    let (mut startup, child_startup) = UnixStream::pair()?;
    startup.set_read_timeout(Some(STARTUP_TIMEOUT))?;

    // Keep inherited descriptors above stdio even when the CLI was invoked
    // with one of 0/1/2 closed. Command replaces those three in the child.
    let listener = duplicate_for_child(server.as_fd())?;
    let notification = duplicate_for_child(child_startup.as_fd())?;
    let listener_fd = listener.as_raw_fd();
    let notification_fd = notification.as_raw_fd();
    let mut command =
        Command::new(env::current_exe().context("Cannot locate the Rockstar executable")?);
    command
        .arg(GUI_ARGUMENT)
        .arg(listener_fd.to_string())
        .arg(notification_fd.to_string())
        .current_dir(&request.root)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    // SAFETY: The child-side hook only makes syscalls and constructs OS-code
    // errors. It does not allocate or acquire locks after fork. CLOEXEC is
    // cleared only in the child, so no other CLI subprocess can inherit these.
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            set_close_on_exec(listener_fd, false)?;
            set_close_on_exec(notification_fd, false)
        });
    }
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let mut child = command
        .spawn()
        .context("Cannot start the Rockstar GUI process")?;
    drop(command);
    // The child now owns the reservation. In particular, don't keep a copy of
    // its startup endpoint here: that would hide EOF if it crashes before Ready.
    drop((server, listener, notification, child_startup));

    let result = (|| {
        let (sender, receiver) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("rockstar-startup".into())
            .spawn(move || {
                let _ = sender.send(receive_startup_status(&mut startup));
            })
            .context("Cannot start the Rockstar startup response reader")?;
        // Bound the entire response, not each individual socket read.
        receiver
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .context("No startup response from Rockstar within 30 seconds")?
    })();
    if !matches!(&result, Ok(StartupStatus::Ready)) {
        // kill may race with an already exited child; wait still reaps it.
        let _ = child.kill();
        child
            .wait()
            .context("Cannot reap the failed Rockstar GUI process")?;
    }
    match result {
        Ok(StartupStatus::Failed(message)) => Ok(StartupStatus::Failed(format!(
            "{message}\nLog: {}",
            log_path.display()
        ))),
        result => {
            result.with_context(|| format!("Rockstar failed to start; log: {}", log_path.display()))
        }
    }
}

pub(crate) fn run_gui_process(listener_arg: &OsStr, startup_arg: &OsStr) -> Result<()> {
    let listener_fd = parse_inherited_fd(listener_arg)?;
    let startup_fd = parse_inherited_fd(startup_arg)?;
    ensure!(
        listener_fd != startup_fd,
        "Duplicate internal Rockstar descriptor"
    );
    let mut startup = UnixStream::from(take_inherited_fd(startup_fd)?);
    startup.set_write_timeout(Some(STARTUP_TIMEOUT))?;

    // GPUI also reports some initialization failures through panics. Preserve
    // their normal log output, but return the message to the waiting CLI too.
    let result = catch_unwind(AssertUnwindSafe(|| {
        let listener = UnixListener::from(take_inherited_fd(listener_fd)?);
        let request = OpenWorkspaceRequest::new(
            &env::current_dir().context("Cannot determine the GUI working directory")?,
        )?;
        crate::run_gui(
            request,
            InstanceServer::from_listener(listener),
            startup.try_clone()?,
        )
    }))
    .unwrap_or_else(|panic| {
        let message = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .unwrap_or("unknown panic");
        Err(anyhow::anyhow!("Rockstar GUI panicked: {message}"))
    });
    if let Err(error) = &result {
        // Once Ready was sent, the write half is shut down on all clones, so a
        // later runtime failure cannot be mistaken for a second startup result.
        let _ = send_startup_status(&mut startup, StartupStatus::Failed(format!("{error:#}")));
    }
    result
}

pub(crate) fn send_startup_status(startup: &mut UnixStream, status: StartupStatus) -> Result<()> {
    match status {
        StartupStatus::Ready => startup.write_all(&[0])?,
        StartupStatus::Failed(message) => {
            startup.write_all(&[1])?;
            let bytes = message.as_bytes();
            instance::write_message(
                startup,
                &bytes[..bytes.len().min(instance::MAX_MESSAGE_LEN)],
            )?;
        }
    }
    startup.shutdown(Shutdown::Write)?;
    Ok(())
}

fn receive_startup_status(startup: &mut UnixStream) -> Result<StartupStatus> {
    let mut status = [0];
    startup
        .read_exact(&mut status)
        .context("Rockstar exited or stopped responding before startup completed")?;
    match status[0] {
        0 => Ok(StartupStatus::Ready),
        1 => Ok(StartupStatus::Failed(
            String::from_utf8_lossy(&instance::read_message(startup)?).into_owned(),
        )),
        _ => bail!("Invalid startup response from Rockstar"),
    }
}

fn open_gui_log() -> Result<(File, PathBuf)> {
    let state_home = env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .context("Cannot locate Rockstar logs: neither XDG_STATE_HOME nor HOME is set")?;
    ensure!(
        state_home.is_absolute(),
        "Rockstar log directory must be absolute"
    );
    let directory = state_home.join("rockstar");
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)
        .with_context(|| {
            format!(
                "Cannot create Rockstar log directory: {}",
                directory.display()
            )
        })?;
    let path = directory.join("rockstar.log");
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("Cannot open Rockstar log: {}", path.display()))?;
    Ok((file, path))
}

fn duplicate_for_child(fd: BorrowedFd<'_>) -> io::Result<OwnedFd> {
    // SAFETY: fd is live for this call. F_DUPFD_CLOEXEC creates a fresh owned
    // descriptor, leaving the original and its descriptor flags unchanged.
    let duplicate = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
    if duplicate == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fcntl returned a new descriptor that has no other Rust owner.
    Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
}

fn parse_inherited_fd(argument: &OsStr) -> Result<RawFd> {
    let fd: RawFd = argument
        .to_str()
        .context("Invalid internal Rockstar descriptor")?
        .parse()
        .context("Invalid internal Rockstar descriptor")?;
    ensure!(fd >= 3, "An internal Rockstar descriptor cannot be stdio");
    Ok(fd)
}

fn take_inherited_fd(fd: RawFd) -> Result<OwnedFd> {
    // Restore CLOEXEC before GPUI starts threads or launches pi, LSPs, etc.
    // Otherwise those processes could keep the instance reservation alive.
    set_close_on_exec(fd, true).context("Cannot restore an inherited Rockstar descriptor")?;
    // SAFETY: These distinct, non-stdio descriptors are adopted at process
    // entry, before opening other descriptors. fcntl verified that fd is live;
    // its ownership was transferred by the launcher through exec.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn set_close_on_exec(fd: RawFd, enabled: bool) -> io::Result<()> {
    // SAFETY: fcntl validates the descriptor and neither call uses pointers.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    let flags = if enabled {
        flags | libc::FD_CLOEXEC
    } else {
        flags & !libc::FD_CLOEXEC
    };
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
