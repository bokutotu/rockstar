//! Retained workspace contents and the single list used to select them.
use super::*;
use gpui_kit::base::Tab as ItemTab;

#[derive(Clone)]
pub(super) enum OpenItem {
    Chat(Entity<ChatSession>),
    File(Entity<FileBuffer>),
    Terminal(Entity<crate::terminal::TerminalSession>),
    Media(Entity<crate::media::MediaSession>),
    Diff(Entity<crate::diff::DiffSession>),
}

pub(crate) enum WipeMode {
    Safe,
    Force,
}

impl OpenItem {
    fn check_wipe(&self, mode: WipeMode, cx: &App) -> anyhow::Result<()> {
        if let Self::File(buffer) = self {
            let buffer = buffer.read(cx);
            if buffer.saving {
                anyhow::bail!("File is being saved. Please wait before wiping it.");
            }
            if matches!(mode, WipeMode::Safe) && buffer.dirty() {
                anyhow::bail!("File has unsaved changes. Save it or use :bw! to discard them.");
            }
        }
        Ok(())
    }

    pub(super) fn id(&self) -> EntityId {
        match self {
            Self::Chat(session) => session.entity_id(),
            Self::File(buffer) => buffer.entity_id(),
            Self::Terminal(session) => session.entity_id(),
            Self::Media(session) => session.entity_id(),
            Self::Diff(session) => session.entity_id(),
        }
    }

    pub(super) fn label(&self, root: &Path, cx: &App) -> String {
        match self {
            Self::Chat(session) => format!("Chat {}", session.read(cx).number),
            Self::Terminal(session) => session.read(cx).label(),
            Self::Diff(session) => session.read(cx).label(),
            Self::Media(session) => match &session.read(cx).source {
                crate::media::MediaSource::File(path) => path.strip_prefix(root).unwrap_or(path).display().to_string(),
                source => source.label(),
            },
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

impl Tab {
    pub(crate) fn wipe_active_item(
        &mut self,
        mode: WipeMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        let current = self.panes[self.active].read(cx).item_id(cx);
        let Some(index) = self.items.iter().position(|item| Some(item.id()) == current) else {
            anyhow::bail!("This pane has no item to wipe.");
        };
        self.items[index].check_wipe(mode, cx)?;
        let replacement = self.next_item(current, true).filter(|id| Some(*id) != current);
        let item = self.items.remove(index);
        let id = item.id();
        let mut affected = Vec::new();
        for pane in &self.panes {
            if pane.update(cx, |pane, cx| pane.forget_item(id, cx)) {
                affected.push(pane.entity_id());
            }
        }
        if let OpenItem::File(buffer) = &item {
            self.lsp.update(cx, |lsp, cx| lsp.unbind(buffer, cx));
        }
        if let OpenItem::Media(session) = &item {
            session.update(cx, |session, cx| session.shutdown(cx));
        }
        if let Some(replacement) = replacement {
            for pane_id in affected {
                self.show_item(pane_id, replacement, window, cx);
            }
        }
        self.pending.clear();
        self.reveal_active_item(cx);
        if !cx.global::<Mode>().is_overlay() {
            self.focus_active(window, cx);
        }
        cx.notify();
        Ok(())
    }

    pub(super) fn buffers(&self) -> impl Iterator<Item = &Entity<FileBuffer>> {
        self.items.iter().filter_map(|item| match item {
            OpenItem::File(buffer) => Some(buffer),
            OpenItem::Chat(_) | OpenItem::Terminal(_) | OpenItem::Media(_) | OpenItem::Diff(_) => None,
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
                self.watches.update(cx, |watches, cx| watches.attach(buffer, cx));
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
                OpenItem::Media(session) => pane.show_media(session, window, cx),
                OpenItem::Diff(session) => pane.show_diff(session, cx),
            }
            pane.set_content_visible(self.visible, cx);
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
            .role(Role::TabList)
            .aria_label("Open chats, files, media, terminals, and diffs")
            .w_full()
            .flex_shrink_0()
            .flex()
            .items_stretch()
            .text_sm()
            .line_height(relative(1.))
            .whitespace_nowrap()
            .overflow_x_scroll()
            .track_scroll(&self.item_scroll)
            .bg(theme.muted)
            .when(self.items.is_empty(), |bar| {
                bar.child(
                    div()
                        .flex_shrink_0()
                        .px_3()
                        .border_b_1()
                        .border_color(theme.border)
                        .text_color(theme.muted_foreground)
                        .child("No open chats, files, media, terminals, or diffs"),
                )
            })
            .children(self.items.iter().enumerate().map(|(index, item)| {
                let id = item.id();
                let selected = Some(id) == current;
                let label = item.label(&self.root, cx);
                ItemTab::new(("open-item", id.as_u64()))
                    .selected(selected)
                    .accessibility_label(label.clone())
                    .set_position(index + 1, self.items.len())
                    .flex_shrink_0()
                    .px_3()
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
                    .border_b_1()
                    .border_color(if selected { theme.primary } else { theme.border })
                    .hover(|style| style.bg(theme.accent))
                    .child(label)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if matches!(
                            *cx.global::<Mode>(),
                            Mode::CommandPalette | Mode::FileFinder | Mode::Switcher | Mode::Confirm
                        ) {
                            return;
                        }
                        this.pending.clear();
                        cx.set_global(Mode::Normal);
                        this.show_item(this.panes[this.active].entity_id(), id, window, cx);
                    }))
            }))
            // Continue the same 1px border across unused space, without adding
            // another border below the tabs and increasing the bar's height.
            .child(div().flex_1().border_b_1().border_color(theme.border))
    }
}
