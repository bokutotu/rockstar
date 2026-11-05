mod agent;
mod app;
mod chat;
mod command_palette;
mod file_search;
mod input_shortcuts;
mod math;
mod mermaid;
mod mode;
mod pi;
mod workspace;

#[cfg(test)]
mod e2e_tests;
#[cfg(test)]
mod test_support;

use std::{env, process::ExitCode, sync::Arc};

use gpui_kit::{
    App, TitlebarOptions, WindowBounds, WindowOptions,
    component::{Theme, ThemeMode},
    px, size,
};

use app::open_dock_window;

const USAGE: &str = "Usage: rockstar\n\nChat with pi in the current directory.";

fn init_key_handling(cx: &mut App) {
    mode::init(cx);
    workspace::init(cx);
    input_shortcuts::init(cx);
    cx.intercept_keystrokes(|event, _, cx| {
        // Intercept before keybindings in every window, regardless of the focused view.
        if mode::handle_key(&event.keystroke, cx) {
            cx.stop_propagation();
        }
    })
    .detach();
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
            open_dock_window(options, agent, current_dir, Arc::new(pi::connect), cx)
                .expect("failed to open the app window");
            cx.activate(true);
        });

    ExitCode::SUCCESS
}
