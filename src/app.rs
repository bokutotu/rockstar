use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Context, Entity, FocusHandle, Font, FontFallbacks,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, Styled as _, Subscription,
    TitlebarOptions, Window, WindowBounds, WindowOptions,
    component::{ActiveTheme as _, Theme, ThemeMode, WindowExt as _, notification::Notification},
    div, px, size,
};

use crate::{
    agent::{AgentConnection, AgentHandle, AgentRequest},
    chat::ChatView,
    command_palette::{Command, CommandPalette},
    interaction::{Interaction, InteractionMode},
};

struct AppView {
    interaction: Entity<Interaction>,
    normal_focus: FocusHandle,
    normal_active: bool,
    agent: AgentHandle,
    chat: Entity<ChatView>,
    palette: Entity<CommandPalette>,
    _key_subscription: Subscription,
}

impl AppView {
    fn new(connection: AgentConnection, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let interaction = cx.new(|_| Interaction::new());
        let agent = connection.handle.clone();
        let chat = cx.new(|cx| ChatView::new(connection, interaction.clone(), window, cx));
        let palette = cx.new(|cx| CommandPalette::new(interaction.clone(), window, cx));
        let key_subscription = cx.intercept_keystrokes({
            let interaction = interaction.clone();
            let window_handle = window.window_handle();
            move |event, window, cx| {
                // Intercept before keybindings, regardless of the focused view.
                if window.window_handle() == window_handle
                    && interaction.update(cx, |interaction, cx| {
                        interaction.handle_key(&event.keystroke, cx)
                    })
                {
                    cx.stop_propagation();
                }
            }
        });
        cx.observe_in(&interaction, window, |this, interaction, window, cx| {
            this.apply_mode(interaction.read(cx).mode(), window, cx);
        })
        .detach();
        cx.subscribe_in(&palette, window, |this, _, command, window, cx| {
            this.handle_command(command, window, cx);
        })
        .detach();
        let mode = interaction.read(cx).mode();
        let mut view = Self {
            interaction,
            normal_focus: cx.focus_handle(),
            normal_active: false,
            agent,
            chat,
            palette,
            _key_subscription: key_subscription,
        };
        view.apply_mode(mode, window, cx);
        view
    }

    fn apply_mode(&mut self, mode: InteractionMode, window: &mut Window, cx: &mut Context<Self>) {
        self.normal_active = matches!(mode, InteractionMode::Normal);
        if self.normal_active {
            self.normal_focus.focus(window, cx);
        }
        cx.notify();
    }

    fn handle_command(&mut self, command: &Command, window: &mut Window, cx: &mut Context<Self>) {
        match command {
            Command::NewSession => match self.agent.send(AgentRequest::NewSession) {
                Ok(()) => self.interaction.update(cx, |interaction, cx| {
                    interaction.set_mode(InteractionMode::Normal, cx);
                }),
                Err(error) => window
                    .push_notification(Notification::error(error.to_string()).autohide(false), cx),
            },
        }
    }
}

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mode = self.interaction.read(cx).mode();
        if self.normal_active != matches!(mode, InteractionMode::Normal) {
            self.apply_mode(mode, window, cx);
        }
        div()
            .id("app")
            .track_focus(&self.normal_focus)
            .key_context("Rockstar")
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .font(Font {
                family: cx.theme().font_family.clone(),
                fallbacks: Some(FontFallbacks::from_fonts(vec![
                    "Noto Sans CJK JP".into(),
                    "Noto Color Emoji".into(),
                ])),
                ..Default::default()
            })
            .text_size(cx.theme().font_size)
            .child(self.chat.clone())
            .child(self.palette.clone())
    }
}

pub fn open_chat_window(
    agent: AgentConnection,
    cx: &mut App,
) -> gpui_kit::Result<(AnyWindowHandle, Entity<ChatView>)> {
    gpui_kit::init(cx);
    Theme::change(ThemeMode::Light, None, cx);
    Theme::update(cx, |theme| {
        theme.font_family = "JetBrainsMono Nerd Font".into();
        // Match Alacritty's 6 pt at 96 dpi.
        theme.font_size = px(8.0);
        theme.mono_font_family = theme.font_family.clone();
        theme.mono_font_size = theme.font_size;
    });
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
    let (window, app) = gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| AppView::new(agent, window, cx))
    })?;
    Ok((window, app.read(cx).chat.clone()))
}
