use gpui_kit::{
    AppContext as _, Context, Entity, FocusHandle, Font, FontFallbacks, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, Styled as _, Subscription, Window,
    component::{ActiveTheme as _, WindowExt as _, notification::Notification},
    div,
};

use crate::{
    agent::{AgentConnection, AgentHandle, AgentRequest},
    chat::ChatView,
    command_palette::{Command, CommandPalette},
    interaction::{Interaction, InteractionMode},
};

pub(crate) struct AppView {
    interaction: Entity<Interaction>,
    normal_focus: FocusHandle,
    normal_active: bool,
    agent: AgentHandle,
    chat: Entity<ChatView>,
    palette: Entity<CommandPalette>,
    _key_subscription: Subscription,
}

impl AppView {
    pub(crate) fn new(
        connection: AgentConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
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
