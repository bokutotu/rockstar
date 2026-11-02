mod math;
mod mermaid;

#[cfg(test)]
mod visual_tests;

use math::MathPlugin;
use mermaid::MermaidPlugin;
use std::{env, fs, io, path::Path, process::ExitCode};

use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Context, Entity, IntoElement, ParentElement as _,
    Render, SharedString, Styled as _, TitlebarOptions, Window, WindowBounds, WindowOptions,
    base::{TextView, TextViewState},
    component::{ActiveTheme as _, Theme, ThemeMode},
    div, px, size,
};

const USAGE: &str = "Usage: rockstar FILE.md\n\nDisplay a UTF-8 Markdown file.";

fn load_source(path: &Path) -> io::Result<SharedString> {
    fs::read_to_string(path).map(Into::into)
}

struct MarkdownView {
    document: Entity<TextViewState>,
    inline_math: MathPlugin,
    block_math: MathPlugin,
    mermaid: MermaidPlugin,
}

impl MarkdownView {
    fn new(source: SharedString, cx: &mut Context<Self>) -> Self {
        let document = cx.new(|cx| TextViewState::markdown(&source, cx));
        let inline_math = MathPlugin::inline(&document);
        let block_math = MathPlugin::block(&document);
        let mermaid = MermaidPlugin::new(&document);
        Self {
            document,
            inline_math,
            block_math,
            mermaid,
        }
    }
}

impl Render for MarkdownView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .font_family("Noto Sans CJK JP")
            .text_size(px(16.0))
            .child(
                TextView::new(&self.document)
                    .plugin(self.inline_math.clone())
                    .plugin(self.block_math.clone())
                    .plugin(self.mermaid.clone())
                    .scrollable(true)
                    .p_6(),
            )
    }
}

fn open_markdown_window(source: SharedString, cx: &mut App) -> gpui_kit::Result<AnyWindowHandle> {
    gpui_kit::init(cx);
    Theme::change(ThemeMode::Light, None, cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(960.0), px(720.0)), cx)),
        window_min_size: Some(size(px(360.0), px(240.0))),
        titlebar: Some(TitlebarOptions {
            title: Some("Rockstar — Markdown".into()),
            ..Default::default()
        }),
        app_id: Some("rockstar".into()),
        ..Default::default()
    };
    gpui_kit::open_window(options, cx, |_, cx| {
        cx.new(|cx| MarkdownView::new(source, cx))
    })
    .map(|(window, _)| window)
}

fn main() -> ExitCode {
    let args: Vec<_> = env::args_os().skip(1).collect();
    let path = match args.as_slice() {
        [arg] if arg == "--help" || arg == "-h" => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        [path] => Path::new(path),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    let source = match load_source(path) {
        Ok(source) => source,
        Err(error) => {
            eprintln!("rockstar: could not read Markdown: {error}");
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

            open_markdown_window(source, cx).expect("failed to open the Markdown window");
            cx.activate(true);
        });

    ExitCode::SUCCESS
}
