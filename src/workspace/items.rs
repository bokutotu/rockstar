//! Retained workspace contents and the single list used to select them.
use super::*;
use gpui_kit::base::Tab;

#[derive(Clone)]
pub(super) enum OpenItem {
    Chat(Entity<ChatSession>),
    File(Entity<FileBuffer>),
    Terminal(Entity<crate::terminal::TerminalSession>),
}

impl OpenItem {
    fn id(&self) -> EntityId {
        match self {
            Self::Chat(session) => session.entity_id(),
            Self::File(buffer) => buffer.entity_id(),
            Self::Terminal(session) => session.entity_id(),
        }
    }

    fn label(&self, root: &Path, cx: &App) -> String {
        match self {
            Self::Chat(session) => format!("Chat {}", session.read(cx).number),
            Self::Terminal(session) => session.read(cx).label(),
            Self::File(buffer) => {
                let buffer = buffer.read(cx);
                format!(
                    "{}{}",
                    buffer
                        .path
                        .strip_prefix(root)
                        .unwrap_or(&buffer.path)
                        .display(),
                    if buffer.dirty() { " ●" } else { "" },
                )
            }
        }
    }
}

impl Workspace {
    pub(super) fn buffers(&self) -> impl Iterator<Item = &Entity<FileBuffer>> {
        self.items.iter().filter_map(|item| match item {
            OpenItem::File(buffer) => Some(buffer),
            OpenItem::Chat(_) | OpenItem::Terminal(_) => None,
        })
    }

    pub(super) fn find_buffer(&self, path: &Path, cx: &App) -> Option<Entity<FileBuffer>> {
        self.buffers()
            .find(|buffer| buffer.read(cx).path == path)
            .cloned()
    }

    pub(super) fn register_item(&mut self, item: OpenItem, cx: &mut Context<Self>) -> EntityId {
        let id = item.id();
        if !self.items.iter().any(|entry| entry.id() == id) {
            if let OpenItem::File(buffer) = &item {
                // The item bar must update even when no pane displays this buffer.
                cx.observe(buffer, |_, _, cx| cx.notify()).detach();
            }
            if let OpenItem::Terminal(session) = &item {
                let mut label = session.read(cx).label();
                cx.observe(session, move |_, session, cx| {
                    let next = session.read(cx).label();
                    if next != label {
                        label = next;
                        cx.notify();
                    }
                }).detach();
            }
            self.items.push(item);
            cx.notify();
        }
        id
    }

    fn next_item(&self, current: Option<EntityId>, forward: bool) -> Option<EntityId> {
        let index = self
            .items
            .iter()
            .position(|item| Some(item.id()) == current);
        let next = index.map_or(0, |index| cycle_index(index, self.items.len(), forward));
        self.items.get(next).map(OpenItem::id)
    }

    pub(super) fn reveal_active_item(&self, cx: &App) {
        let current = self.panes[self.active].read(cx).item_id(cx);
        if let Some(index) = self
            .items
            .iter()
            .position(|item| Some(item.id()) == current)
        {
            self.item_scroll.scroll_to_item(index);
        }
    }

    pub(super) fn show_item(
        &mut self,
        pane_id: EntityId,
        item_id: EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pane) = self
            .panes
            .iter()
            .find(|pane| pane.entity_id() == pane_id)
            .cloned()
        else {
            return;
        };
        let Some(item) = self.items.iter().find(|item| item.id() == item_id).cloned() else {
            return;
        };
        pane.update(cx, |pane, cx| {
            // Explicit selection also invalidates any earlier load/connection.
            pane.request += 1;
            match item {
                OpenItem::Chat(session) => pane.show_chat(session, &self.root, window, cx),
                OpenItem::File(buffer) => pane.show_file(buffer, window, cx),
                OpenItem::Terminal(session) => pane.show_terminal(session, window, cx),
            }
        });
        if self.panes[self.active] == pane {
            self.reveal_active_item(cx);
            if !cx.global::<Mode>().is_overlay() {
                self.focus_active(window, cx);
            }
        }
        cx.notify();
    }

    pub(super) fn cycle(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let pane = &self.panes[self.active];
        let pane_id = pane.entity_id();
        if let Some(next) = self.next_item(pane.read(cx).item_id(cx), forward) {
            self.show_item(pane_id, next, window, cx);
        }
    }

    pub(super) fn render_item_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.panes[self.active].read(cx).item_id(cx);
        let theme = cx.theme();
        div()
            .id("open-items")
            .test_support()
            .role(Role::TabList)
            .aria_label("Open chats, files, and terminals")
            .w_full()
            .h(px(32.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .overflow_x_scroll()
            .track_scroll(&self.item_scroll)
            .bg(theme.muted)
            .border_b_1()
            .border_color(theme.border)
            .when(self.items.is_empty(), |bar| {
                bar.child(
                    div()
                        .px_3()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("No open chats, files, or terminals"),
                )
            })
            .children(self.items.iter().enumerate().map(|(index, item)| {
                let id = item.id();
                let selected = Some(id) == current;
                let label = item.label(&self.root, cx);
                Tab::new(("open-item", id.as_u64()))
                    .selected(selected)
                    .accessibility_label(label.clone())
                    .set_position(index + 1, self.items.len())
                    .h_full()
                    .flex_shrink_0()
                    .px_3()
                    .text_sm()
                    .cursor_pointer()
                    .bg(if selected {
                        theme.background
                    } else {
                        theme.muted
                    })
                    .text_color(if selected {
                        theme.foreground
                    } else {
                        theme.muted_foreground
                    })
                    .border_b_2()
                    .border_color(if selected { theme.primary } else { theme.muted })
                    .hover(|style| style.bg(theme.accent))
                    .child(label)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if matches!(
                            *cx.global::<Mode>(),
                            Mode::CommandPalette | Mode::FileFinder | Mode::Confirm
                        ) {
                            return;
                        }
                        this.pending.clear();
                        cx.set_global(Mode::Normal);
                        this.show_item(this.panes[this.active].entity_id(), id, window, cx);
                    }))
            }))
    }
}
