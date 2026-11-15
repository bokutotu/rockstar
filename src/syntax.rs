//! Shared grammar registration and language detection for editors, diffs, and Markdown.
use std::{path::Path, sync::LazyLock};

use anyhow::{Context as _, Result, ensure};
use gpui_kit::component::highlighter::{GrammarConfig, LanguageRegistry};

/// Install all overrides before any editor or Markdown highlighter is created.
pub(crate) fn init() -> Result<()> {
    // Keep the original bundled queries when init is called more than once.
    static CONFIGS: LazyLock<Vec<GrammarConfig>> = LazyLock::new(grammar_configs);
    let registry = LanguageRegistry::singleton();
    for config in CONFIGS.iter() {
        let grammar = config.language.as_ref().context("Missing syntax grammar")?;
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(grammar)
            .with_context(|| format!("Incompatible {} grammar", config.name))?;
        ensure!(
            !config.highlights.trim().is_empty(),
            "Missing {} highlight query",
            config.name
        );
        // Compile the same combined query as SyntaxHighlighter. Validate every
        // definition before registering any, rather than falling back to text.
        tree_sitter::Query::new(
            grammar,
            &format!(
                "{}\n{}\n{}",
                config.injections, config.locals, config.highlights
            ),
        )
        .with_context(|| format!("Invalid {} syntax query", config.name))?;
    }
    for config in CONFIGS.iter() {
        registry.register(&config.name, config);
    }
    // These also apply to Markdown code-fence labels and language injections.
    for (alias, language) in [
        ("hs", "haskell"),
        ("hsc", "haskell"),
        ("terraform", "hcl"),
        ("tf", "hcl"),
        ("tfvars", "hcl"),
        ("tex", "latex"),
        ("c_sharp", "csharp"),
        ("c#", "csharp"),
        ("jsx", "javascript"),
        ("javascriptreact", "javascript"),
        ("typescriptreact", "tsx"),
        ("gql", "graphql"),
        ("exs", "elixir"),
        ("zsh", "bash"),
        ("shell", "bash"),
        ("shellscript", "bash"),
        ("patch", "diff"),
        ("svg", "xml"),
        ("xaml", "xml"),
        ("csproj", "xml"),
    ] {
        let config = registry
            .language(language)
            .with_context(|| format!("Missing syntax language {language} for alias {alias}"))?;
        registry.register(alias, &config);
    }
    Ok(())
}

fn grammar_configs() -> Vec<GrammarConfig> {
    let registry = LanguageRegistry::singleton();
    let bundled = |name| {
        registry
            .language(name)
            .unwrap_or_else(|| panic!("Missing bundled syntax language {name}"))
    };
    let mut configs = vec![
        GrammarConfig::new(
            "haskell",
            tree_sitter_haskell::LANGUAGE.into(),
            ["css", "html", "javascript", "json", "sql", "typescript"]
                .into_iter()
                .map(Into::into)
                .collect(),
            tree_sitter_haskell::HIGHLIGHTS_QUERY,
            tree_sitter_haskell::INJECTIONS_QUERY,
            tree_sitter_haskell::LOCALS_QUERY,
        ),
        GrammarConfig::new(
            "nix",
            tree_sitter_nix::LANGUAGE.into(),
            vec!["bash".into()],
            tree_sitter_nix::HIGHLIGHTS_QUERY,
            tree_sitter_nix::INJECTIONS_QUERY,
            "",
        ),
        GrammarConfig::new(
            "hcl",
            tree_sitter_hcl::LANGUAGE.into(),
            vec![],
            include_str!("syntax/hcl.scm"),
            "",
            "",
        ),
        GrammarConfig::new(
            "dart",
            tree_sitter_dart::LANGUAGE.into(),
            vec![],
            tree_sitter_dart::HIGHLIGHTS_QUERY,
            "",
            tree_sitter_dart::LOCALS_QUERY,
        ),
        GrammarConfig::new(
            "latex",
            tree_sitter_latex::LANGUAGE.into(),
            vec![],
            include_str!("syntax/latex.scm"),
            "",
            "",
        ),
        GrammarConfig::new(
            "xml",
            tree_sitter_xml::LANGUAGE_XML.into(),
            vec![],
            tree_sitter_xml::XML_HIGHLIGHT_QUERY,
            "",
            "",
        ),
    ];

    // These bundled parsers have no highlight query in gpui-component 0.8.1.
    for (language, highlights) in [
        ("csharp", tree_sitter_c_sharp::HIGHLIGHTS_QUERY),
        ("cmake", tree_sitter_cmake::HIGHLIGHTS_QUERY),
        ("swift", tree_sitter_swift::HIGHLIGHTS_QUERY),
        ("graphql", include_str!("syntax/graphql.scm")),
        ("proto", include_str!("syntax/proto.scm")),
    ] {
        let mut config = bundled(language);
        config.highlights = highlights.to_owned().into();
        configs.push(config);
    }

    // C++'s upstream query contains only additions to C, not the common rules.
    let mut cpp = bundled("cpp");
    cpp.highlights = format!("{}\n{}", bundled("c").highlights, cpp.highlights).into();
    configs.push(cpp);

    // Likewise, TSX needs JavaScript + TypeScript + JSX, not just TS additions.
    let mut javascript = bundled("javascript");
    javascript.highlights = format!(
        "{}\n{}",
        javascript.highlights,
        tree_sitter_javascript::JSX_HIGHLIGHT_QUERY
    )
    .into();
    configs.push(javascript);
    let typescript = bundled("typescript");
    let mut tsx = bundled("tsx");
    tsx.highlights = format!(
        "{}\n{}",
        typescript.highlights,
        tree_sitter_javascript::JSX_HIGHLIGHT_QUERY
    )
    .into();
    tsx.injections = typescript.injections;
    tsx.injection_languages = typescript.injection_languages;
    configs.push(tsx);

    for config in &mut configs {
        // Adapt upstream editor-specific annotations to GPUI's theme vocabulary.
        // Rust Tree-sitter does not evaluate Neovim's lua-match? predicate.
        config.highlights = config
            .highlights
            .replace(" @spell", "")
            .replace(" @nospell", "")
            .replace("@character", "@string")
            .replace("@module", "@type")
            .replace("@escape", "@string.escape")
            .replace("@markup.link", "@link_uri")
            .replace("@markup.heading", "@title")
            .replace("@markup.raw", "@text.literal")
            .replace("#lua-match?", "#match?")
            .replace("^[%u@][%u%d_]+$", "^[A-Z@][A-Z0-9_]+$")
            .into();
    }
    configs
}

pub(crate) fn language_settings(path: &Path) -> (&'static str, usize) {
    match path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
    {
        "Makefile" | "GNUmakefile" | "makefile" => return ("make", 4),
        ".bashrc" | ".bash_profile" | ".profile" | ".zshrc" => return ("bash", 2),
        "CMakeLists.txt" => return ("cmake", 2),
        _ => {}
    }
    match path.extension().and_then(|s| s.to_str()).unwrap_or("") {
        "rs" => ("rust", 4),
        "py" | "pyi" => ("python", 4),
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
        "tf" | "tfvars" | "hcl" => ("hcl", 2),
        "cs" => ("csharp", 4),
        "go" => ("go", 4),
        "hs" | "hsc" | "lhs" | "cabal" => ("haskell", 4),
        "rb" => ("ruby", 2),
        "html" | "htm" => ("html", 2),
        "css" | "scss" => ("css", 2),
        "sh" | "bash" | "zsh" => ("bash", 2),
        "toml" => ("toml", 4),
        "yaml" | "yml" => ("yaml", 2),
        "md" | "markdown" | "mdx" => ("markdown", 4),
        "sql" => ("sql", 2),
        "kt" | "kts" | "ktm" => ("kotlin", 4),
        "java" => ("java", 4),
        "php" | "php3" | "php4" | "php5" | "phtml" => ("php", 4),
        "dart" => ("dart", 2),
        "tex" => ("latex", 2),
        "xml" | "svg" | "xaml" | "csproj" => ("xml", 2),
        "cmake" => ("cmake", 2),
        "mk" => ("make", 4),
        "astro" => ("astro", 4),
        "svelte" => ("svelte", 4),
        "ex" | "exs" => ("elixir", 4),
        "ejs" => ("ejs", 4),
        "erb" => ("erb", 4),
        "scala" | "sc" | "sbt" => ("scala", 4),
        "swift" => ("swift", 4),
        "zig" => ("zig", 4),
        "graphql" | "gql" => ("graphql", 4),
        "proto" => ("proto", 4),
        "diff" | "patch" => ("diff", 4),
        _ => ("plain", 4),
    }
}
