use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, ScrollHandle, StatefulInteractiveElement as _, Styled as _, Task,
    TitlebarOptions, Window, WindowBounds, WindowOptions,
    base::{TextView, TextViewState},
    component::{
        ActiveTheme as _, Disableable as _, Theme, ThemeMode, WindowExt as _,
        button::{Button, ButtonVariants as _},
        input::{Textarea, TextareaState},
        notification::Notification,
    },
    div, px, size,
};

use crate::{
    agent::{CodingAgent, Message},
    math::MathPlugin,
    mermaid::MermaidPlugin,
};

// GPUI rendering state for a single message.
struct MarkdownContent {
    document: Entity<TextViewState>,
    inline_math: MathPlugin,
    block_math: MathPlugin,
    mermaid: MermaidPlugin,
}

impl MarkdownContent {
    fn new(source: &str, cx: &mut App) -> Self {
        let document = cx.new(|cx| TextViewState::markdown(source, cx).selectable(true));
        Self {
            inline_math: MathPlugin::inline(&document),
            block_math: MathPlugin::block(&document),
            mermaid: MermaidPlugin::new(&document),
            document,
        }
    }

    fn view(&self) -> TextView {
        TextView::new(&self.document)
            .plugin(self.inline_math.clone())
            .plugin(self.block_math.clone())
            .plugin(self.mermaid.clone())
    }
}

enum ChatMessage {
    User(MarkdownContent),
    Assistant(MarkdownContent),
}

impl ChatMessage {
    fn new(message: Message, cx: &mut App) -> Self {
        match message {
            Message::User(text) => Self::User(MarkdownContent::new(&text, cx)),
            Message::Assistant(text) => Self::Assistant(MarkdownContent::new(&text, cx)),
        }
    }
}

pub struct ChatView<A: CodingAgent> {
    messages: Vec<ChatMessage>,
    input: Entity<TextareaState>,
    agent: Option<A>,
    request: Option<Task<()>>,
    scroll: ScrollHandle,
}

impl<A: CodingAgent> ChatView<A> {
    fn new(agent: A, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(3)
                .placeholder("Ask pi…")
        });
        cx.observe(&input, |_, _, cx| cx.notify()).detach();
        input.update(cx, |input, cx| input.focus(window, cx));
        Self {
            messages: Vec::new(),
            input,
            agent: Some(agent),
            request: None,
            scroll: ScrollHandle::new(),
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().to_string();
        if text.trim().is_empty() {
            return;
        }
        let Some(mut agent) = self.agent.take() else {
            return;
        };
        let response = cx.background_spawn(async move {
            let result = agent.prompt(text).await;
            (agent, result)
        });
        self.request = Some(cx.spawn_in(window, async move |this, cx| {
            let (agent, result) = response.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.agent = Some(agent);
                this.request = None;
                match result {
                    Ok(messages) => {
                        for message in messages {
                            this.messages.push(ChatMessage::new(message, cx));
                        }
                        this.input
                            .update(cx, |input, cx| input.set_value("", window, cx));
                        this.scroll.scroll_to_bottom();
                    }
                    Err(error) => window.push_notification(
                        Notification::error(error.to_string()).autohide(false),
                        cx,
                    ),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
}

impl<A: CodingAgent> Render for ChatView<A> {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.agent.is_none();
        let blank = self.input.read(cx).value().trim().is_empty();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .font_family("Noto Sans CJK JP")
            .text_size(px(16.0))
            .child(
                div()
                    .id("conversation")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .p_6()
                    .child(div().flex().flex_col().gap_5().children(
                        self.messages.iter().enumerate().map(|(index, message)| {
                            let (label, markdown) = match message {
                                ChatMessage::User(markdown) => ("You", markdown),
                                ChatMessage::Assistant(markdown) => ("Assistant", markdown),
                            };
                            div()
                                .id(("message", index))
                                .w_full()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(
                                    div()
                                        .text_size(px(13.0))
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .text_color(cx.theme().muted_foreground)
                                        .child(label),
                                )
                                .child(markdown.view())
                        }),
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_end()
                    .gap_3()
                    .p_4()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        Textarea::new(&self.input)
                            .flex_1()
                            .readonly(busy)
                            .aria_label("Message to pi"),
                    )
                    .child(
                        Button::new("send")
                            .primary()
                            .label("Send")
                            .disabled(busy || blank)
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
    }
}

pub fn open_chat_window<A: CodingAgent>(
    agent: A,
    cx: &mut App,
) -> gpui_kit::Result<(AnyWindowHandle, Entity<ChatView<A>>)> {
    gpui_kit::init(cx);
    Theme::change(ThemeMode::Light, None, cx);
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
    gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| ChatView::new(agent, window, cx))
    })
}

#[cfg(test)]
#[path = "visual_tests.rs"]
mod visual_tests;
