use std::{
    collections::VecDeque,
    io::{self, BufRead, BufReader, Write},
    os::unix::process::CommandExt,
    path::Path,
    process::{self, ChildStdin, Command, Stdio},
    thread,
};

use anyhow::{Context as _, anyhow, bail};
use async_channel::{Receiver, RecvError, Sender};
use futures_lite::future::{block_on, or};
use serde::{Deserialize, Serialize};

use crate::agent::{AgentConnection, AgentError, AgentEvent, AgentHandle, AgentRequest};

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum PiOutgoingRecord {
    Prompt { id: String, message: String },
    NewSession { id: String },
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
    NewSession(CommandResponse<NewSessionResponseData>),
    #[serde(other)]
    Other,
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
struct NewSessionResponseData {
    cancelled: bool,
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
    input: ChildStdin,
    records: Receiver<Result<PiIncomingRecord, AgentError>>,
    next_request_id: u64,
}

enum ActiveRequest {
    Prompt {
        id: String,
        accepted: bool,
        settled: bool,
        failure: Option<AgentError>,
    },
    NewSession {
        id: String,
    },
}

enum Incoming {
    Request(Result<AgentRequest, RecvError>),
    Record(Result<Result<PiIncomingRecord, AgentError>, RecvError>),
}

pub fn connect(cwd: &Path) -> Result<AgentConnection, AgentError> {
    let parent_pid = process::id() as libc::pid_t;
    let mut command = Command::new("pi");
    command
        .args(["--mode", "rpc"])
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
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
    let agent = PiCodingAgent {
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

    let (requests, receiver) = async_channel::unbounded();
    let (sender, events) = async_channel::unbounded();
    thread::Builder::new()
        .name("pi-rpc".into())
        .spawn(move || agent.run(receiver, sender))?;
    Ok(AgentConnection {
        handle: AgentHandle { requests },
        events,
    })
}

impl PiCodingAgent {
    fn run(mut self, requests: Receiver<AgentRequest>, events: Sender<AgentEvent>) {
        let result = block_on(self.process_requests(&requests, &events));
        requests.close();
        if let Err(error) = result {
            eprintln!("rockstar: {error:#}");
            process::exit(1);
        }
    }

    async fn process_requests(
        &mut self,
        requests: &Receiver<AgentRequest>,
        events: &Sender<AgentEvent>,
    ) -> Result<(), AgentError> {
        let mut pending = VecDeque::new();
        let mut active: Option<ActiveRequest> = None;
        loop {
            if requests.is_closed() || events.is_closed() {
                return Ok(());
            }
            if active.is_none()
                && let Some(request) = pending.pop_front()
            {
                active = Some(self.start_request(request)?);
            }
            let incoming = or(async { Incoming::Request(requests.recv().await) }, async {
                Incoming::Record(self.records.recv().await)
            })
            .await;
            let record = match incoming {
                Incoming::Request(Ok(request)) => {
                    pending.push_back(request);
                    continue;
                }
                Incoming::Request(Err(_)) => return Ok(()),
                Incoming::Record(result) => result.context("pi disconnected")??,
            };
            self.handle_extension(&record)?;
            if let Some(event) = decode_event(&record)
                && events.send(event).await.is_err()
            {
                return Ok(());
            }
            if let Some(event) = active
                .as_mut()
                .and_then(|request| request.handle_record(&record))
            {
                active = None;
                if events.send(event).await.is_err() {
                    return Ok(());
                }
            }
        }
    }

    fn start_request(&mut self, request: AgentRequest) -> Result<ActiveRequest, AgentError> {
        self.next_request_id += 1;
        let id = self.next_request_id.to_string();
        let (record, active) = match request {
            AgentRequest::Prompt(message) => (
                PiOutgoingRecord::Prompt {
                    id: id.clone(),
                    message,
                },
                ActiveRequest::Prompt {
                    id,
                    accepted: false,
                    settled: false,
                    failure: None,
                },
            ),
            AgentRequest::NewSession => (
                PiOutgoingRecord::NewSession { id: id.clone() },
                ActiveRequest::NewSession { id },
            ),
        };
        self.write_record(&record)?;
        Ok(active)
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
            Self::NewSession { id } => {
                let PiIncomingRecord::Response(PiResponse::NewSession(response)) = record else {
                    return None;
                };
                if response.id.as_deref() != Some(id.as_str()) {
                    return None;
                }
                let event = match &response.result {
                    Ok(NewSessionResponseData { cancelled: false }) => {
                        AgentEvent::NewSessionFinished
                    }
                    Ok(NewSessionResponseData { cancelled: true }) => {
                        AgentEvent::Error(anyhow!("New session cancelled."))
                    }
                    Err(error) => AgentEvent::Error(anyhow!("{error}")),
                };
                Some(event)
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
            let text = match content {
                MessageContent::Text(text) => text.clone(),
                MessageContent::Blocks(blocks) => message_text(blocks),
            };
            (!text.is_empty()).then_some(AgentEvent::UserMessage(text))
        }
        PiIncomingRecord::MessageEnd {
            message: PiMessage::Assistant { content, .. },
        } => Some(AgentEvent::AssistantFinished(message_text(content))),
        _ => None,
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
