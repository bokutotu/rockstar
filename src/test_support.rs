use std::sync::{Arc, Mutex};

use async_channel::{Receiver, Sender};
use gpui_kit::{HeadlessAppContext, assets::Assets};

use crate::agent::{AgentConnection, AgentEvent, AgentHandle, AgentRequest, Message};

pub static RENDER_LOCK: Mutex<()> = Mutex::new(());

pub fn mock_agent() -> (AgentConnection, Receiver<AgentRequest>, Sender<AgentEvent>) {
    let (sender, requests) = async_channel::unbounded();
    let (events, receiver) = async_channel::unbounded();
    (
        AgentConnection {
            handle: AgentHandle { requests: sender },
            events: receiver,
        },
        requests,
        events,
    )
}

pub fn reply_with_messages(events: &Sender<AgentEvent>, messages: Vec<Message>) {
    for message in messages {
        match message {
            Message::User(text) => events.try_send(AgentEvent::UserMessage(text)).unwrap(),
            Message::Assistant(text) => {
                events.try_send(AgentEvent::AssistantStarted).unwrap();
                events
                    .try_send(AgentEvent::AssistantFinished(text))
                    .unwrap();
            }
        }
    }
    events.try_send(AgentEvent::PromptFinished(Ok(()))).unwrap();
}

pub fn headless_app() -> HeadlessAppContext {
    // All UI tests need the same native font inventory: GPUI's default
    // monospace font is cached process-wide, even across separate Apps.
    HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(Assets),
        gpui_kit::platform::current_headless_renderer,
    )
}
