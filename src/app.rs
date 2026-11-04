use gpui_kit::{
    AppContext as _, Context, Entity, FocusHandle, Font, FontFallbacks, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, Styled as _, Window, component::ActiveTheme as _, div,
};

use crate::{agent::Agent, chat::ChatView, command_palette::CommandPalette, mode::Mode};

pub(crate) struct AppView {
    normal_focus: FocusHandle,
    chat: Entity<ChatView>,
    palette: Entity<CommandPalette>,
}

impl AppView {
    pub(crate) fn new(mut agent: Agent, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let chat = cx.new(|cx| {
            ChatView::new(
                agent.prompt_sender(),
                agent
                    .take_results()
                    .expect("agent results belong to the chat"),
                window,
                cx,
            )
        });
        let palette = cx.new(|cx| CommandPalette::new(agent.new_session_sender(), window, cx));
        cx.observe_global_in::<Mode>(window, |this, window, cx| {
            if *cx.global::<Mode>() == Mode::Normal {
                this.normal_focus.focus(window, cx);
            }
        })
        .detach();
        let normal_focus = cx.focus_handle();
        if *cx.global::<Mode>() == Mode::Normal {
            normal_focus.focus(window, cx);
        }
        Self {
            normal_focus,
            chat,
            palette,
        }
    }
}

impl Render for AppView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
