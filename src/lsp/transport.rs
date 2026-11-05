//! A bounded, non-blocking-for-GPUI stdio JSON-RPC client. No shell is involved.
use super::language::{self, Encoding, ServerKind};
use anyhow::{Context as _, Result, anyhow, bail};
use async_channel::{Receiver, Sender};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_BODY: usize = 64 * 1024 * 1024;
type Reply = Sender<std::result::Result<Value, String>>;

#[derive(Clone, Debug)]
pub(crate) struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
}
impl From<ServerKind> for CommandSpec {
    fn from(kind: ServerKind) -> Self {
        let (program, args) = kind.command();
        Self {
            program: program.into(),
            args: args.iter().map(|s| (*s).into()).collect(),
        }
    }
}
#[derive(Clone, Debug)]
pub(crate) enum ServerState {
    Starting,
    Ready {
        capabilities: Value,
        encoding: Encoding,
    },
    Failed(String),
    Stopped,
}
#[derive(Debug)]
pub(crate) enum Event {
    State(ServerKind),
    Notification(ServerKind, String, Value),
    Request(ServerKind, Value, String, Value),
}
struct Pending {
    reply: Reply,
    deadline: Instant,
}
enum Mail {
    Send(String, Value, Option<Reply>),
    Respond(Value, Value),
    Incoming(Value),
    End(String),
    Stop,
}

pub(crate) struct ServerSession {
    pub kind: ServerKind,
    state: Arc<Mutex<ServerState>>,
    mail: mpsc::Sender<Mail>,
    finished: Receiver<()>,
}
impl Drop for ServerSession {
    fn drop(&mut self) {
        let _ = self.mail.send(Mail::Stop);
    }
}
pub(crate) struct Response(Receiver<std::result::Result<Value, String>>);
impl Response {
    pub(crate) async fn recv(self) -> Result<Value> {
        self.0
            .recv()
            .await
            .map_err(|_| anyhow!("Language server disconnected"))?
            .map_err(anyhow::Error::msg)
    }
}
impl ServerSession {
    pub(crate) fn start(
        kind: ServerKind,
        root: PathBuf,
        command: CommandSpec,
        events: Sender<Event>,
    ) -> Arc<Self> {
        let (mail, rx) = mpsc::channel();
        let state = Arc::new(Mutex::new(ServerState::Starting));
        let (done, finished) = async_channel::bounded(1);
        let session = Arc::new(Self {
            kind,
            state: state.clone(),
            mail: mail.clone(),
            finished,
        });
        thread::spawn(move || {
            let result = run(kind, &root, &command, mail, rx, &state, &events);
            if let Err(error) = result {
                *state.lock().unwrap() = ServerState::Failed(format!(
                    "{} ({}): {error:#}",
                    command.program,
                    root.display()
                ));
            } else {
                *state.lock().unwrap() = ServerState::Stopped;
            }
            let _ = events.try_send(Event::State(kind));
            drop(done);
        });
        session
    }
    pub(crate) fn stop(&self) -> Receiver<()> {
        let _ = self.mail.send(Mail::Stop);
        self.finished.clone()
    }
    pub(crate) fn state(&self) -> ServerState {
        self.state.lock().unwrap().clone()
    }
    pub(crate) fn encoding(&self) -> Encoding {
        match self.state() {
            ServerState::Ready { encoding, .. } => encoding,
            _ => Encoding::Utf16,
        }
    }
    pub(crate) fn capabilities(&self) -> Value {
        match self.state() {
            ServerState::Ready { capabilities, .. } => capabilities,
            _ => Value::Null,
        }
    }
    pub(crate) fn supports(&self, name: &str) -> bool {
        let caps = self.capabilities();
        caps.get(name)
            .is_some_and(|v| !v.is_null() && v != &Value::Bool(false))
    }
    pub(crate) fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.mail
            .send(Mail::Send(method.into(), params, None))
            .map_err(|_| anyhow!("Language server disconnected"))
    }
    pub(crate) fn request(&self, method: &str, params: Value) -> Response {
        let (tx, rx) = async_channel::bounded(1);
        if let ServerState::Failed(error) = self.state() {
            let _ = tx.try_send(Err(error));
        } else if let Err(mpsc::SendError(Mail::Send(_, _, Some(reply)))) =
            self.mail.send(Mail::Send(method.into(), params, Some(tx)))
        {
            let _ = reply.try_send(Err("Language server disconnected".into()));
        }
        Response(rx)
    }
    pub(crate) fn respond(&self, id: Value, result: Value) {
        let _ = self.mail.send(Mail::Respond(id, result));
    }
}

pub(crate) fn read_message(reader: &mut impl BufRead) -> Result<Option<Value>> {
    let mut length = None;
    let mut total = 0;
    loop {
        let mut line = String::new();
        let read = reader.take(8193).read_line(&mut line)?;
        if read == 0 {
            if total == 0 {
                return Ok(None);
            }
            bail!("EOF inside LSP header");
        }
        total += read;
        if total > 8192 {
            bail!("LSP header exceeds 8 KiB");
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| anyhow!("Malformed LSP header"))?;
        if name.eq_ignore_ascii_case("content-length") {
            if length.is_some() {
                bail!("Duplicate Content-Length");
            }
            length = Some(value.trim().parse::<usize>()?);
        }
    }
    let length = length.ok_or_else(|| anyhow!("Missing Content-Length"))?;
    if length > MAX_BODY {
        bail!("LSP message exceeds 64 MiB");
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok(Some(serde_json::from_slice(&body)?))
}
pub(crate) fn write_message(writer: &mut impl Write, value: &Value) -> Result<()> {
    let body = serde_json::to_vec(value)?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}
fn configuration(kind: ServerKind) -> Value {
    if kind == ServerKind::TypeScript {
        json!({"typescript": {"referencesCodeLens": {"enabled": true, "showOnAllFunctions": true}, "implementationsCodeLens": {"enabled": true}}})
    } else if kind == ServerKind::Rust {
        json!({"rust-analyzer": {"lens": {"enable": true, "references": {"enable": true}}}})
    } else {
        json!({})
    }
}
fn initialization(kind: ServerKind, root: &Path) -> Result<Value> {
    let uri = language::uri(root)?;
    let options = if kind == ServerKind::TypeScript {
        json!({"hostInfo": "rockstar", "disableAutomaticTypingAcquisition": true, "preferences": {
            "includeInlayParameterNameHints": "all", "includeInlayParameterNameHintsWhenArgumentMatchesName": true,
            "includeInlayFunctionParameterTypeHints": true, "includeInlayVariableTypeHints": true,
            "includeInlayPropertyDeclarationTypeHints": true, "includeInlayFunctionLikeReturnTypeHints": true,
            "includeInlayEnumMemberValueHints": true
        }})
    } else if kind == ServerKind::Rust {
        configuration(kind)["rust-analyzer"].clone()
    } else {
        Value::Null
    };
    Ok(
        json!({"processId":std::process::id(),"clientInfo":{"name":"rockstar","version":env!("CARGO_PKG_VERSION")},"rootUri":uri,"rootPath":root,"workspaceFolders":[{"uri":uri,"name":root.file_name().unwrap_or_default().to_string_lossy()}],
        "capabilities":{
            "general":{"positionEncodings":["utf-16","utf-8","utf-32"]},"offsetEncoding":["utf-16","utf-8","utf-32"],
            "workspace":{"configuration":true,"workspaceFolders":true,"applyEdit":false,"semanticTokens":{"refreshSupport":true},"inlayHint":{"refreshSupport":true},"codeLens":{"refreshSupport":true}},
            "window":{"workDoneProgress":true},
            "textDocument":{
                "synchronization":{"didSave":true},
                "completion":{"completionItem":{"snippetSupport":false,"documentationFormat":["markdown","plaintext"]}},
                "hover":{"contentFormat":["markdown","plaintext"]},"definition":{"linkSupport":true},"callHierarchy":{},"documentHighlight":{},"codeLens":{},
                "inlayHint":{"resolveSupport":{"properties":["tooltip","label.tooltip","label.location","label.command","textEdits"]}},
                "publishDiagnostics":{"versionSupport":true,"relatedInformation":true,"tagSupport":{"valueSet":[1,2]}},"diagnostic":{},
                "semanticTokens":{"requests":{"range":true,"full":true},"tokenTypes":["namespace","type","class","enum","interface","struct","typeParameter","parameter","variable","property","enumMember","event","function","method","macro","keyword","modifier","comment","string","number","regexp","operator","decorator"],"tokenModifiers":["declaration","definition","readonly","static","deprecated","abstract","async","modification","documentation","defaultLibrary"],"formats":["relative"],"overlappingTokenSupport":false,"multilineTokenSupport":false}
            }
        },"initializationOptions":options}),
    )
}

struct Process {
    child: Child,
}
impl Drop for Process {
    fn drop(&mut self) {
        // Reclaim compiler children even if the language server has already exited.
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}
fn send_message(writer: &mpsc::SyncSender<Value>, value: &Value) -> Result<()> {
    writer
        .try_send(value.clone())
        .map_err(|error| anyhow!("Language server write queue: {error}"))
}
fn run(
    kind: ServerKind,
    root: &Path,
    spec: &CommandSpec,
    mail: mpsc::Sender<Mail>,
    rx: mpsc::Receiver<Mail>,
    state: &Mutex<ServerState>,
    events: &Sender<Event>,
) -> Result<()> {
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = command.spawn().with_context(|| {
        format!(
            "Cannot start language server. Provide {} in the project's Nix devShell/PATH",
            spec.program
        )
    })?;
    let mut process = Process { child };
    let mut stdin = process.child.stdin.take().unwrap();
    // A server that stops reading stdin must not block cancellation or shutdown.
    let (writer, output) = mpsc::sync_channel::<Value>(128);
    let errors = mail.clone();
    thread::spawn(move || {
        for value in output {
            if let Err(error) = write_message(&mut stdin, &value) {
                let _ = errors.send(Mail::End(error.to_string()));
                break;
            }
        }
    });
    let stdout = process.child.stdout.take().unwrap();
    let stderr = process.child.stderr.take().unwrap();
    let tail = Arc::new(Mutex::new(String::new()));
    let log = tail.clone();
    thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut bytes = [0u8; 1024];
        while let Ok(n) = reader.read(&mut bytes) {
            if n == 0 {
                break;
            }
            let mut log = log.lock().unwrap();
            log.push_str(&String::from_utf8_lossy(&bytes[..n]));
            if log.len() > 4096 {
                let offset = log.ceil_char_boundary(log.len() - 4096);
                log.drain(..offset);
            }
        }
    });
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            match read_message(&mut reader) {
                Ok(Some(value)) => {
                    if mail.send(Mail::Incoming(value)).is_err() {
                        break;
                    }
                }
                Ok(None) => {
                    let _ = mail.send(Mail::End("Language server closed stdout".into()));
                    break;
                }
                Err(error) => {
                    let _ = mail.send(Mail::End(error.to_string()));
                    break;
                }
            }
        }
    });
    send_message(
        &writer,
        &json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":initialization(kind, root)?}),
    )?;
    let started = Instant::now();
    let mut ready = false;
    let mut queue = VecDeque::new();
    let mut pending: HashMap<u64, Pending> = HashMap::new();
    let mut next = 1u64;
    let result = (|| -> Result<()> {
        loop {
            if !ready && started.elapsed() > Duration::from_secs(60) {
                bail!("initialize timed out");
            }
            let expired: Vec<_> = pending
                .iter()
                .filter(|(_, p)| p.reply.is_closed() || Instant::now() >= p.deadline)
                .map(|(&id, _)| id)
                .collect();
            for id in expired {
                if let Some(p) = pending.remove(&id) {
                    let _ = p.reply.try_send(Err("LSP request timed out".into()));
                }
                send_message(
                    &writer,
                    &json!({"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":id}}),
                )?;
            }
            let message = if ready && !queue.is_empty() {
                queue.pop_front().unwrap()
            } else {
                match rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(m) => m,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(_) => Mail::Stop,
                }
            };
            match message {
                Mail::Send(method, params, reply) => {
                    if !ready {
                        queue.push_back(Mail::Send(method, params, reply));
                        continue;
                    }
                    let mut value = json!({"jsonrpc":"2.0","method":method,"params":params});
                    if let Some(reply) = reply {
                        if reply.is_closed() {
                            continue;
                        }
                        let id = next;
                        next += 1;
                        value["id"] = json!(id);
                        pending.insert(
                            id,
                            Pending {
                                reply,
                                deadline: Instant::now() + REQUEST_TIMEOUT,
                            },
                        );
                    }
                    send_message(&writer, &value)?;
                }
                Mail::Respond(id, result) => {
                    send_message(&writer, &json!({"jsonrpc":"2.0","id":id,"result":result}))?
                }
                Mail::Incoming(value) => {
                    if let Some(method) = value.get("method").and_then(Value::as_str) {
                        let params = value.get("params").cloned().unwrap_or(Value::Null);
                        if let Some(id) = value.get("id") {
                            let response = match method {
                                "workspace/configuration" => Some(Value::Array(
                                    params["items"]
                                        .as_array()
                                        .map(|items| {
                                            items
                                                .iter()
                                                .map(|item| {
                                                    let settings = configuration(kind);
                                                    item["section"].as_str().map_or(
                                                        settings.clone(),
                                                        |section| {
                                                            section
                                                                .split('.')
                                                                .try_fold(
                                                                    &settings,
                                                                    |value, key| value.get(key),
                                                                )
                                                                .cloned()
                                                                .unwrap_or_else(|| json!({}))
                                                        },
                                                    )
                                                })
                                                .collect()
                                        })
                                        .unwrap_or_default(),
                                )),
                                "workspace/workspaceFolders" => {
                                    Some(initialization(kind, root)?["workspaceFolders"].clone())
                                }
                                "window/workDoneProgress/create" => Some(Value::Null),
                                "workspace/applyEdit" => Some(
                                    json!({"applied":false,"failureReason":"Unsolicited workspace edits are not supported"}),
                                ),
                                "workspace/semanticTokens/refresh"
                                | "workspace/inlayHint/refresh"
                                | "workspace/codeLens/refresh" => {
                                    let _ = events.try_send(Event::Notification(
                                        kind,
                                        method.into(),
                                        params.clone(),
                                    ));
                                    Some(Value::Null)
                                }
                                // We do not advertise dynamic registration. Acknowledge only static-equivalent features,
                                // never pretend a file-system watcher has been installed.
                                "client/registerCapability" | "client/unregisterCapability" => None,
                                "window/showDocument" => {
                                    let _ = events.try_send(Event::Request(
                                        kind,
                                        id.clone(),
                                        method.into(),
                                        params,
                                    ));
                                    continue;
                                }
                                _ => None,
                            };
                            let message = if let Some(result) = response {
                                json!({"jsonrpc":"2.0","id":id,"result":result})
                            } else {
                                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("Unsupported client method: {method}")}})
                            };
                            send_message(&writer, &message)?;
                        } else {
                            let _ =
                                events.try_send(Event::Notification(kind, method.into(), params));
                        }
                    } else if value.get("id") == Some(&json!(0)) && !ready {
                        if let Some(error) = value.get("error") {
                            bail!("initialize: {error}");
                        }
                        let result = &value["result"];
                        if !result["capabilities"].is_object() {
                            bail!("initialize returned no server capabilities");
                        }
                        let encoding = Encoding::from_initialize(result)?;
                        *state.lock().unwrap() = ServerState::Ready {
                            capabilities: result["capabilities"].clone(),
                            encoding,
                        };
                        send_message(
                            &writer,
                            &json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
                        )?;
                        if matches!(kind, ServerKind::TypeScript | ServerKind::Rust) {
                            send_message(
                                &writer,
                                &json!({"jsonrpc":"2.0","method":"workspace/didChangeConfiguration","params":{"settings": configuration(kind)}}),
                            )?;
                        }
                        ready = true;
                        let _ = events.try_send(Event::State(kind));
                    } else if let Some(id) = value.get("id").and_then(Value::as_u64)
                        && let Some(p) = pending.remove(&id)
                    {
                        let response = if let Some(error) = value.get("error") {
                            Err(error.to_string())
                        } else {
                            Ok(value.get("result").cloned().unwrap_or(Value::Null))
                        };
                        let _ = p.reply.try_send(response);
                    }
                }
                Mail::End(error) => bail!("{error}\n{}", tail.lock().unwrap()),
                Mail::Stop => {
                    if ready {
                        let _ = send_message(
                            &writer,
                            &json!({"jsonrpc":"2.0","id":-1,"method":"shutdown","params":null}),
                        );
                        let deadline = Instant::now() + Duration::from_millis(500);
                        while Instant::now() < deadline {
                            if let Ok(Mail::Incoming(value)) =
                                rx.recv_timeout(Duration::from_millis(20))
                                && value.get("id") == Some(&json!(-1))
                            {
                                break;
                            }
                        }
                        let _ = send_message(
                            &writer,
                            &json!({"jsonrpc":"2.0","method":"exit","params":null}),
                        );
                        let deadline = Instant::now() + Duration::from_millis(300);
                        while Instant::now() < deadline && process.child.try_wait()?.is_none() {
                            thread::sleep(Duration::from_millis(10));
                        }
                    }
                    return Ok(());
                }
            }
        }
    })();
    let error = result
        .as_ref()
        .err()
        .map_or_else(|| "Language server stopped".into(), |e| format!("{e:#}"));
    for (_, p) in pending {
        let _ = p.reply.try_send(Err(error.clone()));
    }
    for message in queue {
        if let Mail::Send(_, _, Some(reply)) = message {
            let _ = reply.try_send(Err(error.clone()));
        }
    }
    result
}

#[cfg(test)]
mod tests;
