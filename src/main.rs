mod agent;
mod app;
mod chat;
mod command_palette;
mod interaction;
mod math;
mod mermaid;
mod pi;

#[cfg(test)]
mod e2e_tests;
#[cfg(test)]
mod test_support;

use std::{env, process::ExitCode};

use gpui_kit::{
    AppContext as _, TitlebarOptions, WindowBounds, WindowOptions,
    component::{Theme, ThemeMode},
    px, size,
};

use app::AppView;

const USAGE: &str = "Usage: rockstar\n\nChat with pi in the current directory.";

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
    let agent = pi::connect(&current_dir).unwrap();

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            gpui_kit::init(cx);
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
            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| AppView::new(agent, window, cx))
            })
            .expect("failed to open the app window");
            cx.activate(true);
        });

    ExitCode::SUCCESS
}
