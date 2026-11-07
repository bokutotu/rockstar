//! One Linux instance per OS user. The abstract Unix socket is both the
//! exclusive startup reservation and the request endpoint; the kernel releases
//! it on exit, including crashes. No lock files or stale socket files remain.
use std::{
    ffi::OsString,
    io::{self, Read, Write},
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd},
        unix::{
            ffi::{OsStrExt, OsStringExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail, ensure};
use async_channel::{Receiver, Sender};
use socket2::{Domain, SockAddr, Socket, Type};

const TIMEOUT: Duration = Duration::from_secs(30);
const RETRY_INTERVAL: Duration = Duration::from_millis(50);
const MAGIC: &[u8; 8] = b"ROCKSTR1";
pub(crate) const MAX_MESSAGE_LEN: usize = 64 * 1024;

pub(crate) struct OpenWorkspaceRequest {
    pub(crate) root: PathBuf,
}

impl OpenWorkspaceRequest {
    pub(crate) fn new(root: &Path) -> Result<Self> {
        let root = root
            .canonicalize()
            .with_context(|| format!("Cannot resolve directory: {}", root.display()))?;
        ensure!(root.is_dir(), "Not a directory: {}", root.display());
        ensure!(
            root.as_os_str().len() <= MAX_MESSAGE_LEN,
            "Directory path is too long"
        );
        Ok(Self { root })
    }
}

pub(crate) struct IncomingRequest {
    pub(crate) request: OpenWorkspaceRequest,
    pub(crate) reply: mpsc::Sender<Result<()>>,
}

pub(crate) struct InstanceServer {
    listener: UnixListener,
}

/// Some means this process owns the only instance. None means the existing
/// instance has applied the request. Errors must never fall back to a new UI.
pub(crate) fn start_or_forward(request: &OpenWorkspaceRequest) -> Result<Option<InstanceServer>> {
    // A leading NUL selects Linux's abstract namespace. Deliberately independent
    // of cwd, executable version, DISPLAY, and XDG_RUNTIME_DIR.
    let address = SockAddr::unix(format!("\0rockstar.instance.{}", unsafe {
        libc::geteuid()
    }))?;
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let socket = Socket::new(Domain::UNIX, Type::STREAM, None)?;
        match socket.bind(&address) {
            Ok(()) => {
                socket.listen(128)?;
                return Ok(Some(InstanceServer {
                    listener: socket.into(),
                }));
            }
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => {}
            Err(error) => return Err(error).context("Cannot reserve the Rockstar instance socket"),
        }

        // AF_UNIX nonblocking connect returns WouldBlock when the accept queue
        // is full. Retry with a fresh socket rather than blocking indefinitely.
        socket.set_nonblocking(true)?;
        match socket.connect(&address) {
            Ok(()) => {
                socket.set_nonblocking(false)?;
                let mut stream: UnixStream = socket.into();
                let remaining = deadline
                    .checked_duration_since(Instant::now())
                    .filter(|duration| !duration.is_zero())
                    .context("Timed out connecting to the existing Rockstar instance")?;
                configure_stream(&stream, remaining)?;
                forward_request(&mut stream, request)
                    .context("The existing Rockstar instance could not open the workspace")?;
                return Ok(None);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionRefused
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                ) =>
            {
                // The owner may be between bind and listen, or may have exited.
                // Binding again is the only way this process may become owner.
                ensure!(
                    Instant::now() < deadline,
                    "Timed out connecting to the existing Rockstar instance"
                );
                thread::sleep(RETRY_INTERVAL);
            }
            Err(error) => {
                return Err(error).context("Cannot contact the existing Rockstar instance");
            }
        }
    }
}

impl AsFd for InstanceServer {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.listener.as_fd()
    }
}

impl InstanceServer {
    pub(crate) fn from_listener(listener: UnixListener) -> Self {
        Self { listener }
    }

    /// Keep self alive until the GUI event loop exits, even if the worker fails.
    /// Blocking socket I/O and response waiting never run on the UI thread.
    pub(crate) fn requests(&self) -> Result<Receiver<IncomingRequest>> {
        let listener = self.listener.try_clone()?;
        let (sender, receiver) = async_channel::bounded(32);
        thread::Builder::new()
            .name("rockstar-instance".into())
            .spawn(move || {
                for connection in listener.incoming() {
                    if sender.is_closed() {
                        break;
                    }
                    match connection {
                        Ok(stream) => {
                            if let Err(error) = handle_connection(stream, &sender) {
                                eprintln!("Rockstar instance request: {error:#}");
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(error) => {
                            eprintln!("Rockstar instance socket: {error}");
                            thread::sleep(RETRY_INTERVAL);
                        }
                    }
                }
            })
            .context("Cannot start the Rockstar instance listener")?;
        Ok(receiver)
    }
}

fn configure_stream(stream: &UnixStream, timeout: Duration) -> Result<()> {
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;

    // Abstract sockets have no filesystem permissions. Authenticate both ends
    // before accepting a request or sending a directory to another process.
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: stream is a live socket and both output pointers refer to valid,
    // writable storage of the size supplied to getsockopt.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error()).context("Cannot identify Rockstar socket peer");
    }
    ensure!(
        length as usize == size_of::<libc::ucred>()
            && credentials.uid == unsafe { libc::geteuid() },
        "Rockstar socket belongs to a different OS user"
    );
    Ok(())
}

fn forward_request(stream: &mut UnixStream, request: &OpenWorkspaceRequest) -> Result<()> {
    stream.write_all(MAGIC)?;
    // Preserve non-UTF-8 filenames, spaces, and quotes without shell parsing.
    write_message(stream, request.root.as_os_str().as_bytes())?;
    let mut status = [0];
    stream
        .read_exact(&mut status)
        .context("No response from Rockstar (it may be busy or closing)")?;
    match status[0] {
        0 => Ok(()),
        1 => bail!("{}", String::from_utf8_lossy(&read_message(stream)?)),
        _ => bail!("Invalid response from Rockstar"),
    }
}

fn handle_connection(mut stream: UnixStream, sender: &Sender<IncomingRequest>) -> Result<()> {
    configure_stream(&stream, TIMEOUT)?;
    let result = (|| {
        let mut magic = [0; MAGIC.len()];
        stream.read_exact(&mut magic)?;
        ensure!(&magic == MAGIC, "Unsupported Rockstar request protocol");
        let root = PathBuf::from(OsString::from_vec(read_message(&mut stream)?));
        ensure!(root.is_absolute(), "Workspace directory must be absolute");
        let request = OpenWorkspaceRequest::new(&root)?;
        let (reply, response) = mpsc::channel();
        sender
            .try_send(IncomingRequest { request, reply })
            .map_err(|_| anyhow::anyhow!("Rockstar is busy or shutting down; try again"))?;
        response
            .recv_timeout(TIMEOUT)
            .context("No response from the Rockstar window")?
    })();
    match result {
        Ok(()) => stream.write_all(&[0])?,
        Err(error) => {
            stream.write_all(&[1])?;
            let message = format!("{error:#}");
            let bytes = message.as_bytes();
            write_message(&mut stream, &bytes[..bytes.len().min(MAX_MESSAGE_LEN)])?;
        }
    }
    Ok(())
}

pub(crate) fn write_message(stream: &mut UnixStream, bytes: &[u8]) -> Result<()> {
    ensure!(
        bytes.len() <= MAX_MESSAGE_LEN,
        "Rockstar message is too large"
    );
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(bytes)?;
    Ok(())
}

pub(crate) fn read_message(stream: &mut UnixStream) -> Result<Vec<u8>> {
    let mut length = [0; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    ensure!(length <= MAX_MESSAGE_LEN, "Rockstar message is too large");
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes)?;
    Ok(bytes)
}
