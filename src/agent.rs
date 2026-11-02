use std::future::Future;

pub type AgentError = anyhow::Error;

#[derive(Clone, Debug)]
pub enum Message {
    User(String),
    Assistant(String),
}

pub trait CodingAgent: Send + 'static {
    /// Returns the newly confirmed messages after this request completes,
    /// not the full conversation or streaming updates.
    fn prompt(
        &mut self,
        input: String,
    ) -> impl Future<Output = Result<Vec<Message>, AgentError>> + Send;
}
