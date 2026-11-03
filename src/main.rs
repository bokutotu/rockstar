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

use app::open_chat_window;

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

    let agent = match env::current_dir()
        .map_err(anyhow::Error::from)
        .and_then(|cwd| pi::connect(&cwd))
    {
        Ok(agent) => agent,
        Err(error) => {
            eprintln!("rockstar: {error:#}");
            return ExitCode::FAILURE;
        }
    };

    let shutdown_requests = agent.handle.requests.clone();
    let remaining_events = agent.events.clone();

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            open_chat_window(agent, cx).expect("failed to open the chat window");
            cx.activate(true);
        });

    shutdown_requests.close();
    while remaining_events.recv_blocking().is_ok() {}

    ExitCode::SUCCESS
}
