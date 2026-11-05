use std::path::PathBuf;

use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Font, FontFallbacks, Refineable as _, StyleRefinement,
    Styled as _, WindowOptions, base::Root, component::ActiveTheme as _,
};

use crate::{
    agent::Agent,
    command_palette,
    workspace::{AgentConnector, Workspace},
};

pub(crate) fn open_dock_window(
    options: WindowOptions,
    agent: Agent,
    root: PathBuf,
    connect: AgentConnector,
    cx: &mut App,
) -> gpui_kit::Result<AnyWindowHandle> {
    let (handle, workspace) = gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| Workspace::new(agent, root, connect, window, cx))
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
        command_palette::bind_mode(&workspace, window, cx);
    })?;
    Ok(handle)
}
