use std::{cell::Cell, path::Path, rc::Rc};

use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, Context, Entity, IntoElement, ParentElement as _,
    Render, Styled as _, TestAppContext, Window, WindowBounds, WindowOptions, div, point, px, size,
    test::{ElementSnapshot, TestWindowExt as _},
};

use crate::{
    agent::{self, AgentBackend, AgentEvent, AgentKind, OpenedChat},
    mode,
};

use super::{ChatPane, ChatSession};

struct SplitChat {
    session: Entity<ChatSession>,
    panes: [Entity<ChatPane>; 2],
}

impl Render for SplitChat {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().flex().children(
            self.panes
                .iter()
                .map(|pane| div().flex_1().min_w_0().h_full().child(pane.clone())),
        )
    }
}

fn open_chat(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<ChatSession>, AgentBackend) {
    let (agent, backend) = agent::channel();
    let (window, session) = cx.update(|cx| {
        gpui_kit::init(cx);
        mode::init(cx);
        let (window, split) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(640.), px(480.)),
                ))),
                ..Default::default()
            },
            cx,
            |window, cx| {
                cx.new(|cx| {
                    let session = cx.new(|cx| {
                        ChatSession::new(
                            OpenedChat {
                                agent,
                                messages: Vec::new(),
                            },
                            AgentKind::Pi,
                            1,
                            window,
                            cx,
                        )
                    });
                    let panes = std::array::from_fn(|_| {
                        cx.new(|cx| ChatPane::new(session.clone(), Path::new("."), window, cx))
                    });
                    SplitChat { session, panes }
                })
            },
        )
        .unwrap();
        (window, split.read(cx).session.clone())
    });
    (window, session, backend)
}

fn send(backend: &AgentBackend, event: AgentEvent, cx: &mut TestAppContext) {
    backend.events.try_send(event).unwrap();
    cx.run_until_parked();
}

fn activity_rows(window: AnyWindowHandle, cx: &mut TestAppContext) -> Vec<ElementSnapshot> {
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.find_all("chat-activity")
    })
    .unwrap()
}

fn assert_activity(window: AnyWindowHandle, text: &str, cx: &mut TestAppContext) {
    let rows = activity_rows(window, cx);
    assert_eq!(rows.len(), 2, "one row per pane");
    for row in rows {
        assert!(row.visible());
        assert_eq!(row.label(), Some(text));
    }
}

#[gpui_kit::test]
fn latest_activity_is_shared_without_becoming_history(cx: &mut TestAppContext) {
    let (window, session, backend) = open_chat(cx);
    assert!(activity_rows(window, cx).is_empty());
    let notifications = Rc::new(Cell::new(0));
    cx.update(|cx| {
        let notifications = notifications.clone();
        cx.observe(&session, move |_, _| {
            notifications.set(notifications.get() + 1)
        })
        .detach();
    });

    for text in ["read src/main.rs", "edit src/main.rs", "bash cargo check"] {
        send(&backend, AgentEvent::ToolActivity(text.into()), cx);
        assert_activity(window, text, cx);
        session.read_with(cx, |session, _| {
            assert_eq!(session.activity.as_deref(), Some(text));
            assert!(session.messages.is_empty());
        });
    }
    assert_eq!(notifications.get(), 3);
    send(
        &backend,
        AgentEvent::ToolActivity("bash cargo check".into()),
        cx,
    );
    assert_eq!(notifications.get(), 3, "unchanged activity does not notify");

    send(&backend, AgentEvent::AssistantStarted, cx);
    send(&backend, AgentEvent::AssistantFinished(String::new()), cx);
    assert_activity(window, "bash cargo check", cx);
    send(&backend, AgentEvent::PromptFinished, cx);
    assert!(activity_rows(window, cx).is_empty());
    session.read_with(cx, |session, _| assert!(session.messages.is_empty()));
}

#[gpui_kit::test]
fn errors_and_disconnects_clear_activity(cx: &mut TestAppContext) {
    let (window, session, backend) = open_chat(cx);
    send(
        &backend,
        AgentEvent::ToolActivity("bash cargo check".into()),
        cx,
    );
    assert_activity(window, "bash cargo check", cx);
    send(
        &backend,
        AgentEvent::Error(anyhow::anyhow!("test error")),
        cx,
    );
    assert!(activity_rows(window, cx).is_empty());

    send(
        &backend,
        AgentEvent::ToolActivity("read src/main.rs".into()),
        cx,
    );
    drop(backend);
    cx.run_until_parked();
    assert!(activity_rows(window, cx).is_empty());
    session.read_with(cx, |session, _| assert!(session.activity.is_none()));
}

#[gpui_kit::test]
fn replacing_the_session_clears_activity_and_ignores_the_old_connection(cx: &mut TestAppContext) {
    let (window, session, old_backend) = open_chat(cx);
    send(
        &old_backend,
        AgentEvent::ToolActivity("read old.rs".into()),
        cx,
    );
    let (agent, new_backend) = agent::channel();
    old_backend
        .events
        .try_send(AgentEvent::ToolActivity("edit old.rs".into()))
        .unwrap();
    cx.update_window(window, |_, window, cx| {
        session.update(cx, |session, cx| {
            session.replace(
                OpenedChat {
                    agent,
                    messages: Vec::new(),
                },
                AgentKind::Pi,
                1,
                window,
                cx,
            )
        });
    })
    .unwrap();
    cx.run_until_parked();
    assert!(activity_rows(window, cx).is_empty());
    assert!(old_backend.events.is_closed());
    send(
        &new_backend,
        AgentEvent::ToolActivity("read new.rs".into()),
        cx,
    );
    assert_activity(window, "read new.rs", cx);
}

#[gpui_kit::test]
fn long_activity_stays_within_one_line_in_each_pane(cx: &mut TestAppContext) {
    let (window, _, backend) = open_chat(cx);
    send(&backend, AgentEvent::ToolActivity("bash ls".into()), cx);
    let short_rows = activity_rows(window, cx);
    assert_eq!(short_rows.len(), 2);

    let long_command = format!("bash echo {}", "日本語-path/".repeat(200));
    send(&backend, AgentEvent::ToolActivity(long_command.clone()), cx);
    let long_rows = activity_rows(window, cx);
    assert_eq!(long_rows.len(), 2);
    for (short, long) in short_rows.iter().zip(&long_rows) {
        assert_eq!(long.label(), Some(long_command.as_str()));
        assert_eq!(
            short.bounds(),
            long.bounds(),
            "long text must not change layout"
        );
        assert!(long.bounds().size.width <= px(320.));
        assert!(long.bounds().size.height > px(0.));
        assert!(long.bounds().size.height < px(40.));
    }
}
