use async_channel::{Receiver, Sender};

pub type AgentError = anyhow::Error;

#[derive(Clone, Debug)]
pub enum Message {
    User(String),
    Assistant(String),
}

#[derive(Debug)]
pub enum AgentRequest {
    Prompt(String),
    NewSession,
}

#[derive(Debug)]
pub enum AgentEvent {
    UserMessage(String),
    AssistantStarted,
    AssistantTextDelta(String),
    AssistantFinished(String),
    PromptFinished(Result<(), AgentError>),
    NewSessionFinished(Result<(), AgentError>),
    Disconnected(AgentError),
}

#[derive(Clone)]
pub struct AgentHandle {
    pub(crate) requests: Sender<AgentRequest>,
}

impl AgentHandle {
    pub fn send(&self, request: AgentRequest) -> Result<(), AgentError> {
        self.requests.try_send(request).map_err(|_| {
            anyhow::anyhow!("pi is no longer connected. Restart Rockstar to start a new session.")
        })
    }
}

pub struct AgentConnection {
    pub handle: AgentHandle,
    pub events: Receiver<AgentEvent>,
}
