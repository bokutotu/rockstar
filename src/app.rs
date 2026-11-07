use std::path::PathBuf;

use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Entity, Font, FontFallbacks, Refineable as _,
    StyleRefinement, Styled as _, WindowOptions, base::Root, component::ActiveTheme as _,
};

use crate::{
    command_palette,
    instance::IncomingRequest,
    workspace::{AgentConnector, WorkspaceWindow},
};

pub(crate) fn open_dock_window(
    options: WindowOptions,
    root: PathBuf,
    connect: AgentConnector,
    cx: &mut App,
) -> gpui_kit::Result<(AnyWindowHandle, Entity<WorkspaceWindow>)> {
    let (handle, workspace) = gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| WorkspaceWindow::new(root, connect, window, cx))
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
        let workspace = workspace.downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| workspace.request_close(window, cx))
                .unwrap_or(true)
        });
    })?;
    Ok((handle, workspace))
}

pub(crate) fn dispatch_requests(
    requests: async_channel::Receiver<IncomingRequest>,
    workspace: Entity<WorkspaceWindow>,
    handle: AnyWindowHandle,
    cx: &mut App,
) {
    let workspace = workspace.downgrade();
    cx.spawn(async move |cx| {
        while let Ok(IncomingRequest { request, reply }) = requests.recv().await {
            let result = handle
                .update(cx, |_, window, cx| {
                    workspace.update(cx, |workspace, cx| {
                        workspace.open_workspace_root(request.root, window, cx)
                    })??;
                    window.activate_window();
                    cx.activate(true);
                    Ok(())
                })
                .and_then(|result| result);
            // Unbounded single-response channel: this never blocks the UI.
            let _ = reply.send(result);
        }
    })
    .detach();
}
