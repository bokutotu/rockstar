use async_channel::{Receiver, Sender};

pub type AgentError = anyhow::Error;

pub const DISCONNECTED_MESSAGE: &str =
    "pi is no longer connected. Restart Rockstar to start a new session.";

#[derive(Clone, Debug)]
pub enum Message {
    User(String),
    Assistant(String),
}

#[derive(Debug)]
pub struct PromptRequest {
    pub text: String,
}

#[derive(Debug)]
pub struct NewSessionRequest;

#[derive(Debug)]
pub enum AgentEvent {
    UserMessage(String),
    AssistantStarted,
    AssistantTextDelta(String),
    AssistantFinished(String),
    PromptFinished,
    NewSessionFinished,
    Error(AgentError),
}

pub struct Agent {
    prompts: Sender<PromptRequest>,
    new_sessions: Sender<NewSessionRequest>,
    results: Option<Receiver<AgentEvent>>,
}

pub(crate) struct AgentBackend {
    pub prompts: Receiver<PromptRequest>,
    pub new_sessions: Receiver<NewSessionRequest>,
    pub events: Sender<AgentEvent>,
}

pub(crate) fn channel() -> (Agent, AgentBackend) {
    let (prompt_sender, prompts) = async_channel::unbounded();
    let (session_sender, new_sessions) = async_channel::unbounded();
    let (result_sender, results) = async_channel::unbounded();
    (
        Agent {
            prompts: prompt_sender,
            new_sessions: session_sender,
            results: Some(results),
        },
        AgentBackend {
            prompts,
            new_sessions,
            events: result_sender,
        },
    )
}

impl Agent {
    pub fn prompt_sender(&self) -> Sender<PromptRequest> {
        self.prompts.clone()
    }

    pub fn new_session_sender(&self) -> Sender<NewSessionRequest> {
        self.new_sessions.clone()
    }

    pub fn take_results(&mut self) -> Option<Receiver<AgentEvent>> {
        self.results.take()
    }
}
