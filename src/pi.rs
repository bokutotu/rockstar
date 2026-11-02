use std::{
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, anyhow, bail};
use async_channel::Receiver;
use serde_json::{Value, json};

use crate::agent::{AgentError, CodingAgent, Message};

type Input = Arc<Mutex<ChildStdin>>;

pub struct PiCodingAgent {
    child: Child,
    input: Option<Input>,
    records: Receiver<Result<Value, AgentError>>,
    next_request_id: u64,
}

impl PiCodingAgent {
    pub fn new(cwd: &Path) -> Result<Self, AgentError> {
        let mut child = Command::new("pi")
            .args(["--mode", "rpc"])
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("Could not start pi. Make sure pi is installed and available on PATH.")?;
        let input = Arc::new(Mutex::new(child.stdin.take().expect("piped pi stdin")));
        let output = child.stdout.take().expect("piped pi stdout");
        let (sender, records) = async_channel::unbounded();
        let weak_input = Arc::downgrade(&input);

        // Always drain stdout, including while the user is editing. Only queue
        // completed messages and control records; token deltas are not displayed.
        thread::spawn(move || {
            let result = (|| -> Result<(), AgentError> {
                for line in BufReader::new(output).lines() {
                    let record: Value = serde_json::from_str(&line?)
                        .context("pi returned an invalid JSONL record")?;
                    match record["type"].as_str() {
                        Some("extension_ui_request") => match record["method"].as_str() {
                            Some("select" | "confirm" | "input" | "editor") => {
                                let Some(input) = weak_input.upgrade() else {
                                    return Ok(());
                                };
                                write_record(
                                    &input,
                                    &json!({
                                        "type": "extension_ui_response",
                                        "id": record["id"],
                                        "cancelled": true,
                                    }),
                                )?;
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
                        Some("extension_error") => {
                            eprintln!("pi extension: {}", record["error"]);
                        }
                        Some("response" | "message_end" | "agent_start" | "agent_settled") => {
                            sender.send_blocking(Ok(record))?;
                        }
                        _ => {}
                    }
                }
                bail!("pi closed its output. Restart Rockstar to start a new session.")
            })();
            if let Err(error) = result {
                let _ = sender.send_blocking(Err(error));
            }
        });

        Ok(Self {
            child,
            input: Some(input),
            records,
            next_request_id: 0,
        })
    }
}

impl CodingAgent for PiCodingAgent {
    async fn prompt(&mut self, input: String) -> Result<Vec<Message>, AgentError> {
        if self.records.is_closed() {
            bail!("pi is no longer connected. Restart Rockstar to start a new session.");
        }
        self.next_request_id += 1;
        let id = self.next_request_id.to_string();
        write_record(
            self.input.as_ref().expect("live pi stdin"),
            &json!({"id": id, "type": "prompt", "message": input}),
        )?;

        let mut messages = Vec::new();
        let mut accepted = false;
        let mut settled = false;
        let mut failure = None;
        loop {
            let record = self.records.recv().await.context("pi disconnected")??;
            match record["type"].as_str() {
                Some("response") if record["id"].as_str() == Some(&id) => {
                    if record["success"] != true {
                        bail!(
                            "{}",
                            record["error"]
                                .as_str()
                                .unwrap_or("pi rejected the request")
                        );
                    }
                    if record["data"]["disposition"] == "handled" {
                        return Ok(messages);
                    }
                    accepted = true;
                }
                Some("agent_start") => settled = false,
                Some("message_end") => {
                    let message = &record["message"];
                    if message["role"] == "assistant" {
                        match message["stopReason"].as_str() {
                            Some("error" | "aborted") => {
                                failure = Some(anyhow!(
                                    "{}",
                                    message["errorMessage"]
                                        .as_str()
                                        .unwrap_or("pi could not complete the response")
                                ));
                                continue;
                            }
                            _ => failure = None,
                        }
                    }
                    if let Some(message) = completed_message(message) {
                        messages.push(message);
                    }
                }
                Some("agent_settled") => {
                    settled = true;
                    if record["aborted"] == true {
                        failure = Some(anyhow!("The pi request was cancelled."));
                    }
                }
                _ => {}
            }
            // Acceptance is not completion. Retrying and tool execution can
            // produce several agent_end events before the session settles.
            if accepted && settled {
                return match failure {
                    Some(error) => Err(error),
                    None => Ok(messages),
                };
            }
        }
    }
}

fn write_record(input: &Input, record: &Value) -> Result<(), AgentError> {
    let mut input = input.lock().map_err(|_| anyhow!("pi input lock failed"))?;
    serde_json::to_writer(&mut *input, record)?;
    input.write_all(b"\n")?;
    input.flush()?;
    Ok(())
}

fn completed_message(message: &Value) -> Option<Message> {
    let role = message["role"].as_str()?;
    if !matches!(role, "user" | "assistant") {
        return None;
    }
    let text = match &message["content"] {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block["type"] == "text")
            .filter_map(|block| block["text"].as_str())
            .collect::<String>(),
        _ => return None,
    };
    if text.is_empty() {
        return None;
    }
    Some(match role {
        "user" => Message::User(text),
        _ => Message::Assistant(text),
    })
}

impl Drop for PiCodingAgent {
    fn drop(&mut self) {
        // EOF lets pi dispose its runtime and extensions. Bound the wait so a
        // broken child cannot keep Rockstar alive indefinitely on shutdown.
        self.input.take();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
                _ => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
