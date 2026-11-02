mod agent;
mod chat;
mod math;
mod mermaid;
mod pi;

use std::{env, process::ExitCode};

use chat::open_chat_window;
use pi::PiCodingAgent;

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
        .and_then(|cwd| PiCodingAgent::new(&cwd))
    {
        Ok(agent) => agent,
        Err(error) => {
            eprintln!("rockstar: {error:#}");
            return ExitCode::FAILURE;
        }
    };

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

    ExitCode::SUCCESS
}
