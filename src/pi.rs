use std::{
    collections::VecDeque,
    io::{self, BufRead, BufReader, Write},
    os::unix::process::CommandExt,
    path::Path,
    process::{self, Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

use anyhow::{Context as _, anyhow, bail};
use async_channel::{Receiver, RecvError};
use futures_lite::future::{block_on, or};
use serde::{Deserialize, Serialize};

use crate::agent::{
    self, AgentBackend, AgentError, AgentEvent, ChatStart, Message, OpenedChat, PromptRequest,
};

mod sessions;
use sessions::SessionEntries;
pub(crate) use sessions::list_sessions;

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum PiOutgoingRecord {
    Prompt { id: String, message: String },
    GetState { id: String },
    GetEntries { id: String },
    ExtensionUiResponse { id: String, cancelled: bool },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum PiIncomingRecord {
    Response(PiResponse),
    AgentStart,
    AgentSettled {
        aborted: bool,
    },
    MessageStart {
        message: PiMessage,
    },
    MessageUpdate {
        #[serde(rename = "assistantMessageEvent")]
        event: AssistantMessageEvent,
    },
    MessageEnd {
        message: PiMessage,
    },
    ExtensionUiRequest(ExtensionUiRequest),
    ExtensionError {
        error: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
enum PiResponse {
    Prompt(CommandResponse<PromptResponseData>),
    GetState(CommandResponse<SessionState>),
    GetEntries(CommandResponse<SessionEntries>),
    #[serde(other)]
    Other,
}

impl PiResponse {
    fn request_id(&self) -> Option<&str> {
        match self {
            Self::Prompt(response) => response.id.as_deref(),
            Self::GetState(response) => response.id.as_deref(),
            Self::GetEntries(response) => response.id.as_deref(),
            Self::Other => None,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionState {
    session_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(
    try_from = "WireCommandResponse<T>",
    bound(deserialize = "T: Deserialize<'de>")
)]
struct CommandResponse<T> {
    id: Option<String>,
    result: Result<T, String>,
}

#[derive(Debug, Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
struct WireCommandResponse<T> {
    id: Option<String>,
    success: bool,
    data: Option<T>,
    error: Option<String>,
}

impl<T> TryFrom<WireCommandResponse<T>> for CommandResponse<T> {
    type Error = &'static str;

    fn try_from(response: WireCommandResponse<T>) -> Result<Self, Self::Error> {
        let result = if response.success {
            Ok(response
                .data
                .ok_or("pi returned a successful response without data")?)
        } else {
            Err(response
                .error
                .ok_or("pi returned a failed response without an error")?)
        };
        Ok(Self {
            id: response.id,
            result,
        })
    }
}

#[derive(Debug, Deserialize)]
struct PromptResponseData {
    disposition: PromptDisposition,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PromptDisposition {
    Handled,
    Started,
    Queued,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
enum PiMessage {
    User {
        content: MessageContent,
    },
    Assistant {
        content: Vec<ContentBlock>,
        #[serde(rename = "stopReason")]
        stop_reason: StopReason,
        #[serde(rename = "errorMessage")]
        error_message: Option<String>,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlock {
    Text {
        text: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
enum StopReason {
    Pending,
    Stop,
    Length,
    ToolUse,
    Error,
    Aborted,
    Deferred,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AssistantMessageEvent {
    TextDelta {
        delta: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
enum ExtensionUiRequest {
    Select {
        id: String,
    },
    Confirm {
        id: String,
    },
    Input {
        id: String,
    },
    Editor {
        id: String,
    },
    Notify {
        message: String,
    },
    #[serde(other)]
    Other,
}

struct PiCodingAgent {
    child: Child,
    input: ChildStdin,
    records: Receiver<Result<PiIncomingRecord, AgentError>>,
    next_request_id: u64,
}

impl Drop for PiCodingAgent {
    fn drop(&mut self) {
        // Also clean up failed startup/handshakes, not just established chats.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

enum ActiveRequest {
    Prompt {
        id: String,
        accepted: bool,
        settled: bool,
        failure: Option<AgentError>,
    },
}

enum Incoming {
    Prompt(Result<PromptRequest, RecvError>),
    Record(Result<Result<PiIncomingRecord, AgentError>, RecvError>),
    ResultsClosed,
}

pub(crate) fn connect(cwd: &Path, start: ChatStart) -> Result<OpenedChat, AgentError> {
    let parent_pid = process::id() as libc::pid_t;
    let mut command = Command::new("pi");
    command
        .args(["--mode", "rpc"])
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let expected_session = match start {
        ChatStart::New => None,
        ChatStart::Resume(path) => {
            let path = cwd
                .join(path)
                .canonicalize()
                .context("The selected session is no longer available")?;
            let id = sessions::validate_session(&path, cwd)?;
            command.arg("--session").arg(path);
            Some(id)
        }
    };
    // SAFETY: The child-side hook only makes syscalls and constructs OS-code
    // errors; it does not allocate or acquire locks after fork.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(
                libc::PR_SET_PDEATHSIG,
                libc::SIGKILL as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
            ) == -1
            {
                return Err(io::Error::last_os_error());
            }
            // Do not start pi if Rockstar exited before the signal was armed.
            if libc::getppid() != parent_pid {
                return Err(io::Error::from_raw_os_error(libc::ESRCH));
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .context("Could not start pi. Make sure pi is installed and available on PATH.")?;
    let input = child.stdin.take().expect("piped pi stdin");
    let output = child.stdout.take().expect("piped pi stdout");
    let (record_sender, records) = async_channel::unbounded();
    let mut pi = PiCodingAgent {
        child,
        input,
        records,
        next_request_id: 0,
    };
    let _ = thread::Builder::new()
        .name("pi-stdout".into())
        .spawn(move || {
            let result = (|| -> Result<(), AgentError> {
                for line in BufReader::new(output).lines() {
                    let record = serde_json::from_str::<PiIncomingRecord>(&line?)
                        .context("pi returned an invalid JSONL record")?;
                    if record_sender.send_blocking(Ok(record)).is_err() {
                        return Ok(());
                    }
                }
                bail!("pi closed its output")
            })();
            if let Err(error) = result {
                let _ = record_sender.send_blocking(Err(error));
            }
        })?;

    let (agent, backend) = agent::channel();
    let (ready, initialized) = mpsc::channel();
    thread::Builder::new()
        .name("pi-rpc".into())
        .spawn(move || {
            let history = match expected_session {
                Some(id) => block_on(pi.load_history(&id, &backend)),
                None => Ok(Vec::new()),
            };
            match history {
                Ok(messages) => {
                    if ready.send(Ok(messages)).is_ok() {
                        pi.run(backend);
                    }
                }
                Err(error) => {
                    let _ = ready.send(Err(error));
                }
            }
        })?;
    // Dropping `agent` on timeout cancels the handshake and its child process.
    let messages = initialized
        .recv_timeout(Duration::from_secs(30))
        .context("Timed out or disconnected while opening the pi session")??;
    Ok(OpenedChat { agent, messages })
}

impl PiCodingAgent {
    async fn load_history(
        &mut self,
        expected_id: &str,
        backend: &AgentBackend,
    ) -> Result<Vec<Message>, AgentError> {
        let id = self.next_id();
        self.write_record(&PiOutgoingRecord::GetState { id: id.clone() })?;
        let PiResponse::GetState(response) = self.read_response(&id, backend).await? else {
            bail!("Unexpected pi response while opening the session");
        };
        let state = response.result.map_err(|error| anyhow!(error))?;
        if state.session_id != expected_id {
            bail!("pi opened a different session; the original chat has been kept");
        }
        let id = self.next_id();
        self.write_record(&PiOutgoingRecord::GetEntries { id: id.clone() })?;
        let PiResponse::GetEntries(response) = self.read_response(&id, backend).await? else {
            bail!("Unexpected pi response while loading session history");
        };
        response
            .result
            .map_err(|error| anyhow!(error))?
            .into_messages()
    }

    async fn read_response(
        &mut self,
        id: &str,
        backend: &AgentBackend,
    ) -> Result<PiResponse, AgentError> {
        loop {
            let record = or(
                async {
                    self.records
                        .recv()
                        .await
                        .context("pi disconnected during startup")?
                },
                async {
                    backend.events.closed().await;
                    Err(anyhow!("Opening the chat was cancelled"))
                },
            )
            .await?;
            self.handle_extension(&record)?;
            // The history snapshot includes finalized startup messages. Do not
            // replay their events as well, or the restored chat would duplicate them.
            if let PiIncomingRecord::Response(response) = record
                && response.request_id() == Some(id)
            {
                return Ok(response);
            }
        }
    }

    fn run(mut self, backend: AgentBackend) {
        let result = block_on(self.process_requests(&backend));
        if let Err(error) = result {
            // A failed session must not terminate the other open chats.
            let _ = backend.events.try_send(AgentEvent::Error(error));
        }
    }

    async fn process_requests(&mut self, backend: &AgentBackend) -> Result<(), AgentError> {
        let mut queued = VecDeque::new();
        let mut active: Option<ActiveRequest> = None;
        loop {
            if backend.events.is_closed() {
                return Ok(());
            }
            if active.is_none()
                && let Some((record, request)) = queued.pop_front()
            {
                self.write_record(&record)?;
                active = Some(request);
            }
            let incoming = or(
                async {
                    backend.events.closed().await;
                    Incoming::ResultsClosed
                },
                or(
                    async { Incoming::Record(self.records.recv().await) },
                    async { Incoming::Prompt(backend.prompts.recv().await) },
                ),
            )
            .await;
            let record = match incoming {
                Incoming::Prompt(Ok(request)) => {
                    queued.push_back(self.prepare_prompt(request));
                    continue;
                }
                Incoming::Prompt(Err(_)) => return Ok(()),
                Incoming::Record(result) => result.context("pi disconnected")??,
                Incoming::ResultsClosed => return Ok(()),
            };
            self.handle_extension(&record)?;
            if let Some(event) = decode_event(&record)
                && backend.events.try_send(event).is_err()
            {
                return Ok(());
            }
            if let Some(event) = active
                .as_mut()
                .and_then(|request| request.handle_record(&record))
            {
                active = None;
                if backend.events.try_send(event).is_err() {
                    return Ok(());
                }
            }
        }
    }

    fn next_id(&mut self) -> String {
        self.next_request_id += 1;
        self.next_request_id.to_string()
    }

    fn prepare_prompt(&mut self, request: PromptRequest) -> (PiOutgoingRecord, ActiveRequest) {
        let id = self.next_id();
        (
            PiOutgoingRecord::Prompt {
                id: id.clone(),
                message: request.text,
            },
            ActiveRequest::Prompt {
                id,
                accepted: false,
                settled: false,
                failure: None,
            },
        )
    }

    fn write_record(&mut self, record: &PiOutgoingRecord) -> Result<(), AgentError> {
        let input = &mut self.input;
        serde_json::to_writer(&mut *input, record)?;
        input.write_all(b"\n")?;
        input.flush()?;
        Ok(())
    }

    fn handle_extension(&mut self, record: &PiIncomingRecord) -> Result<(), AgentError> {
        match record {
            PiIncomingRecord::ExtensionUiRequest(
                ExtensionUiRequest::Select { id }
                | ExtensionUiRequest::Confirm { id }
                | ExtensionUiRequest::Input { id }
                | ExtensionUiRequest::Editor { id },
            ) => {
                self.write_record(&PiOutgoingRecord::ExtensionUiResponse {
                    id: id.clone(),
                    cancelled: true,
                })?;
                eprintln!(
                    "rockstar: cancelled an extension dialog; dialog UI is not available yet"
                );
            }
            PiIncomingRecord::ExtensionUiRequest(ExtensionUiRequest::Notify { message }) => {
                eprintln!("pi: {message}");
            }
            PiIncomingRecord::ExtensionError { error } => eprintln!("pi extension: {error}"),
            _ => {}
        }
        Ok(())
    }
}

impl ActiveRequest {
    fn handle_record(&mut self, record: &PiIncomingRecord) -> Option<AgentEvent> {
        match self {
            Self::Prompt {
                id,
                accepted,
                settled,
                failure,
            } => {
                match record {
                    PiIncomingRecord::Response(PiResponse::Prompt(response))
                        if response.id.as_deref() == Some(id.as_str()) =>
                    {
                        match &response.result {
                            Ok(PromptResponseData {
                                disposition: PromptDisposition::Handled,
                            }) => return Some(AgentEvent::PromptFinished),
                            Ok(PromptResponseData {
                                disposition: PromptDisposition::Started | PromptDisposition::Queued,
                            }) => *accepted = true,
                            Err(error) => {
                                return Some(AgentEvent::Error(anyhow!("{error}")));
                            }
                        }
                    }
                    PiIncomingRecord::AgentStart => *settled = false,
                    PiIncomingRecord::MessageEnd {
                        message:
                            PiMessage::Assistant {
                                stop_reason,
                                error_message,
                                ..
                            },
                    } => {
                        *failure = match stop_reason {
                            StopReason::Error | StopReason::Aborted => Some(anyhow!(
                                "{}",
                                error_message
                                    .as_deref()
                                    .unwrap_or("pi could not complete the response")
                            )),
                            _ => None,
                        };
                    }
                    PiIncomingRecord::AgentSettled { aborted } => {
                        *settled = true;
                        if *aborted {
                            *failure = Some(anyhow!("The pi request was cancelled."));
                        }
                    }
                    _ => {}
                }
                (*accepted && *settled).then(|| {
                    failure
                        .take()
                        .map_or(AgentEvent::PromptFinished, AgentEvent::Error)
                })
            }
        }
    }
}

fn decode_event(record: &PiIncomingRecord) -> Option<AgentEvent> {
    match record {
        PiIncomingRecord::MessageStart {
            message: PiMessage::Assistant { .. },
        } => Some(AgentEvent::AssistantStarted),
        PiIncomingRecord::MessageUpdate {
            event: AssistantMessageEvent::TextDelta { delta },
        } => (!delta.is_empty()).then(|| AgentEvent::AssistantTextDelta(delta.clone())),
        PiIncomingRecord::MessageEnd {
            message: PiMessage::User { content },
        } => {
            let text = content.text();
            (!text.is_empty()).then_some(AgentEvent::UserMessage(text))
        }
        PiIncomingRecord::MessageEnd {
            message: PiMessage::Assistant { content, .. },
        } => Some(AgentEvent::AssistantFinished(message_text(content))),
        _ => None,
    }
}

impl MessageContent {
    fn text(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Blocks(blocks) => message_text(blocks),
        }
    }
}

impl PiMessage {
    fn display_message(&self) -> Option<Message> {
        match self {
            Self::User { content } => {
                let text = content.text();
                (!text.is_empty()).then_some(Message::User(text))
            }
            Self::Assistant { content, .. } => {
                let text = message_text(content);
                (!text.is_empty()).then_some(Message::Assistant(text))
            }
            Self::Other => None,
        }
    }
}

fn message_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            ContentBlock::Other => None,
        })
        .collect()
}
