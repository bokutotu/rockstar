mod agent;
mod app;
mod chat;
mod command_palette;
mod diff;
mod file_browser;
mod file_buffer;
mod file_search;
mod file_view;
mod file_watch;
mod input_shortcuts;
mod instance;
mod launch;
mod lsp;
mod markdown;
mod math;
mod media;
mod mermaid;
mod mode;
mod pane;
mod pi;
mod syntax;
mod terminal;
mod workspace;

use std::{env, os::unix::net::UnixStream, process::ExitCode, sync::Arc};

use anyhow::Context as _;

use gpui_kit::{
    App, TitlebarOptions, WindowBounds, WindowOptions,
    component::{Theme, ThemeMode},
    px, size,
};

use app::{dispatch_requests, open_dock_window};
use instance::{InstanceServer, OpenWorkspaceRequest};
use launch::{LaunchMode, StartupStatus};

const USAGE: &str = "Usage: rockstar\n\nOpen the current directory in Rockstar's single window, reusing its workspace if already open. Return after startup; Rockstar keeps running when the terminal closes. Use :wsnew <directory> for another workspace, :tabnew for a tab, :chat for pi, :resume for saved chats, :term for a terminal, or \\ff to open files.";

fn init_key_handling(cx: &mut App) {
    mode::init(cx);
    file_browser::init(cx);
    input_shortcuts::init(cx);
}

fn main() -> ExitCode {
    let args: Vec<_> = env::args_os().skip(1).collect();
    let mode = match args.as_slice() {
        [] => LaunchMode::Cli,
        [arg, _, _] if arg == launch::GUI_ARGUMENT => LaunchMode::Gui,
        [arg] if arg == "--help" || arg == "-h" => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    let result = match mode {
        LaunchMode::Cli => run_cli(),
        LaunchMode::Gui => launch::run_gui_process(&args[1], &args[2]),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rockstar: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run_cli() -> anyhow::Result<()> {
    let request = OpenWorkspaceRequest::new(
        &env::current_dir().context("Cannot determine the current directory")?,
    )?;
    launch::run_cli(request)
}

fn run_gui(
    request: OpenWorkspaceRequest,
    server: InstanceServer,
    mut startup: UnixStream,
) -> anyhow::Result<()> {
    let requests = server.requests()?;

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            cx.set_http_client(media::http::MediaHttpClient::new(
                cx.background_executor().clone(),
            ));
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            gpui_kit::init(cx);
            init_key_handling(cx);
            Theme::change(ThemeMode::Light, None, cx);
            Theme::update(cx, |theme| {
                theme.font_family = "JetBrainsMono Nerd Font".into();
                theme.font_size = px(11.0);
                theme.mono_font_family = theme.font_family.clone();
                theme.mono_font_size = theme.font_size;
            });
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
            let result = (|| {
                let (window, workspace) =
                    open_dock_window(options, request.root, Arc::new(pi::connect), cx)
                        .context("Cannot open the Rockstar window")?;
                dispatch_requests(requests, workspace, window, cx);
                cx.activate(true);
                launch::send_startup_status(&mut startup, StartupStatus::Ready)
            })();
            if let Err(error) = result {
                eprintln!("rockstar: {error:#}");
                let _ = launch::send_startup_status(
                    &mut startup,
                    StartupStatus::Failed(format!("{error:#}")),
                );
                cx.quit();
            }
        });

    // Retain the exclusive reservation for the entire GUI lifetime, not just
    // until startup finishes or the request worker stops.
    drop(server);
    Ok(())
}
