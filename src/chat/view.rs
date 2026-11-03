use gpui_kit::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, Styled as _, Task, Window,
    component::{WindowExt as _, notification::Notification},
    div,
};

use super::{
    composer::{Composer, ComposerEvent, ComposerView},
    conversation::ConversationView,
};
use crate::{
    agent::{AgentConnection, AgentEvent, AgentHandle, AgentRequest, Message},
    interaction::Interaction,
};

pub struct ChatView {
    conversation: Entity<ConversationView>,
    composer: Entity<ComposerView>,
    agent: AgentHandle,
    _events_task: Task<()>,
}

impl ChatView {
    pub(crate) fn new(
        connection: AgentConnection,
        interaction: Entity<Interaction>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let conversation = cx.new(|_| ConversationView::new());
        let composer = cx.new(|cx| ComposerView::new(interaction, window, cx));
        cx.subscribe_in(&composer, window, |this, _, event, window, cx| {
            let ComposerEvent::SubmitRequested(text) = event;
            this.submit(text.clone(), window, cx);
        })
        .detach();
        let AgentConnection { handle, events } = connection;
        let events_task = cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = events.recv().await {
                if this
                    .update_in(cx, |this, window, cx| {
                        this.handle_agent_event(event, window, cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            conversation,
            composer,
            agent: handle,
            _events_task: events_task,
        }
    }

    fn submit(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        if text.trim().is_empty() {
            return;
        }
        match self.agent.send(AgentRequest::Prompt(text)) {
            Ok(()) => self
                .composer
                .update(cx, |composer, cx| composer.clear(window, cx)),
            Err(error) => {
                window.push_notification(Notification::error(error.to_string()).autohide(false), cx)
            }
        }
    }

    fn handle_agent_event(
        &mut self,
        event: AgentEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            AgentEvent::UserMessage(text) => self.conversation.update(cx, |conversation, cx| {
                conversation.append(Message::User(text), cx);
            }),
            AgentEvent::AssistantStarted => self.conversation.update(cx, |conversation, _| {
                conversation.start_assistant();
            }),
            AgentEvent::AssistantTextDelta(text) => {
                self.conversation.update(cx, |conversation, cx| {
                    conversation.append_assistant(text, cx);
                });
            }
            AgentEvent::AssistantFinished(text) => {
                self.conversation.update(cx, |conversation, cx| {
                    conversation.finish_assistant(text, cx);
                });
            }
            AgentEvent::PromptFinished(Ok(())) => {}
            AgentEvent::NewSessionFinished(Ok(())) => {
                self.conversation
                    .update(cx, |conversation, cx| conversation.clear(cx));
                self.composer
                    .update(cx, |composer, cx| composer.clear(window, cx));
            }
            AgentEvent::PromptFinished(Err(error))
            | AgentEvent::NewSessionFinished(Err(error))
            | AgentEvent::Disconnected(error) => {
                window.push_notification(Notification::error(error.to_string()).autohide(false), cx)
            }
        }
    }
}

impl Render for ChatView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("chat")
            .size_full()
            .flex()
            .flex_col()
            .child(self.conversation.clone())
            .child(Composer::new(&self.composer))
    }
}
