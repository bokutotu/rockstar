use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use async_channel::Sender;
use gpui_kit::{
    AnyWindowHandle, App, HeadlessAppContext, TitlebarOptions, WindowBounds, WindowOptions,
    assets::Assets,
    component::{Theme, ThemeMode},
    px, size,
};

use crate::{
    agent::{self, Agent, AgentBackend, AgentEvent, Message},
    app::open_dock_window,
    init_key_handling,
    workspace::AgentConnector,
};

pub static RENDER_LOCK: Mutex<()> = Mutex::new(());

pub fn mock_agent() -> (Agent, AgentBackend) {
    agent::channel()
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
    events.try_send(AgentEvent::PromptFinished).unwrap();
}

pub fn headless_app() -> HeadlessAppContext {
    // All UI tests need the same native font inventory: GPUI's default
    // monospace font is cached process-wide, even across separate Apps.
    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(|cx| {
        gpui_kit::init(cx);
        init_key_handling(cx);
        Theme::change(ThemeMode::Light, None, cx);
        Theme::update(cx, |theme| {
            theme.font_family = "JetBrainsMono Nerd Font".into();
            theme.font_size = px(11.0);
            theme.mono_font_family = theme.font_family.clone();
            theme.mono_font_size = theme.font_size;
        });
    });
    cx
}

pub(crate) fn open_app_window(agent: Agent, cx: &mut App) -> gpui_kit::Result<AnyWindowHandle> {
    open_app_window_in(agent, PathBuf::from(env!("CARGO_MANIFEST_DIR")), cx)
}

pub(crate) fn open_app_window_in(
    agent: Agent,
    workspace: PathBuf,
    cx: &mut App,
) -> gpui_kit::Result<AnyWindowHandle> {
    open_app_window_with_connector(
        agent,
        workspace,
        Arc::new(|_| anyhow::bail!("unexpected agent connection")),
        cx,
    )
}

pub(crate) fn open_app_window_with_connector(
    agent: Agent,
    workspace: PathBuf,
    connect: AgentConnector,
    cx: &mut App,
) -> gpui_kit::Result<AnyWindowHandle> {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(960.0), px(720.0)), cx)),
        window_min_size: Some(size(px(360.0), px(240.0))),
        titlebar: Some(TitlebarOptions {
            title: Some("Rockstar — pi".into()),
            ..Default::default()
        }),
        app_id: Some("rockstar".into()),
        ..Default::default()
    };
    open_dock_window(options, agent, workspace, connect, cx)
}
