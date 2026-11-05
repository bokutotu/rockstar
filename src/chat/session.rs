use async_channel::Sender;
use gpui_kit::{
    Context, EventEmitter, SharedString, Task, Window,
    component::{WindowExt as _, notification::Notification},
};

use crate::agent::{Agent, AgentEvent, Message, PromptRequest};

/// A conversation and draft shared by every pane displaying this session.
/// The single receiver lives here, not in a view: splitting must not distribute
/// incoming events between competing consumers.
pub(crate) struct ChatSession {
    pub(crate) number: usize,
    pub(super) messages: Vec<Message>,
    pub(super) draft: SharedString,
    pub(super) prompts: Sender<PromptRequest>,
    assistant: Option<usize>,
    _events: Task<()>,
}

pub(super) enum ConversationChange {
    Append(Message),
    AppendText(usize, String),
    Replace(usize, Message),
    Remove(usize),
}

impl EventEmitter<ConversationChange> for ChatSession {}

impl ChatSession {
    pub(crate) fn new(
        mut agent: Agent,
        number: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let events = agent.take_results().expect("session owns agent events");
        let task = cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = events.recv().await {
                if this
                    .update_in(cx, |this, window, cx| this.apply_event(event, window, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            number,
            messages: Vec::new(),
            draft: "".into(),
            prompts: agent.prompt_sender(),
            assistant: None,
            _events: task,
        }
    }

    pub(super) fn set_draft(&mut self, draft: SharedString, cx: &mut Context<Self>) {
        if self.draft != draft {
            self.draft = draft;
            cx.notify();
        }
    }

    fn append(&mut self, message: Message, cx: &mut Context<Self>) {
        self.messages.push(message.clone());
        cx.emit(ConversationChange::Append(message));
    }

    fn apply_event(&mut self, event: AgentEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            AgentEvent::UserMessage(text) => self.append(Message::User(text), cx),
            AgentEvent::AssistantStarted => self.assistant = None,
            AgentEvent::AssistantTextDelta(text) => {
                if text.is_empty() {
                    return;
                }
                if let Some(index) = self.assistant {
                    if let Message::Assistant(content) = &mut self.messages[index] {
                        content.push_str(&text);
                    }
                    cx.emit(ConversationChange::AppendText(index, text));
                } else {
                    self.assistant = Some(self.messages.len());
                    self.append(Message::Assistant(text), cx);
                }
            }
            AgentEvent::AssistantFinished(text) => {
                if let Some(index) = self.assistant.take() {
                    if text.is_empty() {
                        self.messages.remove(index);
                        cx.emit(ConversationChange::Remove(index));
                    } else {
                        let message = Message::Assistant(text);
                        self.messages[index] = message.clone();
                        cx.emit(ConversationChange::Replace(index, message));
                    }
                } else if !text.is_empty() {
                    self.append(Message::Assistant(text), cx);
                }
            }
            AgentEvent::PromptFinished => {}
            AgentEvent::Error(error) => {
                window
                    .push_notification(Notification::error(error.to_string()).autohide(false), cx);
            }
        }
    }
}
