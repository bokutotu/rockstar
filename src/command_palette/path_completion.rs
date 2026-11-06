use std::{fs, path::Path};

use anyhow::Context as _;

/// One cycle of completed commands, retained until the user edits the input.
pub(super) struct PathCompletion {
    candidates: Vec<String>,
    selected: usize,
}

impl PathCompletion {
    pub(super) fn start(candidates: Vec<String>, forward: bool) -> Option<Self> {
        if candidates.is_empty() {
            return None;
        }
        let selected = if forward { 0 } else { candidates.len() - 1 };
        Some(Self {
            candidates,
            selected,
        })
    }

    pub(super) fn current(&self) -> &str {
        &self.candidates[self.selected]
    }

    pub(super) fn is_unique(&self) -> bool {
        self.candidates.len() == 1
    }

    pub(super) fn advance(&mut self, forward: bool) -> &str {
        let count = self.candidates.len();
        self.selected = if forward {
            (self.selected + 1) % count
        } else {
            (self.selected + count - 1) % count
        };
        self.current()
    }
}

pub(super) fn wsnew_argument(command: &str) -> Option<&str> {
    let text = command.trim_start();
    let (name, argument) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
    name.eq_ignore_ascii_case("wsnew")
        .then(|| argument.trim_start())
}

/// Reads only the directory containing the incomplete name. Keep the entered
/// path spelling in the result; resolution is only needed for the disk lookup.
pub(super) fn wsnew_candidates(
    command: &str,
    root: &Path,
    home: Option<&Path>,
) -> anyhow::Result<Vec<String>> {
    let Some(argument) = wsnew_argument(command) else {
        return Ok(Vec::new());
    };
    let mut command_prefix = command[..command.len() - argument.len()].to_owned();
    if !command_prefix.ends_with(char::is_whitespace) {
        command_prefix.push(' ');
    }
    let quoted = argument.starts_with('"');
    let query = if let Some(rest) = argument.strip_prefix('"') {
        // Also accept an opening quote whose closing quote has not been typed.
        rest.trim_end().strip_suffix('"').unwrap_or(rest)
    } else {
        argument
    };
    let render = |path: &str| {
        if quoted || path.contains(char::is_whitespace) || path.starts_with('"') {
            format!("{command_prefix}\"{path}\"")
        } else {
            format!("{command_prefix}{path}")
        }
    };
    if query == "~" {
        let home = home.context("HOME is not set")?;
        anyhow::ensure!(home.is_dir(), "Not a directory: {}", home.display());
        return Ok(vec![render("~/")]);
    }
    if matches!(query, "." | "..") {
        return Ok(vec![render(&format!("{query}/"))]);
    }
    let (path_prefix, name_prefix) = match query.rfind('/') {
        Some(index) => query.split_at(index + 1),
        None => ("", query),
    };
    let directory = if let Some(relative) = path_prefix.strip_prefix("~/") {
        home.context("HOME is not set")?.join(relative)
    } else {
        root.join(path_prefix)
    };
    let entries = fs::read_dir(&directory)
        .with_context(|| format!("Could not read directory: {}", directory.display()))?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        // is_dir follows symlinks, so links to directories are candidates too.
        if name.starts_with(name_prefix) && entry.path().is_dir() {
            paths.push(format!("{path_prefix}{name}/"));
        }
    }
    // Sort before quoting so whitespace does not change the candidate order.
    paths.sort_unstable();
    Ok(paths
        .iter()
        .map(|path| {
            // A literal directory named '~' must not become a HOME lookup.
            render(if path == "~/" { "./~/" } else { path })
        })
        .collect())
}
