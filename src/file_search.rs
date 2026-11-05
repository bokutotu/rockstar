//! Workspace file listing and fuzzy matching for `@` file mentions.
//!
//! pi's RPC mode has no file listing command, so Rockstar walks the workspace
//! itself, the same way pi's TUI does with `fd`: hidden files are included and
//! `.gitignore` rules are respected.
use std::{path::Path, process::Command};

use ignore::WalkBuilder;

/// Stop walking huge trees; matches past this point are rarely useful.
const MAX_ENTRIES: usize = 50_000;

/// Lists the files and directories under `root` as `/`-separated relative
/// paths. Directories end with `/`.
pub(crate) fn scan(root: &Path) -> Vec<String> {
    let mut paths = Vec::new();
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .filter_entry(|entry| entry.file_name() != ".git")
        .build();
    for entry in walker.flatten() {
        if entry.depth() == 0 {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(root) else {
            continue;
        };
        let mut path = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if entry.file_type().is_some_and(|kind| kind.is_dir()) {
            path.push('/');
        }
        paths.push(path);
        if paths.len() >= MAX_ENTRIES {
            break;
        }
    }
    paths.sort();
    paths
}

/// Telescope-style file list: tracked files (even if now ignored) plus untracked,
/// non-ignored files. Outside Git, use the same ignore-aware walker as mentions.
pub(crate) fn project_files(root: &Path) -> anyhow::Result<Vec<String>> {
    if let Ok(output) = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .output()
        && output.status.success()
    {
        let mut files = output
            .stdout
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .filter(|p| root.join(p).is_file())
            .collect::<Vec<_>>();
        files.sort();
        files.dedup();
        return Ok(files);
    }
    let mut paths = Vec::new();
    for entry in WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .filter_entry(|entry| entry.file_name() != ".git")
        .build()
    {
        let entry = entry?;
        if entry.depth() > 0 && entry.path().is_file() {
            paths.push(
                entry
                    .path()
                    .strip_prefix(root)?
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        if paths.len() >= MAX_ENTRIES {
            break;
        }
    }
    paths.sort();
    Ok(paths)
}

/// Finds the `@` mention being typed at `cursor` (a byte offset into `text`).
///
/// Returns the byte offset of the `@` and the query after it. A mention starts
/// a whitespace-separated word, so `user@example.com` is not one.
pub(crate) fn mention_at(text: &str, cursor: usize) -> Option<(usize, &str)> {
    let before = text.get(..cursor)?;
    let start = before
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map_or(0, |(index, c)| index + c.len_utf8());
    let query = before[start..].strip_prefix('@')?;
    Some((start, query))
}

/// The text that replaces an `@query` mention when `path` is chosen.
pub(crate) fn mention_text(path: &str) -> String {
    let mention = if path.contains(char::is_whitespace) {
        format!("@\"{path}\"")
    } else {
        format!("@{path}")
    };
    // Keep completing inside a directory; finish the word after a file.
    if path.ends_with('/') {
        mention
    } else {
        mention + " "
    }
}

/// Returns up to `limit` paths matching `query`, best first.
pub(crate) fn fuzzy_match(paths: &[String], query: &str, limit: usize) -> Vec<String> {
    let query: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    let mut scored: Vec<(i64, &String)> = paths
        .iter()
        .filter_map(|path| score(&query, path).map(|score| (score, path)))
        .collect();
    scored.sort_by(|(a_score, a), (b_score, b)| {
        b_score
            .cmp(a_score)
            .then_with(|| a.len().cmp(&b.len()))
            .then_with(|| a.cmp(b))
    });
    scored
        .into_iter()
        .take(limit)
        .map(|(_, path)| path.clone())
        .collect()
}

/// Scores `path` against a lowercase `query`; `None` if it does not match.
fn score(query: &[char], path: &str) -> Option<i64> {
    let depth = path.trim_end_matches('/').matches('/').count() as i64;
    if query.is_empty() {
        // Without a query, show the shallowest entries first.
        return Some(-depth * 10);
    }

    let chars: Vec<char> = path.chars().flat_map(char::to_lowercase).collect();
    let name_start = chars[..chars.len().saturating_sub(1)]
        .iter()
        .rposition(|&c| c == '/')
        .map_or(0, |index| index + 1);

    // Greedy subsequence match, rewarding word starts and consecutive runs.
    let mut score = 0i64;
    let mut next = 0;
    let mut previous: Option<usize> = None;
    for &wanted in query {
        let index = next + chars[next..].iter().position(|&c| c == wanted)?;
        let at_word_start = index == 0 || matches!(chars[index - 1], '/' | '_' | '-' | '.' | ' ');
        score += 1;
        if at_word_start {
            score += 8;
        }
        if previous.is_some_and(|previous| previous + 1 == index) {
            score += 5;
        }
        if index >= name_start {
            score += 2;
        }
        previous = Some(index);
        next = index + 1;
    }

    let query: String = query.iter().collect();
    let name: String = chars[name_start..].iter().collect();
    if name.starts_with(&query) {
        score += 40;
    } else if name.contains(&query) {
        score += 25;
    } else if chars.iter().collect::<String>().contains(&query) {
        score += 15;
    }
    Some(score - depth)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn paths(list: &[&str]) -> Vec<String> {
        list.iter().map(|path| path.to_string()).collect()
    }

    #[test]
    fn mention_requires_word_start() {
        assert_eq!(mention_at("@", 1), Some((0, "")));
        assert_eq!(mention_at("see @src/ma", 11), Some((4, "src/ma")));
        assert_eq!(mention_at("a\n@x", 4), Some((2, "x")));
        assert_eq!(mention_at("user@example", 12), None);
        assert_eq!(mention_at("@src done", 9), None);
        assert_eq!(
            mention_at("日本 @ファ", "日本 @ファ".len()),
            Some((7, "ファ"))
        );
    }

    #[test]
    fn mention_text_quotes_spaces_and_keeps_directories_open() {
        assert_eq!(mention_text("src/main.rs"), "@src/main.rs ");
        assert_eq!(mention_text("src/"), "@src/");
        assert_eq!(mention_text("my notes.md"), "@\"my notes.md\" ");
    }

    #[test]
    fn fuzzy_match_prefers_file_names() {
        let list = paths(&[
            "Cargo.toml",
            "src/",
            "src/chat/composer.rs",
            "src/command_palette.rs",
            "src/main.rs",
            "src/mermaid.rs",
        ]);
        assert_eq!(fuzzy_match(&list, "main", 3), vec!["src/main.rs"]);
        assert_eq!(fuzzy_match(&list, "comp", 1), vec!["src/chat/composer.rs"]);
        assert_eq!(fuzzy_match(&list, "cmp", 1), vec!["src/command_palette.rs"]);
        assert_eq!(fuzzy_match(&list, "", 2), vec!["src/", "Cargo.toml"]);
        assert!(fuzzy_match(&list, "zzz", 5).is_empty());
    }

    #[test]
    fn scan_respects_gitignore_and_skips_git() {
        let root = std::env::temp_dir().join(format!("rockstar-scan-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join(".gitignore"), "target/\n").unwrap();
        fs::write(root.join("src/main.rs"), "").unwrap();
        fs::write(root.join(".git/HEAD"), "").unwrap();
        fs::write(root.join("target/out"), "").unwrap();

        let found = scan(&root);
        fs::remove_dir_all(&root).unwrap();
        assert_eq!(found, paths(&[".gitignore", "src/", "src/main.rs"]));
    }
}
