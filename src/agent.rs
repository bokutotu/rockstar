use std::path::PathBuf;

use async_channel::{Receiver, Sender};
use chrono::{DateTime, Utc};

pub type AgentError = anyhow::Error;

pub const DISCONNECTED_MESSAGE: &str =
    "pi is no longer connected. Use :resume to reopen a saved chat or :new to start a new one.";

/// A saved conversation offered by the resume picker. The path is its identity.
pub(crate) struct SavedSession {
    pub path: PathBuf,
    pub title: String,
    pub updated_at: DateTime<Utc>,
}

pub(crate) enum ChatStart {
    New,
    Resume(PathBuf),
}

/// A connection whose initial history has been loaded successfully.
pub(crate) struct OpenedChat {
    pub agent: Agent,
    pub messages: Vec<Message>,
}

#[derive(Clone, Debug)]
pub enum Message {
    User(String),
    Assistant(String),
}

#[derive(Debug)]
pub struct PromptRequest {
    pub text: String,
}

/// Current context-window usage, not cumulative session token consumption.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextUsage {
    pub used_tokens: u64,
    pub limit_tokens: u64,
}

#[derive(Debug)]
pub enum AgentEvent {
    UserMessage(String),
    AssistantStarted,
    AssistantTextDelta(String),
    AssistantFinished(String),
    ContextUsageUpdated(Option<ContextUsage>),
    PromptFinished,
    Error(AgentError),
}

pub struct Agent {
    prompts: Sender<PromptRequest>,
    results: Option<Receiver<AgentEvent>>,
}

pub(crate) struct AgentBackend {
    pub prompts: Receiver<PromptRequest>,
    pub events: Sender<AgentEvent>,
}

pub(crate) fn channel() -> (Agent, AgentBackend) {
    let (prompt_sender, prompts) = async_channel::unbounded();
    let (result_sender, results) = async_channel::unbounded();
    (
        Agent {
            prompts: prompt_sender,
            results: Some(results),
        },
        AgentBackend {
            prompts,
            events: result_sender,
        },
    )
}

impl Agent {
    pub fn prompt_sender(&self) -> Sender<PromptRequest> {
        self.prompts.clone()
    }

    pub fn take_results(&mut self) -> Option<Receiver<AgentEvent>> {
        self.results.take()
    }
}
