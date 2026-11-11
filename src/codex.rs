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

const DEVELOPER_INSTRUCTIONS: &str = "Never request interactive user input via question tools or dialogs. If clarification is essential, ask concisely in plain assistant text; otherwise decide autonomously and proceed.";

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

struct CodexAgent {
    child: Child,
    input: ChildStdin,
    frames: Receiver<Result<IncomingFrame, AgentError>>,
    next_request_id: u64,
    thread_id: String,
    queued: VecDeque<PromptRequest>,
    active: Option<ActiveTurn>,
}

impl Drop for CodexAgent {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct ActiveTurn {
    request_id: u64,
    turn_id: Option<String>,
    accepted: bool,
    settled: Option<TurnOutcome>,
    cancel_requested: bool,
    interrupt_sent: bool,
}

enum TurnOutcome {
    Finished,
    Cancelled,
    Failed(String),
}

impl ActiveTurn {
    fn new(request_id: u64) -> Self {
        Self {
            request_id,
            turn_id: None,
            accepted: false,
            settled: None,
            cancel_requested: false,
            interrupt_sent: false,
        }
    }

    fn take_final_event_if_ready(&mut self) -> Option<AgentEvent> {
        if !self.accepted {
            return None;
        }
        let settled = self.settled.take()?;
        Some(match settled {
            TurnOutcome::Finished if self.cancel_requested => AgentEvent::PromptCancelled,
            TurnOutcome::Finished => AgentEvent::PromptFinished,
            TurnOutcome::Cancelled => AgentEvent::PromptCancelled,
            TurnOutcome::Failed(_) if self.cancel_requested => AgentEvent::PromptCancelled,
            TurnOutcome::Failed(message) => AgentEvent::Error(anyhow!("{message}")),
        })
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
        ChatStart::Resume(ResumeTarget::Codex(id)) => Some(id),
        ChatStart::Resume(_) => bail!("Selected session does not belong to codex"),
    };

    let mut codex = spawn_app_server(&canonical_cwd, Stdio::inherit())?;
    let (agent, backend) = agent::channel();
    let (ready, initialized) = mpsc::channel();
    thread::Builder::new()
        .name("codex-rpc".into())
        .spawn(move || {
            let startup =
                block_on(codex.initialize_thread(&canonical_cwd, resume_id.as_deref(), &backend));
            match startup {
                Ok(messages) => {
                    if ready.send(Ok(messages)).is_ok() {
                        codex.run(backend);
                    }
                }
                Err(error) => {
                    let _ = ready.send(Err(error));
                }
            }
        })?;
    let messages = initialized
        .recv_timeout(Duration::from_secs(30))
        .context("Timed out or disconnected while opening the codex session")??;
    Ok(OpenedChat { agent, messages })
}

pub(crate) fn list_sessions(root: &Path) -> Result<Vec<SavedSession>, AgentError> {
    let root = root
        .canonicalize()
        .context("Cannot resolve the workspace directory")?;
    let mut codex = spawn_app_server(&root, Stdio::null())?;
    let (done_tx, done_rx) = mpsc::channel();
    let root_for_rpc = root.clone();
    thread::Builder::new()
        .name("codex-list".into())
        .spawn(move || {
            let result = block_on(async {
                codex.initialize_handshake(None).await?;
                let id = codex.next_id();
                codex.send_request(
                    id,
                    "thread/list",
                    json!({
                        "cwd": root_for_rpc.to_string_lossy(),
                        "sortKey": "updated_at",
                        "sortDirection": "desc",
                        "limit": 100,
                        "sourceKinds": ["cli", "vscode", "exec", "appServer"],
                        "useStateDbOnly": true,
                    }),
                )?;
                let response = codex.read_response(id, None).await?;
                parse_thread_list(&response, &root_for_rpc)
            });
            let _ = done_tx.send(result);
        })?;
    done_rx
        .recv_timeout(Duration::from_secs(15))
        .context("Timed out while listing codex sessions")?
}

fn spawn_app_server(cwd: &Path, stderr: Stdio) -> Result<CodexAgent, AgentError> {
    let mut command = Command::new("codex");
    command
        .args(["app-server", "--listen", "stdio://"])
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(stderr);
    agent::arm_parent_death_signal(&mut command);
    let mut child = command
        .spawn()
        .context("Could not start codex. Make sure codex is installed and available on PATH.")?;
    let input = child.stdin.take().expect("piped codex stdin");
    let output = child.stdout.take().expect("piped codex stdout");
    let (frame_sender, frames) = async_channel::unbounded();
    let _ = thread::Builder::new()
        .name("codex-stdout".into())
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
                bail!("codex closed its output")
            })();
            if let Err(error) = result {
                let _ = frame_sender.send_blocking(Err(error));
            }
        })?;
    Ok(CodexAgent {
        child,
        input,
        frames,
        next_request_id: 0,
        thread_id: String::new(),
        queued: VecDeque::new(),
        active: None,
    })
}

impl CodexAgent {
    async fn initialize_handshake(
        &mut self,
        backend: Option<&AgentBackend>,
    ) -> Result<(), AgentError> {
        let id = self.next_id();
        self.send_request(
            id,
            "initialize",
            json!({
                "clientInfo": {
                    "name": "rockstar",
                    "title": "Rockstar",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "capabilities": null,
            }),
        )?;
        self.read_response(id, backend).await?;
        self.send_notification("initialized", None)?;
        Ok(())
    }

    async fn initialize_thread(
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
                    "thread/start",
                    json!({
                        "cwd": cwd_str,
                        "approvalPolicy": "never",
                        "sandbox": "danger-full-access",
                        "developerInstructions": DEVELOPER_INSTRUCTIONS,
                    }),
                )?;
                let response = self.read_response(id, Some(backend)).await?;
                let thread_id = response["thread"]["id"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| anyhow!("codex did not return a thread id"))?;
                self.thread_id = thread_id.to_owned();
                Ok(Vec::new())
            }
            Some(target_id) => {
                self.thread_id = target_id.to_owned();
                let id = self.next_id();
                self.send_request(
                    id,
                    "thread/resume",
                    json!({
                        "threadId": target_id,
                        "cwd": cwd_str,
                        "approvalPolicy": "never",
                        "sandbox": "danger-full-access",
                        "developerInstructions": DEVELOPER_INSTRUCTIONS,
                        "excludeTurns": false,
                    }),
                )?;
                let response = self.read_response(id, Some(backend)).await?;
                let thread = &response["thread"];
                let loaded_id = thread["id"].as_str().unwrap_or(target_id);
                if loaded_id != target_id {
                    bail!("codex opened a different thread; the original chat has been kept");
                }
                if let Some(thread_cwd) = thread["cwd"].as_str()
                    && !cwd_belongs_to(thread_cwd, cwd)
                {
                    bail!("The selected session belongs to another workspace");
                }
                self.thread_id = loaded_id.to_owned();
                if turns_have_full_items(&thread["turns"]) {
                    Ok(messages_from_turns(&thread["turns"]))
                } else {
                    self.load_paginated_items(backend).await
                }
            }
        }
    }

    async fn load_paginated_items(
        &mut self,
        backend: &AgentBackend,
    ) -> Result<Vec<Message>, AgentError> {
        let mut messages = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let id = self.next_id();
            self.send_request(
                id,
                "thread/items/list",
                json!({
                    "threadId": self.thread_id,
                    "sortDirection": "asc",
                    "limit": 200,
                    "cursor": cursor,
                }),
            )?;
            let page = self.read_response(id, Some(backend)).await?;
            if let Some(entries) = page.get("data").and_then(Value::as_array) {
                for entry in entries {
                    if let Some(message) = item_to_message(&entry["item"]) {
                        messages.push(message);
                    }
                }
            }
            cursor = page
                .get("nextCursor")
                .and_then(Value::as_str)
                .filter(|next| !next.is_empty())
                .map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        Ok(messages)
    }

    async fn read_response(
        &mut self,
        expected_id: u64,
        backend: Option<&AgentBackend>,
    ) -> Result<Value, AgentError> {
        let mut context_events = Vec::new();
        loop {
            let frame = match backend {
                Some(backend) => {
                    or(
                        async {
                            self.frames
                                .recv()
                                .await
                                .context("codex disconnected during startup")?
                        },
                        async {
                            backend.events.closed().await;
                            Err(anyhow!("Opening the chat was cancelled"))
                        },
                    )
                    .await?
                }
                None => self.frames.recv().await.context("codex disconnected")??,
            };
            match frame {
                IncomingFrame::ServerRequest { id, method, params } => {
                    let reply = handle_server_request(&method, &params);
                    self.send_reply(id, reply)?;
                }
                IncomingFrame::Response { id, result } if id == expected_id => {
                    let response = result.map_err(|error| anyhow!("{error}"))?;
                    if let Some(backend) = backend {
                        let thread_id = response["thread"]["id"]
                            .as_str()
                            .unwrap_or(&self.thread_id);
                        for (params, event) in context_events {
                            if !thread_id.is_empty()
                                && notification_matches_thread(&params, thread_id)
                            {
                                backend
                                    .events
                                    .try_send(event)
                                    .map_err(|_| anyhow!("Opening the chat was cancelled"))?;
                            }
                        }
                    }
                    return Ok(response);
                }
                IncomingFrame::Notification { method, params } => {
                    if backend.is_some()
                        && let Some(event) = decode_context_event(&method, &params)
                    {
                        context_events.push((params, event));
                    }
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
                        self.start_turn(prompt, backend)?;
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
                    self.request_interrupt()?;
                    continue;
                }
                Incoming::Command(Err(_)) => return Ok(()),
                Incoming::Frame(result) => result.context("codex disconnected")??,
                Incoming::ResultsClosed => return Ok(()),
            };
            match frame {
                IncomingFrame::ServerRequest { id, method, params } => {
                    let reply = handle_server_request(&method, &params);
                    self.send_reply(id, reply)?;
                }
                IncomingFrame::Notification { method, params } => {
                    if !notification_matches_thread(&params, &self.thread_id) {
                        continue;
                    }
                    if method == "turn/started"
                        && let Some(turn_id) = params["turn"]["id"].as_str()
                    {
                        if let Some(active) = &mut self.active
                            && active.turn_id.is_none()
                        {
                            active.turn_id = Some(turn_id.to_owned());
                        }
                        self.maybe_send_pending_interrupt()?;
                    }
                    if let Some(event) = decode_notification(&method, &params)
                        && backend.events.try_send(event).is_err()
                    {
                        return Ok(());
                    }
                    if method == "turn/completed"
                        && let Some(active) = &mut self.active
                    {
                        active.settled = Some(decode_turn_outcome(&params["turn"]));
                        if let Some(final_event) = active.take_final_event_if_ready() {
                            self.active = None;
                            if backend.events.try_send(final_event).is_err() {
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
                        match result {
                            Ok(value) => {
                                if let Some(active) = &mut self.active {
                                    active.accepted = true;
                                    if active.turn_id.is_none()
                                        && let Some(turn_id) = value["turn"]["id"].as_str()
                                    {
                                        active.turn_id = Some(turn_id.to_owned());
                                    }
                                }
                                self.maybe_send_pending_interrupt()?;
                                if let Some(final_event) = self
                                    .active
                                    .as_mut()
                                    .and_then(ActiveTurn::take_final_event_if_ready)
                                {
                                    self.active = None;
                                    if backend.events.try_send(final_event).is_err() {
                                        return Ok(());
                                    }
                                }
                            }
                            Err(error) => {
                                let active = self.active.take().expect("checked active turn");
                                let event = if active.cancel_requested {
                                    AgentEvent::PromptCancelled
                                } else {
                                    AgentEvent::Error(anyhow!("{error}"))
                                };
                                if backend.events.try_send(event).is_err() {
                                    return Ok(());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn start_turn(
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
            "turn/start",
            json!({
                "threadId": self.thread_id,
                "input": [{
                    "type": "text",
                    "text": prompt.text,
                    "text_elements": [],
                }],
                "approvalPolicy": "never",
                "sandboxPolicy": { "type": "dangerFullAccess" },
            }),
        )?;
        self.active = Some(ActiveTurn::new(id));
        Ok(())
    }

    fn request_interrupt(&mut self) -> Result<(), AgentError> {
        if let Some(active) = &mut self.active {
            active.cancel_requested = true;
        }
        self.maybe_send_pending_interrupt()
    }

    fn maybe_send_pending_interrupt(&mut self) -> Result<(), AgentError> {
        let turn_id = match &mut self.active {
            Some(active) if active.cancel_requested && !active.interrupt_sent => {
                match active.turn_id.clone() {
                    Some(turn_id) => {
                        active.interrupt_sent = true;
                        turn_id
                    }
                    None => return Ok(()),
                }
            }
            _ => return Ok(()),
        };
        let interrupt_id = self.next_id();
        self.send_request(
            interrupt_id,
            "turn/interrupt",
            json!({
                "threadId": self.thread_id,
                "turnId": turn_id,
            }),
        )
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

    fn send_notification(&mut self, method: &str, params: Option<Value>) -> Result<(), AgentError> {
        let payload = match params {
            Some(params) => json!({ "jsonrpc": "2.0", "method": method, "params": params }),
            None => json!({ "jsonrpc": "2.0", "method": method }),
        };
        self.write_json(&payload)
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

fn notification_matches_thread(params: &Value, thread_id: &str) -> bool {
    match params
        .get("threadId")
        .or_else(|| params.get("thread")?.get("id"))
        .and_then(Value::as_str)
    {
        Some(id) => id == thread_id,
        None => true,
    }
}

fn decode_context_event(method: &str, params: &Value) -> Option<AgentEvent> {
    match method {
        "item/completed" if params["item"]["type"].as_str() == Some("contextCompaction") => {
            Some(AgentEvent::ContextUsageUpdated(None))
        }
        "thread/compacted" => Some(AgentEvent::ContextUsageUpdated(None)),
        "thread/tokenUsage/updated" => {
            let usage = &params["tokenUsage"];
            let context_usage = usage["last"]["totalTokens"]
                .as_u64()
                .zip(
                    usage["modelContextWindow"]
                        .as_u64()
                        .filter(|&limit| limit > 0),
                )
                .map(|(used_tokens, limit_tokens)| ContextUsage {
                    used_tokens,
                    limit_tokens,
                });
            Some(AgentEvent::ContextUsageUpdated(context_usage))
        }
        _ => None,
    }
}

fn decode_notification(method: &str, params: &Value) -> Option<AgentEvent> {
    if let Some(event) = decode_context_event(method, params) {
        return Some(event);
    }
    match method {
        "item/started" => {
            let item = &params["item"];
            match item.get("type").and_then(Value::as_str)? {
                "agentMessage" => Some(AgentEvent::AssistantStarted),
                _ => item_tool_activity(item).map(AgentEvent::ToolActivity),
            }
        }
        "item/agentMessage/delta" => {
            let delta = params.get("delta").and_then(Value::as_str)?;
            (!delta.is_empty()).then(|| AgentEvent::AssistantTextDelta(delta.to_owned()))
        }
        "item/completed" => {
            let item = &params["item"];
            match item.get("type").and_then(Value::as_str)? {
                "agentMessage" => {
                    let text = item.get("text").and_then(Value::as_str).unwrap_or_default();
                    Some(AgentEvent::AssistantFinished(text.to_owned()))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn decode_turn_outcome(turn: &Value) -> TurnOutcome {
    match turn.get("status").and_then(Value::as_str) {
        Some("interrupted") => TurnOutcome::Cancelled,
        Some("failed") => {
            let message = turn["error"]["message"]
                .as_str()
                .filter(|msg| !msg.is_empty())
                .unwrap_or("codex could not complete the turn")
                .to_owned();
            TurnOutcome::Failed(message)
        }
        _ => TurnOutcome::Finished,
    }
}

/// Show the operation's target, never file contents, diffs, or command output.
fn item_tool_activity(item: &Value) -> Option<String> {
    let kind = item.get("type").and_then(Value::as_str)?;
    let raw = match kind {
        "commandExecution" => match item
            .get("command")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|cmd| !cmd.is_empty())
        {
            Some(cmd) => format!("bash {cmd}"),
            None => "bash".into(),
        },
        "fileChange" => {
            let paths: Vec<&str> = item
                .get("changes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|change| change.get("path").and_then(Value::as_str))
                .map(str::trim)
                .filter(|path| !path.is_empty())
                .collect();
            if paths.is_empty() {
                "edit".into()
            } else {
                format!("edit {}", paths.join(", "))
            }
        }
        "mcpToolCall" => {
            let server = item
                .get("server")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let tool = item
                .get("tool")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|t| !t.is_empty());
            match (server, tool) {
                (Some(server), Some(tool)) => format!("{server}.{tool}"),
                (_, Some(tool)) => tool.to_owned(),
                _ => "mcp".into(),
            }
        }
        "dynamicToolCall" => item
            .get("tool")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .unwrap_or("tool")
            .to_owned(),
        "webSearch" => match item
            .get("query")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|q| !q.is_empty())
        {
            Some(query) => format!("search {query}"),
            None => "search".into(),
        },
        "imageView" => match item
            .get("path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            Some(path) => format!("view {path}"),
            None => "view".into(),
        },
        _ => return None,
    };
    Some(agent::sanitize_activity(&raw))
}

fn handle_server_request(method: &str, params: &Value) -> ServerReply {
    match method {
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            ServerReply::Result(json!({ "decision": "acceptForSession" }))
        }
        "execCommandApproval" | "applyPatchApproval" => {
            ServerReply::Result(json!({ "decision": "approved_for_session" }))
        }
        "item/permissions/requestApproval" => ServerReply::Result(json!({
            "permissions": params.get("permissions").cloned().unwrap_or_else(|| json!({})),
            "scope": "session",
        })),
        "item/tool/requestUserInput" => ServerReply::Result(json!({ "answers": {} })),
        "mcpServer/elicitation/request" => ServerReply::Result(json!({
            "action": "decline",
            "content": null,
            "_meta": null,
        })),
        "item/tool/call" => ServerReply::Result(json!({
            "contentItems": [],
            "success": false,
        })),
        _ => ServerReply::Error {
            code: -32601,
            message: "Interactive client requests are disabled in Rockstar.".into(),
        },
    }
}

fn parse_frame(line: &str) -> Result<IncomingFrame, AgentError> {
    let value: Value =
        serde_json::from_str(line).context("codex returned an invalid JSON record")?;
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
                .ok_or_else(|| anyhow!("codex returned a non-numeric response id"))?;
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
        (None, None) => bail!("codex returned a JSON-RPC record with neither method nor id"),
    }
}

fn turns_have_full_items(turns: &Value) -> bool {
    let Some(turns) = turns.as_array() else {
        return false;
    };
    !turns.is_empty()
        && turns.iter().all(|turn| {
            turn.get("itemsView")
                .and_then(Value::as_str)
                .is_none_or(|view| view == "full")
        })
}

fn messages_from_turns(turns: &Value) -> Vec<Message> {
    turns
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|turn| turn.get("items").and_then(Value::as_array).into_iter().flatten())
        .filter_map(item_to_message)
        .collect()
}

fn item_to_message(item: &Value) -> Option<Message> {
    match item.get("type").and_then(Value::as_str)? {
        "userMessage" => {
            let text: String = item
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            (!text.is_empty()).then_some(Message::User(text))
        }
        "agentMessage" => {
            let text = item.get("text").and_then(Value::as_str)?;
            (!text.is_empty()).then_some(Message::Assistant(text.to_owned()))
        }
        _ => None,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadSummary {
    id: String,
    parent_thread_id: Option<String>,
    #[serde(default)]
    ephemeral: bool,
    preview: Option<String>,
    name: Option<String>,
    updated_at: i64,
    cwd: Option<String>,
    path: Option<String>,
}

fn parse_thread_list(response: &Value, root: &Path) -> Result<Vec<SavedSession>, AgentError> {
    let entries: Vec<ThreadSummary> = response
        .get("data")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .context("Invalid codex thread list")?
        .unwrap_or_default();
    let mut sessions = Vec::new();
    for entry in entries {
        if entry.id.is_empty() || entry.ephemeral || entry.parent_thread_id.is_some() {
            continue;
        }
        if let Some(cwd) = entry.cwd.as_deref() {
            if !cwd_belongs_to(cwd, root) {
                continue;
            }
        } else {
            continue;
        }
        let raw_title = entry
            .name
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .or_else(|| {
                entry
                    .preview
                    .as_deref()
                    .filter(|preview| !preview.trim().is_empty())
            });
        let updated_at = DateTime::<Utc>::from_timestamp(entry.updated_at, 0)
            .unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
        let detail = entry
            .path
            .filter(|path| !path.is_empty())
            .unwrap_or_else(|| format!("codex · {}", entry.id));
        sessions.push(SavedSession {
            target: ResumeTarget::Codex(entry.id),
            detail,
            title: agent::summarize_title(raw_title),
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
    candidate == root || candidate.canonicalize().is_ok_and(|path| path == root)
}
