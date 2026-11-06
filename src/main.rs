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

use std::{env, process::ExitCode, sync::Arc};

use gpui_kit::{
    App, TitlebarOptions, WindowBounds, WindowOptions,
    component::{Theme, ThemeMode},
    px, size,
};

use app::open_dock_window;

const USAGE: &str = "Usage: rockstar\n\nOpen a workspace in the current directory. Use :wsnew <directory> for another workspace, :tabnew for a tab, :chat for pi, :term for a terminal, or \\ff to open files.";

fn init_key_handling(cx: &mut App) {
    mode::init(cx);
    file_browser::init(cx);
    input_shortcuts::init(cx);
}

fn main() -> ExitCode {
    let args: Vec<_> = env::args_os().skip(1).collect();
    match args.as_slice() {
        [] => {}
        [arg] if arg == "--help" || arg == "-h" => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::FAILURE;
        }
    }

    let current_dir = env::current_dir().unwrap();

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            cx.set_http_client(media::http::MediaHttpClient::new(cx.background_executor().clone()));
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
            open_dock_window(options, current_dir, Arc::new(pi::connect), cx)
                .expect("failed to open the app window");
            cx.activate(true);
        });

    ExitCode::SUCCESS
}
