//! Small vector file-type icons; no dependency on an installed icon font.
use gpui_kit::{
    InteractiveElement as _, IntoElement, ParentElement as _, StatefulInteractiveElement as _,
    Styled as _, component::Icon, div, px, rgb,
};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    File,
    Code,
    Text,
    Data,
    Settings,
    Terminal,
    Image,
    Audio,
    Video,
    Archive,
    Folder,
    OpenFolder,
}

macro_rules! icon {
    ($shape:literal) => {
        concat!("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.7\" stroke-linecap=\"round\" stroke-linejoin=\"round\">", $shape, "</svg>").as_bytes()
    };
}

impl Kind {
    fn svg(self) -> &'static [u8] {
        match self {
            Self::File => icon!("<path d=\"M5 3h9l5 5v13H5z M14 3v5h5\"/>"),
            Self::Code => icon!("<path d=\"m8 6-6 6 6 6 m8-12 6 6-6 6 M14 3l-4 18\"/>"),
            Self::Text => icon!("<path d=\"M4 3h16v18H4z M7 7h10 M7 11h10 M7 15h6\"/>"),
            Self::Data => icon!("<path d=\"M9 3H6v6l-3 3 3 3v6h3 M15 3h3v6l3 3-3 3v6h-3\"/>"),
            Self::Settings => {
                icon!("<path d=\"M4 6h16 M4 12h16 M4 18h16 M8 3v6 M16 9v6 M10 15v6\"/>")
            }
            Self::Terminal => icon!(
                "<rect x=\"2\" y=\"4\" width=\"20\" height=\"16\" rx=\"2\"/><path d=\"m6 8 4 4-4 4 M13 16h5\"/>"
            ),
            Self::Image => icon!(
                "<rect x=\"3\" y=\"3\" width=\"18\" height=\"18\" rx=\"2\"/><circle cx=\"8\" cy=\"8\" r=\"2\"/><path d=\"m3 18 6-6 4 4 4-5 4 5\"/>"
            ),
            Self::Audio => icon!(
                "<path d=\"M9 17V5l11-2v12 M9 9l11-2\"/><ellipse cx=\"6\" cy=\"18\" rx=\"3\" ry=\"2\"/><ellipse cx=\"17\" cy=\"16\" rx=\"3\" ry=\"2\"/>"
            ),
            Self::Video => icon!(
                "<rect x=\"2\" y=\"4\" width=\"20\" height=\"16\" rx=\"2\"/><path d=\"m10 8 6 4-6 4z\"/>"
            ),
            Self::Archive => {
                icon!("<path d=\"M5 3h14v18H5z M11 3h2v3h-2v3h2v3h-2 M10 15h4v3h-4z\"/>")
            }
            Self::Folder => icon!("<path d=\"M2 6V4h7l3 3h10v14H2z\"/>"),
            Self::OpenFolder => icon!("<path d=\"M2 20V4h7l3 3h9v3 M2 20l4-10h17l-4 10z\"/>"),
        }
    }
}

fn specification(path: &str, expanded: bool) -> (Kind, &'static str, u32) {
    if path.ends_with('/') {
        return (
            if expanded {
                Kind::OpenFolder
            } else {
                Kind::Folder
            },
            "Folder",
            0xc18b26,
        );
    }
    let path = Path::new(path);
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(
        name.as_str(),
        "makefile" | "gnumakefile" | "cmakelists.txt" | "dockerfile" | "containerfile"
    ) || name.starts_with(".env")
        || name.ends_with(".lock")
    {
        return (Kind::Settings, "Build / configuration", 0x7c6fba);
    }
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "rs" => (Kind::Code, "Rust source", 0xb85c38),
        "py" | "pyi" | "pyw" => (Kind::Code, "Python source", 0x3776ab),
        "js" | "jsx" | "mjs" | "cjs" => (Kind::Code, "JavaScript source", 0xa58100),
        "ts" | "tsx" | "mts" | "cts" => (Kind::Code, "TypeScript source", 0x3178c6),
        "c" | "h" | "cc" | "cpp" | "hpp" | "cs" | "go" | "rb" | "java" | "kt" | "swift" | "lua"
        | "php" | "dart" | "hs" | "html" | "css" | "scss" | "vue" | "svelte" => {
            (Kind::Code, "Source code", 0x518c92)
        }
        "md" | "markdown" | "mdx" => (Kind::Text, "Markdown", 0x4385be),
        "json" | "jsonc" | "xml" | "csv" | "sql" => (Kind::Data, "Data", 0x967000),
        "nix" | "toml" | "yaml" | "yml" | "ini" | "conf" | "cfg" | "cmake" | "mk" => {
            (Kind::Settings, "Configuration", 0x7c6fba)
        }
        "sh" | "bash" | "zsh" | "fish" | "ps1" | "bat" | "cmd" => {
            (Kind::Terminal, "Shell script", 0x47874b)
        }
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico" | "bmp" | "avif" => {
            (Kind::Image, "Image", 0xa15a9c)
        }
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" => (Kind::Audio, "Audio", 0xa15a9c),
        "mp4" | "mkv" | "webm" | "mov" | "avi" => (Kind::Video, "Video", 0xa15a9c),
        "zip" | "gz" | "xz" | "bz2" | "tar" | "7z" | "rar" | "zst" => {
            (Kind::Archive, "Archive", 0xa67c3b)
        }
        "txt" | "rst" | "adoc" | "pdf" | "tex" => (Kind::Text, "Document", 0x73808c),
        _ => (Kind::File, "File", 0x73808c),
    }
}

pub(super) fn view(path: &str, expanded: bool) -> impl IntoElement {
    let (kind, label, color) = specification(path, expanded);
    div()
        .id("file-type-icon")
        .w(px(16.))
        .h(px(16.))
        .flex_shrink_0()
        .aria_label(label)
        .child(
            Icon::default()
                .data(kind.svg())
                .size(px(16.))
                .text_color(rgb(color)),
        )
}
