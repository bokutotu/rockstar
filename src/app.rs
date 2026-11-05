use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Focusable as _, Font, FontFallbacks, Refineable as _,
    StyleRefinement, Styled as _, WindowOptions,
    base::Root,
    component::{
        ActiveTheme as _,
        dock::{DockLayout, DockSkin, panel_handle},
    },
};

use crate::{agent::Agent, chat::ChatView, command_palette};

pub(crate) fn open_dock_window(
    options: WindowOptions,
    mut agent: Agent,
    cx: &mut App,
) -> gpui_kit::Result<AnyWindowHandle> {
    let new_sessions = agent.new_session_sender();
    let (handle, dock) = gpui_kit::open_window(options, cx, |window, cx| {
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
        let (dock, _) = DockSkin::dock_area("workspace", None, window, cx);
        dock.update(cx, |dock, cx| {
            dock.set_center(
                DockLayout::tabs().panel_view(panel_handle(chat), cx),
                window,
                cx,
            );
        });
        dock
    })?;
    handle.update(cx, |_, window, cx| {
        Root::update(window, cx, |root, _, cx| {
            root.style().refine(
                &StyleRefinement::default()
                    .font(Font {
                        family: cx.theme().font_family.clone(),
                        fallbacks: Some(FontFallbacks::from_fonts(vec![
                            "Noto Sans CJK JP".into(),
                            "Noto Color Emoji".into(),
                        ])),
                        ..Default::default()
                    })
                    .text_size(cx.theme().font_size),
            );
            cx.notify();
        });
        command_palette::bind_mode(new_sessions, dock.focus_handle(cx), window, cx);
    })?;
    Ok(handle)
}
