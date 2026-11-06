use async_channel::Sender;
use gpui_kit::{
    Context, EventEmitter, SharedString, Task, Window,
    component::{WindowExt as _, notification::Notification},
};

use crate::agent::{AgentEvent, ContextUsage, Message, OpenedChat, PromptRequest};

/// A conversation and draft shared by every pane displaying this session.
/// The single receiver lives here, not in a view: splitting must not distribute
/// incoming events between competing consumers.
pub(crate) struct ChatSession {
    pub(crate) number: usize,
    pub(super) messages: Vec<Message>,
    pub(super) draft: SharedString,
    /// Latest tool operation for the current response; never part of history.
    pub(super) activity: Option<String>,
    pub(super) context_usage: Option<ContextUsage>,
    pub(super) prompts: Sender<PromptRequest>,
    assistant: Option<usize>,
    _events: Task<()>,
}

pub(super) enum ConversationChange {
    Reset(Vec<Message>),
    Append(Message),
    AppendText(usize, String),
    Replace(usize, Message),
    Remove(usize),
}

impl EventEmitter<ConversationChange> for ChatSession {}

impl ChatSession {
    pub(crate) fn new(
        opened: OpenedChat,
        number: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let OpenedChat {
            mut agent,
            messages,
        } = opened;
        let events = agent.take_results().expect("session owns agent events");
        let task = cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = events.recv().await {
                if this
                    .update_in(cx, |this, window, cx| this.apply_event(event, window, cx))
                    .is_err()
                {
                    return;
                }
            }
            let _ = this.update_in(cx, |this, _, cx| {
                this.set_activity(None, cx);
                this.set_context_usage(None, cx);
            });
        });
        Self {
            number,
            messages,
            draft: "".into(),
            activity: None,
            context_usage: None,
            prompts: agent.prompt_sender(),
            assistant: None,
            _events: task,
        }
    }

    /// Keep this session's identity while replacing its conversation and connection.
    pub(crate) fn replace(
        &mut self,
        opened: OpenedChat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Dropping the old receiver task prevents late replies from entering the new chat.
        *self = Self::new(opened, self.number, window, cx);
        cx.emit(ConversationChange::Reset(self.messages.clone()));
        cx.notify();
    }

    pub(super) fn set_draft(&mut self, draft: SharedString, cx: &mut Context<Self>) {
        if self.draft != draft {
            self.draft = draft;
            cx.notify();
        }
    }

    fn set_activity(&mut self, value: Option<String>, cx: &mut Context<Self>) {
        if self.activity != value {
            self.activity = value;
            cx.notify();
        }
    }

    fn set_context_usage(&mut self, usage: Option<ContextUsage>, cx: &mut Context<Self>) {
        if self.context_usage != usage {
            self.context_usage = usage;
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
            AgentEvent::ToolActivity(text) => self.set_activity(Some(text), cx),
            AgentEvent::ContextUsageUpdated(usage) => self.set_context_usage(usage, cx),
            AgentEvent::PromptFinished => self.set_activity(None, cx),
            AgentEvent::Error(error) => {
                self.set_activity(None, cx);
                window
                    .push_notification(Notification::error(error.to_string()).autohide(false), cx);
            }
        }
    }
}
