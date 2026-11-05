use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use gpui_kit::{
    App, AppContext as _, Bounds, Context, Entity, Focusable as _, InteractiveElement as _,
    IntoElement, KeyBinding, ParentElement as _, Pixels, Render, Styled as _, Window, actions,
    base::Placement,
    canvas,
    component::{
        ActiveTheme as _, WindowExt as _,
        dock::{
            DockArea, DockLayout, DockPlacement, InsertTarget, PanelEvent, PanelId, panel_handle,
        },
        notification::Notification,
    },
    div, outline, px,
};

use crate::{
    agent::{Agent, AgentError},
    chat::{ChatPane, ChatSession, PaneActivated},
    mode::Mode,
};

mod dock;

pub(crate) type AgentConnector = Arc<dyn Fn(&Path) -> Result<Agent, AgentError> + Send + Sync>;

actions!(
    workspace,
    [
        SplitDown,
        SplitRight,
        FocusLeft,
        FocusDown,
        FocusUp,
        FocusRight,
        NextChat,
        PreviousChat
    ]
);

pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("s s", SplitDown, Some("WorkspaceNormal")),
        KeyBinding::new("s v", SplitRight, Some("WorkspaceNormal")),
        KeyBinding::new("s h", FocusLeft, Some("WorkspaceNormal")),
        KeyBinding::new("s j", FocusDown, Some("WorkspaceNormal")),
        KeyBinding::new("s k", FocusUp, Some("WorkspaceNormal")),
        KeyBinding::new("s l", FocusRight, Some("WorkspaceNormal")),
        KeyBinding::new("ctrl-n", NextChat, Some("WorkspaceNormal")),
        KeyBinding::new("ctrl-p", PreviousChat, Some("WorkspaceNormal")),
    ]);
}

pub(crate) struct Workspace {
    root: PathBuf,
    sessions: Vec<Entity<ChatSession>>,
    panes: Vec<Entity<ChatPane>>,
    active: usize,
    dock: Entity<DockArea>,
    connect: AgentConnector,
}

impl Workspace {
    pub(crate) fn new(
        agent: Agent,
        root: PathBuf,
        connect: AgentConnector,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let dock = dock::new(window, cx);
        let session = cx.new(|cx| ChatSession::new(agent, 1, window, cx));
        let mut workspace = Self {
            root,
            sessions: vec![session.clone()],
            panes: Vec::new(),
            active: 0,
            dock,
            connect,
        };
        let pane = workspace.add_pane(session, window, cx);
        workspace.dock.update(cx, |dock, cx| {
            dock.set_center(
                DockLayout::tabs().panel_view(panel_handle(pane), cx),
                window,
                cx,
            );
        });
        workspace
    }

    fn add_pane(
        &mut self,
        session: Entity<ChatSession>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<ChatPane> {
        let pane = cx.new(|cx| ChatPane::new(session, &self.root, window, cx));
        let index = self.panes.len();
        cx.on_focus_in(&pane.focus_handle(cx), window, move |this, _, cx| {
            this.active = index;
            cx.notify();
        })
        .detach();
        cx.subscribe(&pane, move |this, _, _: &PaneActivated, cx| {
            this.active = index;
            cx.notify();
        })
        .detach();
        self.panes.push(pane.clone());
        pane
    }

    pub(crate) fn focus_active(&self, window: &mut Window, cx: &mut App) {
        let pane = &self.panes[self.active];
        if *cx.global::<Mode>() == Mode::Insert {
            pane.update(cx, |pane, cx| pane.focus_input(window, cx));
        } else {
            pane.focus_handle(cx).focus(window, cx);
        }
    }

    fn show_titles(&self, cx: &mut App) {
        for pane in &self.panes {
            pane.update(cx, |pane, cx| {
                pane.show_title = true;
                cx.emit(PanelEvent::LayoutChanged);
                cx.notify();
            });
        }
    }

    fn split(&mut self, placement: Placement, window: &mut Window, cx: &mut Context<Self>) {
        let source = &self.panes[self.active];
        let node = self
            .dock
            .read(cx)
            .layout(DockPlacement::Center)
            .and_then(|tree| tree.find_panel_node(PanelId::from(source.entity_id())));
        let Some(node) = node else {
            return;
        };
        let session = source.read(cx).session.clone();
        let pane = self.add_pane(session, window, cx);
        self.dock.update(cx, |dock, cx| {
            dock.add_panel_view(
                panel_handle(pane.clone()),
                DockPlacement::Center,
                None,
                window,
                cx,
            );
            dock.move_panel(
                PanelId::from(pane.entity_id()),
                InsertTarget::Split {
                    node,
                    placement,
                    size: None,
                },
                window,
                cx,
            );
        });
        self.active = self.panes.len() - 1;
        self.show_titles(cx);
        self.focus_active(window, cx);
        cx.notify();
    }

    fn move_focus(&mut self, direction: Placement, window: &mut Window, cx: &mut Context<Self>) {
        let from = self.panes[self.active].read(cx).bounds.get();
        let next = self
            .panes
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != self.active)
            .filter_map(|(index, pane)| {
                let to = pane.read(cx).bounds.get();
                let (gap, overlap, distance) = match direction {
                    Placement::Left => (
                        from.left() - to.right(),
                        from.bottom().min(to.bottom()) - from.top().max(to.top()),
                        (from.center().y - to.center().y).abs(),
                    ),
                    Placement::Right => (
                        to.left() - from.right(),
                        from.bottom().min(to.bottom()) - from.top().max(to.top()),
                        (from.center().y - to.center().y).abs(),
                    ),
                    Placement::Top => (
                        from.top() - to.bottom(),
                        from.right().min(to.right()) - from.left().max(to.left()),
                        (from.center().x - to.center().x).abs(),
                    ),
                    Placement::Bottom => (
                        to.top() - from.bottom(),
                        from.right().min(to.right()) - from.left().max(to.left()),
                        (from.center().x - to.center().x).abs(),
                    ),
                };
                (f32::from(gap) >= -0.5 && f32::from(overlap) > 0.0).then_some((
                    index,
                    f32::from(gap),
                    f32::from(distance),
                ))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.2.total_cmp(&b.2)))
            .map(|(index, _, _)| index);
        if let Some(index) = next {
            self.active = index;
            self.focus_active(window, cx);
            cx.notify();
        }
    }

    fn cycle_chat(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let pane = &self.panes[self.active];
        let current = &pane.read(cx).session;
        let index = self
            .sessions
            .iter()
            .position(|session| session == current)
            .expect("pane session belongs to workspace");
        let count = self.sessions.len();
        let next = if forward {
            (index + 1) % count
        } else {
            (index + count - 1) % count
        };
        let session = self.sessions[next].clone();
        pane.update(cx, |pane, cx| {
            pane.show_session(session, &self.root, window, cx)
        });
        self.focus_active(window, cx);
        cx.notify();
    }

    pub(crate) fn new_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Keep the command's destination even if the user focuses another pane
        // while pi is starting. A failed connection does not change any session.
        let pane = self.panes[self.active].clone();
        let root = self.root.clone();
        let connect = self.connect.clone();
        let connecting = cx.background_spawn(async move { connect(&root) });
        cx.spawn_in(window, async move |this, cx| {
            let result = connecting.await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(agent) => {
                    let number = this.sessions.len() + 1;
                    let session = cx.new(|cx| ChatSession::new(agent, number, window, cx));
                    this.sessions.push(session.clone());
                    pane.update(cx, |pane, cx| {
                        pane.show_session(session, &this.root, window, cx)
                    });
                    this.show_titles(cx);
                    if this.panes[this.active] == pane
                        && *cx.global::<Mode>() != Mode::CommandPalette
                    {
                        this.focus_active(window, cx);
                    }
                    cx.notify();
                }
                Err(error) => window
                    .push_notification(Notification::error(error.to_string()).autohide(false), cx),
            });
        })
        .detach();
    }
}

fn paint_active_border(bounds: Bounds<Pixels>, window: &mut Window, cx: &App) {
    window
        .paint_quad(outline(bounds, cx.theme().border, Default::default()).border_widths(px(2.0)));
}

impl Render for Workspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active_bounds = self.panes[self.active].read(cx).bounds.clone();
        div()
            .size_full()
            .relative()
            .key_context(if *cx.global::<Mode>() == Mode::Normal {
                "WorkspaceNormal"
            } else {
                "Workspace"
            })
            .on_action(cx.listener(|this, _: &SplitDown, window, cx| {
                this.split(Placement::Bottom, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SplitRight, window, cx| {
                this.split(Placement::Right, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusLeft, window, cx| {
                this.move_focus(Placement::Left, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusDown, window, cx| {
                this.move_focus(Placement::Bottom, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusUp, window, cx| {
                this.move_focus(Placement::Top, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusRight, window, cx| {
                this.move_focus(Placement::Right, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &NextChat, window, cx| this.cycle_chat(true, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &PreviousChat, window, cx| {
                    this.cycle_chat(false, window, cx)
                }),
            )
            .child(self.dock.clone())
            // Paint after the dock, without changing layout or intercepting input.
            .child(
                canvas(
                    |_, _, _| {},
                    move |_, _, window, cx| paint_active_border(active_bounds.get(), window, cx),
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
    }
}
