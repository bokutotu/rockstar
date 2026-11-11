use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

use anyhow::{Context as _, anyhow, bail};
use async_channel::{Receiver, RecvError, TryRecvError};
use chrono::{DateTime, Utc};
use futures_lite::future::{block_on, or};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::{
    self, AgentBackend, AgentCommand, AgentError, AgentEvent, ChatStart, ContextUsage, Message,
    OpenedChat, PromptRequest, ResumeTarget, SavedSession,
};

const SYSTEM_PROMPT_APPEND: &str = "Never call the ask_question tool or any interactive question UI. If clarification is essential, ask concisely in plain assistant text; otherwise decide autonomously and proceed.";

#[derive(Debug)]
enum IncomingFrame {
    Response {
        id: u64,
        result: Result<Value, String>,
    },
    Notification {
        method: String,
        params: Value,
    },
    ServerRequest {
        id: Value,
        method: String,
        params: Value,
    },
}

#[derive(Debug, PartialEq, Eq)]
enum ServerReply {
    Result(Value),
    Error { code: i64, message: String },
}

struct AgyAgent {
    child: Child,
    input: ChildStdin,
    frames: Receiver<Result<IncomingFrame, AgentError>>,
    next_request_id: u64,
    session_id: String,
    queued: VecDeque<PromptRequest>,
    active: Option<ActivePrompt>,
}

impl Drop for AgyAgent {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct ActivePrompt {
    request_id: u64,
    segment_text: String,
    assistant_open: bool,
    cancelled: bool,
}

impl ActivePrompt {
    fn new(request_id: u64) -> Self {
        Self {
            request_id,
            segment_text: String::new(),
            assistant_open: false,
            cancelled: false,
        }
    }
}

enum Incoming {
    Command(Result<AgentCommand, RecvError>),
    Frame(Result<Result<IncomingFrame, AgentError>, RecvError>),
    ResultsClosed,
}

pub(crate) fn connect(cwd: &Path, start: ChatStart) -> Result<OpenedChat, AgentError> {
    let canonical_cwd = cwd
        .canonicalize()
        .context("Cannot resolve the workspace directory")?;
    let resume_id = match start {
        ChatStart::New => None,
        ChatStart::Resume(ResumeTarget::Agy(id)) => Some(id),
        ChatStart::Resume(_) => bail!("Selected session does not belong to agy"),
    };

    let mut agy = spawn_acp(&canonical_cwd, Stdio::inherit())?;
    let (agent, backend) = agent::channel();
    let (ready, initialized) = mpsc::channel();
    thread::Builder::new()
        .name("agy-rpc".into())
        .spawn(move || {
            let startup =
                block_on(agy.initialize_session(&canonical_cwd, resume_id.as_deref(), &backend));
            match startup {
                Ok(messages) => {
                    if ready.send(Ok(messages)).is_ok() {
                        agy.run(backend);
                    }
                }
                Err(error) => {
                    let _ = ready.send(Err(error));
                }
            }
        })?;
    let messages = initialized
        .recv_timeout(Duration::from_secs(30))
        .context("Timed out or disconnected while opening the agy session")??;
    Ok(OpenedChat { agent, messages })
}

pub(crate) fn list_sessions(root: &Path) -> Result<Vec<SavedSession>, AgentError> {
    let root = root
        .canonicalize()
        .context("Cannot resolve the workspace directory")?;
    let mut agy = spawn_acp(&root, Stdio::null())?;
    let (done_tx, done_rx) = mpsc::channel();
    let root_for_rpc = root.clone();
    thread::Builder::new()
        .name("agy-list".into())
        .spawn(move || {
            let result = block_on(async {
                agy.initialize_handshake(None).await?;
                let id = agy.next_id();
                agy.send_request(
                    id,
                    "session/list",
                    json!({ "cwd": root_for_rpc.to_string_lossy() }),
                )?;
                let response = agy.read_response(id, None, None).await?;
                parse_session_list(&response, &root_for_rpc)
            });
            let _ = done_tx.send(result);
        })?;
    done_rx
        .recv_timeout(Duration::from_secs(15))
        .context("Timed out while listing agy sessions")?
}

fn spawn_acp(cwd: &Path, stderr: Stdio) -> Result<AgyAgent, AgentError> {
    let mut command = Command::new("agy");
    command
        .args(["acp", "--dangerously-skip-permissions"])
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(stderr);
    agent::arm_parent_death_signal(&mut command);
    let mut child = command
        .spawn()
        .context("Could not start agy. Make sure agy is installed and available on PATH.")?;
    let input = child.stdin.take().expect("piped agy stdin");
    let output = child.stdout.take().expect("piped agy stdout");
    let (frame_sender, frames) = async_channel::unbounded();
    let _ = thread::Builder::new()
        .name("agy-stdout".into())
        .spawn(move || {
            let result = (|| -> Result<(), AgentError> {
                for line in BufReader::new(output).lines() {
                    let line = line?;
                    if line.trim().is_empty() {
                        continue;
                    }
                    let frame = parse_frame(&line)?;
                    if frame_sender.send_blocking(Ok(frame)).is_err() {
                        return Ok(());
                    }
                }
                bail!("agy closed its output")
            })();
            if let Err(error) = result {
                let _ = frame_sender.send_blocking(Err(error));
            }
        })?;
    Ok(AgyAgent {
        child,
        input,
        frames,
        next_request_id: 0,
        session_id: String::new(),
        queued: VecDeque::new(),
        active: None,
    })
}

fn session_meta() -> Value {
    json!({
        "antigravity": {
            "systemPrompt": {
                "append": SYSTEM_PROMPT_APPEND,
            }
        }
    })
}

impl AgyAgent {
    async fn initialize_handshake(
        &mut self,
        backend: Option<&AgentBackend>,
    ) -> Result<(), AgentError> {
        let id = self.next_id();
        self.send_request(
            id,
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": {},
                "clientInfo": {
                    "name": "rockstar",
                    "version": env!("CARGO_PKG_VERSION"),
                },
            }),
        )?;
        self.read_response(id, backend, None).await?;
        Ok(())
    }

    async fn initialize_session(
        &mut self,
        cwd: &Path,
        resume_id: Option<&str>,
        backend: &AgentBackend,
    ) -> Result<Vec<Message>, AgentError> {
        self.initialize_handshake(Some(backend)).await?;
        let cwd_str = cwd.to_string_lossy();
        match resume_id {
            None => {
                let id = self.next_id();
                self.send_request(
                    id,
                    "session/new",
                    json!({
                        "cwd": cwd_str,
                        "mcpServers": [],
                        "_meta": session_meta(),
                    }),
                )?;
                let response = self.read_response(id, Some(backend), None).await?;
                let session_id = response
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| anyhow!("agy did not return a sessionId"))?;
                self.session_id = session_id.to_owned();
                Ok(Vec::new())
            }
            Some(target_id) => {
                self.session_id = target_id.to_owned();
                let id = self.next_id();
                let mut history = HistoryBuilder::default();
                self.send_request(
                    id,
                    "session/load",
                    json!({
                        "sessionId": target_id,
                        "cwd": cwd_str,
                        "mcpServers": [],
                        "_meta": session_meta(),
                    }),
                )?;
                let response = self
                    .read_response(id, Some(backend), Some((target_id, &mut history)))
                    .await?;
                let loaded_id = response
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or(target_id);
                if loaded_id != target_id {
                    bail!("agy opened a different session; the original chat has been kept");
                }
                self.session_id = loaded_id.to_owned();
                Ok(history.finish())
            }
        }
    }

    async fn read_response(
        &mut self,
        expected_id: u64,
        backend: Option<&AgentBackend>,
        mut history: Option<(&str, &mut HistoryBuilder)>,
    ) -> Result<Value, AgentError> {
        let mut context_updates = Vec::new();
        loop {
            let frame = match backend {
                Some(backend) => {
                    or(
                        async {
                            self.frames
                                .recv()
                                .await
                                .context("agy disconnected during startup")?
                        },
                        async {
                            backend.events.closed().await;
                            Err(anyhow!("Opening the chat was cancelled"))
                        },
                    )
                    .await?
                }
                None => self
                    .frames
                    .recv()
                    .await
                    .context("agy disconnected")??,
            };
            match frame {
                IncomingFrame::ServerRequest { id, method, params } => {
                    let reply = handle_server_request(&method, &params);
                    self.send_reply(id, reply)?;
                }
                IncomingFrame::Notification { method, params } => {
                    if method == "session/update" {
                        let update = &params["update"];
                        if let Some((session_id, builder)) = history.as_mut()
                            && params.get("sessionId").and_then(Value::as_str) == Some(*session_id)
                        {
                            builder.push(update);
                        }
                        if backend.is_some()
                            && update["sessionUpdate"].as_str() == Some("usage_update")
                            && let Some(session_id) = params.get("sessionId").and_then(Value::as_str)
                        {
                            context_updates.push((
                                session_id.to_owned(),
                                decode_context_usage(update),
                            ));
                        }
                    }
                }
                IncomingFrame::Response { id, result } if id == expected_id => {
                    let response = result.map_err(|error| anyhow!("{error}"))?;
                    if let Some(backend) = backend {
                        let session_id = response
                            .get("sessionId")
                            .and_then(Value::as_str)
                            .unwrap_or(&self.session_id);
                        for (update_session_id, usage) in context_updates {
                            if update_session_id == session_id {
                                backend
                                    .events
                                    .try_send(AgentEvent::ContextUsageUpdated(usage))
                                    .map_err(|_| anyhow!("Opening the chat was cancelled"))?;
                            }
                        }
                    }
                    return Ok(response);
                }
                IncomingFrame::Response { .. } => {}
            }
        }
    }

    fn run(mut self, backend: AgentBackend) {
        let result = block_on(self.process_requests(&backend));
        if let Err(error) = result {
            backend.commands.close();
            let mut pending: Vec<String> = self.queued.drain(..).map(|p| p.text).collect();
            while let Ok(command) = backend.commands.try_recv() {
                if let AgentCommand::Prompt(prompt) = command {
                    pending.push(prompt.text);
                }
            }
            agent::return_pending_prompts(&backend, pending);
            let _ = backend.events.try_send(AgentEvent::Error(error));
        }
    }

    async fn process_requests(&mut self, backend: &AgentBackend) -> Result<(), AgentError> {
        loop {
            if backend.events.is_closed() {
                return Ok(());
            }
            let incoming = match backend.commands.try_recv() {
                Ok(command) => Incoming::Command(Ok(command)),
                Err(TryRecvError::Closed) => return Ok(()),
                Err(TryRecvError::Empty) => {
                    if self.active.is_none()
                        && let Some(prompt) = self.queued.pop_front()
                    {
                        self.start_prompt(prompt, backend)?;
                    }
                    or(
                        async {
                            backend.events.closed().await;
                            Incoming::ResultsClosed
                        },
                        or(
                            async { Incoming::Command(backend.commands.recv().await) },
                            async { Incoming::Frame(self.frames.recv().await) },
                        ),
                    )
                    .await
                }
            };
            let frame = match incoming {
                Incoming::Command(Ok(AgentCommand::Prompt(prompt))) => {
                    self.queued.push_back(prompt);
                    continue;
                }
                Incoming::Command(Ok(AgentCommand::Cancel)) => {
                    let pending = self.queued.drain(..).map(|prompt| prompt.text).collect();
                    agent::return_pending_prompts(backend, pending);
                    if let Some(active) = &mut self.active
                        && !active.cancelled
                    {
                        active.cancelled = true;
                        self.send_notification(
                            "session/cancel",
                            json!({ "sessionId": self.session_id }),
                        )?;
                    }
                    continue;
                }
                Incoming::Command(Err(_)) => return Ok(()),
                Incoming::Frame(result) => result.context("agy disconnected")??,
                Incoming::ResultsClosed => return Ok(()),
            };
            match frame {
                IncomingFrame::ServerRequest { id, method, params } => {
                    let reply = handle_server_request(&method, &params);
                    self.send_reply(id, reply)?;
                }
                IncomingFrame::Notification { method, params } => {
                    if method != "session/update"
                        || params.get("sessionId").and_then(Value::as_str)
                            != Some(self.session_id.as_str())
                    {
                        continue;
                    }
                    let update = &params["update"];
                    if update["sessionUpdate"].as_str() == Some("usage_update") {
                        let usage = decode_context_usage(update);
                        if backend
                            .events
                            .try_send(AgentEvent::ContextUsageUpdated(usage))
                            .is_err()
                        {
                            return Ok(());
                        }
                    } else if let Some(active) = &mut self.active {
                        for event in active.handle_update(update) {
                            if backend.events.try_send(event).is_err() {
                                return Ok(());
                            }
                        }
                    }
                }
                IncomingFrame::Response { id, result } => {
                    if self
                        .active
                        .as_ref()
                        .is_some_and(|active| active.request_id == id)
                    {
                        let active = self.active.take().expect("checked active prompt");
                        for event in active.finish(result) {
                            if backend.events.try_send(event).is_err() {
                                return Ok(());
                            }
                        }
                    }
                }
            }
        }
    }

    fn start_prompt(
        &mut self,
        prompt: PromptRequest,
        backend: &AgentBackend,
    ) -> Result<(), AgentError> {
        let id = self.next_id();
        let _ = backend
            .events
            .try_send(AgentEvent::UserMessage(prompt.text.clone()));
        self.send_request(
            id,
            "session/prompt",
            json!({
                "sessionId": self.session_id,
                "prompt": [{ "type": "text", "text": prompt.text }],
            }),
        )?;
        self.active = Some(ActivePrompt::new(id));
        Ok(())
    }

    fn next_id(&mut self) -> u64 {
        self.next_request_id += 1;
        self.next_request_id
    }

    fn send_request(&mut self, id: u64, method: &str, params: Value) -> Result<(), AgentError> {
        self.write_json(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
    }

    fn send_notification(&mut self, method: &str, params: Value) -> Result<(), AgentError> {
        self.write_json(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
    }

    fn send_reply(&mut self, id: Value, reply: ServerReply) -> Result<(), AgentError> {
        let payload = match reply {
            ServerReply::Result(result) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result,
            }),
            ServerReply::Error { code, message } => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": code,
                    "message": message,
                },
            }),
        };
        self.write_json(&payload)
    }

    fn write_json(&mut self, value: &Value) -> Result<(), AgentError> {
        let input = &mut self.input;
        serde_json::to_writer(&mut *input, value)?;
        input.write_all(b"\n")?;
        input.flush()?;
        Ok(())
    }
}

fn decode_context_usage(update: &Value) -> Option<ContextUsage> {
    let used_tokens = update.get("used").and_then(Value::as_u64)?;
    let limit_tokens = update
        .get("size")
        .and_then(Value::as_u64)
        .filter(|&limit| limit > 0)?;
    Some(ContextUsage {
        used_tokens,
        limit_tokens,
    })
}

impl ActivePrompt {
    fn flush_segment(&mut self, events: &mut Vec<AgentEvent>) {
        if self.assistant_open {
            self.assistant_open = false;
            let text = std::mem::take(&mut self.segment_text);
            events.push(AgentEvent::AssistantFinished(text));
        }
    }

    fn handle_update(&mut self, update: &Value) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        match update.get("sessionUpdate").and_then(Value::as_str) {
            Some("agent_message_chunk") => {
                if let Some(text) = chunk_text(&update["content"])
                    && !text.is_empty()
                {
                    if !self.assistant_open {
                        self.assistant_open = true;
                        events.push(AgentEvent::AssistantStarted);
                    }
                    self.segment_text.push_str(text);
                    events.push(AgentEvent::AssistantTextDelta(text.to_owned()));
                }
            }
            Some("tool_call") => {
                self.flush_segment(&mut events);
                let name = update
                    .get("name")
                    .or_else(|| update.get("title"))
                    .and_then(Value::as_str)
                    .unwrap_or("tool");
                let args = update
                    .get("args")
                    .filter(|v| v.is_object())
                    .or_else(|| update.get("rawInput"))
                    .unwrap_or(&Value::Null);
                events.push(AgentEvent::ToolActivity(tool_activity(name, args)));
            }
            Some("agent_thought_chunk") => {
                self.flush_segment(&mut events);
            }
            _ => {}
        }
        events
    }

    fn finish(mut self, result: Result<Value, String>) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        self.flush_segment(&mut events);
        match result {
            Ok(value) => {
                let stop_reason = value.get("stopReason").and_then(Value::as_str);
                if self.cancelled || stop_reason == Some("cancelled") {
                    events.push(AgentEvent::PromptCancelled);
                } else {
                    events.push(AgentEvent::PromptFinished);
                }
            }
            Err(error) => {
                if self.cancelled {
                    events.push(AgentEvent::PromptCancelled);
                } else {
                    events.push(AgentEvent::Error(anyhow!("{error}")));
                }
            }
        }
        events
    }
}

#[derive(Default)]
struct HistoryBuilder {
    messages: Vec<Message>,
    user_open: bool,
    assistant_open: bool,
}

impl HistoryBuilder {
    fn push(&mut self, update: &Value) {
        match update.get("sessionUpdate").and_then(Value::as_str) {
            Some("user_message_chunk") => {
                self.assistant_open = false;
                if let Some(text) = chunk_text(&update["content"])
                    && !text.is_empty()
                {
                    if self.user_open
                        && let Some(Message::User(existing)) = self.messages.last_mut()
                    {
                        existing.push_str(text);
                    } else {
                        self.user_open = true;
                        self.messages.push(Message::User(text.to_owned()));
                    }
                }
            }
            Some("agent_message_chunk") => {
                self.user_open = false;
                if let Some(text) = chunk_text(&update["content"])
                    && !text.is_empty()
                {
                    if self.assistant_open
                        && let Some(Message::Assistant(existing)) = self.messages.last_mut()
                    {
                        existing.push_str(text);
                    } else {
                        self.assistant_open = true;
                        self.messages.push(Message::Assistant(text.to_owned()));
                    }
                }
            }
            Some("tool_call" | "agent_thought_chunk") => {
                self.user_open = false;
                self.assistant_open = false;
            }
            _ => {}
        }
    }

    fn finish(self) -> Vec<Message> {
        self.messages
    }
}

fn chunk_text(content: &Value) -> Option<&str> {
    (content.get("type").and_then(Value::as_str) == Some("text"))
        .then(|| content.get("text").and_then(Value::as_str))
        .flatten()
}

fn parse_frame(line: &str) -> Result<IncomingFrame, AgentError> {
    let value: Value = serde_json::from_str(line).context("agy returned an invalid JSON record")?;
    let has_method = value
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let has_id = value.get("id").filter(|id| !id.is_null()).cloned();
    match (has_method, has_id) {
        (Some(method), Some(id)) => Ok(IncomingFrame::ServerRequest {
            id,
            method,
            params: value.get("params").cloned().unwrap_or(Value::Null),
        }),
        (Some(method), None) => Ok(IncomingFrame::Notification {
            method,
            params: value.get("params").cloned().unwrap_or(Value::Null),
        }),
        (None, Some(id)) => {
            let id = id
                .as_u64()
                .ok_or_else(|| anyhow!("agy returned a non-numeric response id"))?;
            let result = if let Some(error) = value.get("error").filter(|e| !e.is_null()) {
                let message = error
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| error.to_string());
                Err(message)
            } else {
                Ok(value.get("result").cloned().unwrap_or(Value::Null))
            };
            Ok(IncomingFrame::Response { id, result })
        }
        (None, None) => bail!("agy returned a JSON-RPC record with neither method nor id"),
    }
}

fn handle_server_request(method: &str, params: &Value) -> ServerReply {
    if method == "session/request_permission" {
        let options = params
            .get("options")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let chosen = options
            .iter()
            .find(|opt| opt.get("kind").and_then(Value::as_str) == Some("allow_always"))
            .or_else(|| {
                options.iter().find(|opt| {
                    opt.get("kind")
                        .and_then(Value::as_str)
                        .is_some_and(|kind| kind.starts_with("allow"))
                })
            })
            .or_else(|| options.first())
            .and_then(|opt| opt.get("optionId").and_then(Value::as_str));
        return match chosen {
            Some(option_id) => ServerReply::Result(json!({
                "outcome": {
                    "outcome": "selected",
                    "optionId": option_id,
                }
            })),
            None => ServerReply::Result(json!({
                "outcome": {
                    "outcome": "cancelled",
                }
            })),
        };
    }
    ServerReply::Error {
        code: -32601,
        message: "Interactive UI requests are disabled in Rockstar; ask in plain text or proceed autonomously.".into(),
    }
}

/// Show the operation's target, never file contents, edits, or tool output.
fn tool_activity(name: &str, args: &Value) -> String {
    let detail = match name {
        "read_file" | "write_file" | "edit_file" | "view_file" | "delete_file" => {
            args.get("Path").or_else(|| args.get("path"))
        }
        "run_command" => args.get("CommandLine").or_else(|| args.get("command")),
        "grep_search" | "find_by_name" => args
            .get("Query")
            .or_else(|| args.get("Pattern"))
            .or_else(|| args.get("SearchPath")),
        "search_web" => args.get("query"),
        "read_url_content" => args.get("Url").or_else(|| args.get("url")),
        _ => None,
    }
    .and_then(Value::as_str)
    .map(str::trim)
    .filter(|detail| !detail.is_empty());
    let text = match detail {
        Some(detail) => format!("{name} {detail}"),
        None => name.to_owned(),
    };
    agent::sanitize_activity(&text)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionListEntry {
    session_id: String,
    cwd: Option<String>,
    title: Option<String>,
    updated_at: Option<String>,
}

fn parse_session_list(response: &Value, root: &Path) -> Result<Vec<SavedSession>, AgentError> {
    let entries: Vec<SessionListEntry> = response
        .get("sessions")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .context("Invalid agy session list")?
        .unwrap_or_default();
    let mut sessions = Vec::new();
    for entry in entries {
        if entry.session_id.is_empty() {
            continue;
        }
        let Some(cwd) = entry.cwd.as_deref() else {
            continue;
        };
        if !cwd_belongs_to(cwd, root) {
            continue;
        }
        let updated_at = entry
            .updated_at
            .as_deref()
            .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
            .map(|time| time.with_timezone(&Utc))
            .unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
        let detail = format!("agy · {}", entry.session_id);
        sessions.push(SavedSession {
            target: ResumeTarget::Agy(entry.session_id),
            detail,
            title: agent::summarize_title(entry.title.as_deref()),
            updated_at,
        });
    }
    sessions.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.detail.cmp(&b.detail))
    });
    Ok(sessions)
}

fn cwd_belongs_to(raw_cwd: &str, root: &Path) -> bool {
    let candidate = Path::new(raw_cwd);
    if candidate == root || candidate.canonicalize().is_ok_and(|path| path == root) {
        return true;
    }
    if raw_cwd.contains('%')
        && let Ok(url) = url::Url::parse(&format!("file://{raw_cwd}"))
        && let Ok(decoded) = url.to_file_path()
    {
        return decoded == root || decoded.canonicalize().is_ok_and(|path| path == root);
    }
    false
}
