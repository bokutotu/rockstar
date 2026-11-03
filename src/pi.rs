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
use serde_json::{Value, json};

use crate::agent::{AgentConnection, AgentError, AgentEvent, AgentHandle, AgentRequest};

struct PiCodingAgent {
    input: ChildStdin,
    records: Receiver<Result<Value, AgentError>>,
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
    Record(Result<Result<Value, AgentError>, RecvError>),
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
    thread::Builder::new()
        .name("pi-stdout".into())
        .spawn(move || {
            let result = (|| -> Result<(), AgentError> {
                for line in BufReader::new(output).lines() {
                    let record = serde_json::from_str(&line?)
                        .context("pi returned an invalid JSONL record")?;
                    if record_sender.send_blocking(Ok(record)).is_err() {
                        return Ok(());
                    }
                }
                bail!("pi closed its output. Restart Rockstar to start a new session.")
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
            let _ = events.try_send(AgentEvent::Disconnected(error));
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
            if let Some(event) = decode_event(&record) {
                events.send(event).await?;
            }
            if let Some(event) = active
                .as_mut()
                .and_then(|request| request.handle_record(&record))
            {
                active = None;
                events.send(event).await?;
            }
        }
    }

    fn start_request(&mut self, request: AgentRequest) -> Result<ActiveRequest, AgentError> {
        self.next_request_id += 1;
        let id = self.next_request_id.to_string();
        let (record, active) = match request {
            AgentRequest::Prompt(text) => (
                json!({"id": id, "type": "prompt", "message": text}),
                ActiveRequest::Prompt {
                    id,
                    accepted: false,
                    settled: false,
                    failure: None,
                },
            ),
            AgentRequest::NewSession => (
                json!({"id": id, "type": "new_session"}),
                ActiveRequest::NewSession { id },
            ),
        };
        self.write_record(&record)?;
        Ok(active)
    }

    fn write_record(&mut self, record: &Value) -> Result<(), AgentError> {
        let input = &mut self.input;
        serde_json::to_writer(&mut *input, record)?;
        input.write_all(b"\n")?;
        input.flush()?;
        Ok(())
    }

    fn handle_extension(&mut self, record: &Value) -> Result<(), AgentError> {
        match record["type"].as_str() {
            Some("extension_ui_request") => match record["method"].as_str() {
                Some("select" | "confirm" | "input" | "editor") => {
                    self.write_record(&json!({
                        "type": "extension_ui_response",
                        "id": record["id"],
                        "cancelled": true,
                    }))?;
                    eprintln!(
                        "rockstar: cancelled an extension dialog; dialog UI is not available yet"
                    );
                }
                Some("notify") => {
                    if let Some(message) = record["message"].as_str() {
                        eprintln!("pi: {message}");
                    }
                }
                _ => {}
            },
            Some("extension_error") => eprintln!("pi extension: {}", record["error"]),
            _ => {}
        }
        Ok(())
    }
}

impl ActiveRequest {
    fn handle_record(&mut self, record: &Value) -> Option<AgentEvent> {
        match self {
            Self::Prompt {
                id,
                accepted,
                settled,
                failure,
            } => {
                match record["type"].as_str() {
                    Some("response") if record["id"].as_str() == Some(id.as_str()) => {
                        if record["success"] != true {
                            return Some(AgentEvent::PromptFinished(Err(response_error(record))));
                        }
                        match record["data"]["disposition"].as_str() {
                            Some("handled") => return Some(AgentEvent::PromptFinished(Ok(()))),
                            Some("started" | "queued") => *accepted = true,
                            _ => {
                                return Some(AgentEvent::PromptFinished(Err(anyhow!(
                                    "pi returned an invalid prompt response"
                                ))));
                            }
                        }
                    }
                    Some("agent_start") => *settled = false,
                    Some("message_end") if record["message"]["role"] == "assistant" => {
                        let message = &record["message"];
                        *failure = match message["stopReason"].as_str() {
                            Some("error" | "aborted") => Some(anyhow!(
                                "{}",
                                message["errorMessage"]
                                    .as_str()
                                    .unwrap_or("pi could not complete the response")
                            )),
                            _ => None,
                        };
                    }
                    Some("agent_settled") => {
                        *settled = true;
                        if record["aborted"] == true {
                            *failure = Some(anyhow!("The pi request was cancelled."));
                        }
                    }
                    _ => {}
                }
                (*accepted && *settled)
                    .then(|| AgentEvent::PromptFinished(failure.take().map_or(Ok(()), Err)))
            }
            Self::NewSession { id } => {
                if record["type"] != "response" || record["id"].as_str() != Some(id.as_str()) {
                    return None;
                }
                let result = if record["success"] != true {
                    Err(response_error(record))
                } else {
                    match record["data"]["cancelled"].as_bool() {
                        Some(false) => Ok(()),
                        Some(true) => Err(anyhow!("New session cancelled.")),
                        None => Err(anyhow!("pi returned an invalid new_session response")),
                    }
                };
                Some(AgentEvent::NewSessionFinished(result))
            }
        }
    }
}

fn response_error(record: &Value) -> AgentError {
    anyhow!(
        "{}",
        record["error"]
            .as_str()
            .unwrap_or("pi rejected the request")
    )
}

fn decode_event(record: &Value) -> Option<AgentEvent> {
    match record["type"].as_str()? {
        "message_start" if record["message"]["role"] == "assistant" => {
            Some(AgentEvent::AssistantStarted)
        }
        "message_update" if record["assistantMessageEvent"]["type"] == "text_delta" => {
            let text = record["assistantMessageEvent"]["delta"].as_str()?;
            (!text.is_empty()).then(|| AgentEvent::AssistantTextDelta(text.into()))
        }
        "message_end" => {
            let message = &record["message"];
            let text = message_text(message);
            match message["role"].as_str()? {
                "user" if !text.is_empty() => Some(AgentEvent::UserMessage(text)),
                "assistant" => Some(AgentEvent::AssistantFinished(text)),
                _ => None,
            }
        }
        _ => None,
    }
}

fn message_text(message: &Value) -> String {
    match &message["content"] {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block["type"] == "text")
            .filter_map(|block| block["text"].as_str())
            .collect(),
        _ => String::new(),
    }
}
