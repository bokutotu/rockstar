use std::path::{Path, PathBuf};
use gpui_kit::{
    Context, Entity, InteractiveElement as _, IntoElement, Keystroke, ParentElement as _, Render,
    ScrollHandle, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use crate::{agent::Message, mode::Mode};

use super::{
    message::ChatMessage,
    session::{ChatSession, ConversationChange},
};

/// Markdown rendering and scroll state belong to a pane, not to the session.
pub(super) struct ConversationView {
    messages: Vec<ChatMessage>,
    scroll: ScrollHandle,
    /// None follows the latest message; a cursor pauses automatic scrolling.
    current: Option<usize>,
    pending_scroll: Option<usize>,
    base: PathBuf,
    visible: bool,
}

impl ConversationView {
    pub(super) fn new(session: &Entity<ChatSession>, base: &Path, cx: &mut Context<Self>) -> Self {
        cx.on_release(|this, cx| {
            for message in &this.messages { message.shutdown(cx); }
        }).detach();
        cx.observe_global::<Mode>(|_, cx| cx.notify()).detach();
        let messages: Vec<_> = session
            .read(cx)
            .messages
            .clone()
            .into_iter()
            .map(|message| ChatMessage::new(message, base, cx))
            .collect();
        cx.subscribe(session, |this, _, change, cx| {
            match change {
                ConversationChange::Reset(messages) => {
                    for message in &this.messages { message.shutdown(cx); }
                    this.messages = messages.iter().cloned().map(|message| {
                        let message = ChatMessage::new(message, &this.base, cx);
                        message.set_visible(this.visible, cx);
                        message
                    }).collect();
                    this.scroll = ScrollHandle::new();
                    this.current = None;
                    this.pending_scroll = None;
                }
                ConversationChange::Append(message) => {
                    let message = ChatMessage::new(message.clone(), &this.base, cx);
                    message.set_visible(this.visible, cx);
                    this.messages.push(message)
                }
                ConversationChange::AppendText(index, text) => {
                    this.messages[*index].append_text(text, cx)
                }
                ConversationChange::Replace(index, message) => {
                    let (Message::User(text) | Message::Assistant(text)) = message;
                    this.messages[*index].set_text(text, cx)
                }
                ConversationChange::Remove(index) => {
                    this.messages.remove(*index).shutdown(cx);
                    if let Some(current) = this.current
                        && *index <= current
                    {
                        this.current = this.messages.len().checked_sub(1).map(|last| {
                            if current > *index { current - 1 } else { current.min(last) }
                        });
                        if let Some(current) = this.current {
                            this.pending_scroll = Some(current);
                        }
                    }
                }
            }
            if this.current.is_none() {
                this.pending_scroll = Some(this.messages.len());
            }
            cx.notify();
        })
        .detach();
        let scroll = ScrollHandle::new();
        let pending_scroll = Some(messages.len());
        Self {
            messages,
            scroll,
            current: None,
            pending_scroll,
            base: base.into(),
            visible: true,
        }
    }

    pub(super) fn current_message(&self) -> Option<usize> {
        self.current.or_else(|| self.messages.len().checked_sub(1))
    }

    pub(super) fn move_message(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(current) = self.current_message() else { return; };
        let next = if forward {
            (current + 1).min(self.messages.len() - 1)
        } else {
            current.saturating_sub(1)
        };
        self.current = Some(next);
        self.pending_scroll = Some(next);
        cx.notify();
    }

    pub(super) fn follow_latest(&mut self, cx: &mut Context<Self>) {
        self.current = None;
        self.pending_scroll = Some(self.messages.len());
        cx.notify();
    }

    pub(super) fn scroll_to_edge(&mut self, bottom: bool, cx: &mut Context<Self>) {
        if bottom {
            self.follow_latest(cx);
        } else {
            self.current = (!self.messages.is_empty()).then_some(0);
            self.pending_scroll = Some(0);
            cx.notify();
        }
    }

    pub(super) fn scroll_key(&mut self, key: &Keystroke, cx: &mut Context<Self>) -> bool {
        if *cx.global::<Mode>() != Mode::Normal
            || key.modifiers.alt || key.modifiers.platform || key.modifiers.shift
        {
            return false;
        }
        let height = self.scroll.bounds().size.height.max(px(24.));
        let delta = match (key.modifiers.control, key.key.as_str()) {
            (false, "up") => px(24.),
            (false, "down") => -px(24.),
            (false, "pageup") | (true, "b") => height * 0.9,
            (false, "pagedown") | (true, "f") => -height * 0.9,
            (true, "u") => height * 0.5,
            (true, "d") => -height * 0.5,
            (false, "home" | "end") => {
                self.scroll_to_edge(key.key == "end", cx);
                return true;
            }
            _ => return false,
        };
        // Cancel a queued follow request before it reaches the next layout pass.
        self.pending_scroll = None;
        let mut offset = self.scroll.offset();
        offset.y = (offset.y + delta).clamp(-self.scroll.max_offset().y, px(0.));
        self.scroll.set_offset(offset);
        self.pause_following();
        cx.notify();
        true
    }

    fn pause_following(&mut self) {
        self.pending_scroll = None;
        self.current = self.messages.len().checked_sub(1)
            .map(|last| self.scroll.top_item().min(last));
    }

    pub(super) fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.visible = visible;
        for message in &self.messages { message.set_visible(visible, cx); }
    }
}

impl Render for ConversationView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(index) = self.pending_scroll.take() {
            self.scroll.scroll_to_top_of_item(index);
        }
        let current = (*cx.global::<Mode>() == Mode::Normal)
            .then(|| self.current_message())
            .flatten();
        div()
            .id("conversation")
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_5()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .on_scroll(cx.listener(|this, _, _, cx| {
                this.pause_following();
                cx.notify();
            }))
            .p_6()
            .children(
                self.messages
                    .iter()
                    .enumerate()
                    .map(|(index, message)| message.view(index, current == Some(index), cx)),
            )
            // A tail anchor lets following and navigation replace the same pending
            // scroll request, so a queued new-message scroll cannot override j/k.
            .child(div().h_0().flex_shrink_0())
    }
}
