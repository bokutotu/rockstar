use async_channel::{Receiver, Sender};
use gpui_kit::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, Styled as _, Task, Window,
    component::{WindowExt as _, notification::Notification},
    div,
};

use super::{
    composer::{Composer, ComposerView},
    conversation::ConversationView,
};
use crate::{
    agent::{AgentEvent, Message, PromptRequest},
    interaction::Interaction,
};

pub struct ChatView {
    conversation: Entity<ConversationView>,
    composer: Entity<ComposerView>,
    _events_task: Task<()>,
}

impl ChatView {
    pub(crate) fn new(
        prompts: Sender<PromptRequest>,
        events: Receiver<AgentEvent>,
        interaction: Entity<Interaction>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let conversation = cx.new(|_| ConversationView::new());
        let composer = cx.new(|cx| ComposerView::new(prompts, interaction, window, cx));
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
            _events_task: events_task,
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
            AgentEvent::PromptFinished => {}
            AgentEvent::NewSessionFinished => {
                self.conversation
                    .update(cx, |conversation, cx| conversation.clear(cx));
                self.composer
                    .update(cx, |composer, cx| composer.clear(window, cx));
            }
            AgentEvent::Error(error) => {
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
