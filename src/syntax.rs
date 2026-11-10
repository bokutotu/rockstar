//! Shared language detection for source editors and diff views.
use std::path::Path;

pub(crate) fn language_settings(path: &Path) -> (&'static str, usize) {
    match path.file_name().and_then(|name| name.to_str()).unwrap_or("") {
        "Makefile" | "GNUmakefile" | "makefile" => return ("make", 4),
        ".bashrc" | ".bash_profile" | ".profile" | ".zshrc" => return ("bash", 2),
        "CMakeLists.txt" => return ("cmake", 2),
        _ => {}
    }
    match path.extension().and_then(|s| s.to_str()).unwrap_or("") {
        "rs" => ("rust", 4),
        "py" => ("python", 4),
        "nix" => ("nix", 2),
        "lua" => ("lua", 2),
        "js" | "mjs" | "cjs" => ("javascript", 4),
        "jsx" => ("javascript", 2),
        "ts" | "mts" | "cts" => ("typescript", 4),
        "tsx" => ("tsx", 2),
        "json" | "jsonc" => ("json", 4),
        "c" | "h" => ("c", 4),
        "cc" | "cpp" | "cxx" | "c++" | "C" | "hh" | "hpp" | "hxx" | "h++" | "H" | "cu" | "cuh" => {
            ("cpp", 4)
        }
        "tf" | "tfvars" => ("hcl", 2),
        "cs" => ("c_sharp", 4),
        "go" => ("go", 4),
        "hs" | "hsc" | "lhs" | "cabal" => ("haskell", 4),
        "rb" => ("ruby", 2),
        "html" => ("html", 2),
        "css" => ("css", 2),
        "sh" | "bash" | "zsh" => ("bash", 2),
        "toml" => ("toml", 4),
        "yaml" | "yml" => ("yaml", 2),
        "md" | "markdown" | "mdx" => ("markdown", 4),
        "sql" => ("sql", 2),
        "kt" => ("kotlin", 4),
        "java" => ("java", 4),
        "php" => ("php", 4),
        "dart" => ("dart", 2),
        "tex" => ("latex", 2),
        "xml" | "svg" | "xaml" | "csproj" => ("xml", 2),
        "cmake" => ("cmake", 2),
        "mk" => ("make", 4),
        _ => ("plain", 4),
    }
}
