use async_channel::{Receiver, Sender};

pub type AgentError = anyhow::Error;

pub const DISCONNECTED_MESSAGE: &str =
    "pi is no longer connected. Use :new to start a new chat.";

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
pub enum AgentEvent {
    UserMessage(String),
    AssistantStarted,
    AssistantTextDelta(String),
    AssistantFinished(String),
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
