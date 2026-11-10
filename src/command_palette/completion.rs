// Keep sorted, including every alias handled by CommandPaletteView::confirm.
const COMMAND_NAMES: &[&str] = &[
    "agy",
    "bw",
    "bw!",
    "close",
    "codex",
    "diff",
    "diffstaged",
    "diffunstaged",
    "e!",
    "new",
    "pi",
    "prdiff",
    "q",
    "qa",
    "quit",
    "resume",
    "stage",
    "tabclose",
    "tabnew",
    "term",
    "unstage",
    "w",
    "write",
    "wsclose",
    "wsnew",
];

/// One cycle of completed commands, retained until the user edits the input.
pub(super) struct Completion {
    pub(super) candidates: Vec<String>,
    pub(super) selected: usize,
}

impl Completion {
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

pub(super) fn command_candidates(command: &str) -> Vec<String> {
    let name = command.trim_start();
    // Only complete the command name, never replace an argument with a command.
    if name.contains(char::is_whitespace) {
        return Vec::new();
    }
    let leading = &command[..command.len() - name.len()];
    let query = name.to_ascii_lowercase();
    COMMAND_NAMES
        .iter()
        .filter(|candidate| candidate.starts_with(&query))
        .map(|candidate| format!("{leading}{candidate}"))
        .collect()
}
