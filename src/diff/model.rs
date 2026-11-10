//! Read-only comparisons, independent of Git processes and GPUI.
use std::{ops::Range, path::PathBuf, time::Duration};

use similar::{DiffTag, TextDiff};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LocalFilter {
    All,
    Staged,
    Unstaged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DiffRequest {
    Local(LocalFilter),
    PullRequest,
}

#[derive(Clone, Debug)]
pub(crate) enum DiffSource {
    Local(LocalFilter),
    /// The opening PR's merge base stays fixed; the other side is the saved worktree.
    PullRequest {
        number: u64,
        merge_base_oid: String,
        head_oid: String,
        url: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RepositoryWatchTarget {
    pub worktree: PathBuf,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
}

impl DiffSource {
    pub(crate) fn label(&self) -> String {
        match self {
            Self::Local(LocalFilter::All) => "Diff".into(),
            Self::Local(LocalFilter::Staged) => "Diff · staged".into(),
            Self::Local(LocalFilter::Unstaged) => "Diff · unstaged".into(),
            Self::PullRequest { number, .. } => format!("Diff · PR #{number}"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum DiffSection {
    Staged,
    Unstaged,
    PullRequest,
}

impl DiffSection {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Staged => "Staged",
            Self::Unstaged => "Unstaged",
            Self::PullRequest => "PR",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChangeKind {
    Added,
    Deleted,
    Modified,
    Renamed,
    Copied,
    TypeChanged,
    Untracked,
    Conflict,
}

impl ChangeKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Added => "A",
            Self::Deleted => "D",
            Self::Modified => "M",
            Self::Renamed => "R",
            Self::Copied => "C",
            Self::TypeChanged => "T",
            Self::Untracked => "?",
            Self::Conflict => "U",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DiffEntry {
    pub section: DiffSection,
    pub old_path: Option<PathBuf>,
    pub new_path: Option<PathBuf>,
    pub kind: ChangeKind,
}

impl DiffEntry {
    pub(crate) fn path(&self) -> &std::path::Path {
        self.new_path
            .as_deref()
            .or(self.old_path.as_deref())
            .expect("diff entry has a path")
    }

    pub(crate) fn label(&self) -> String {
        match (&self.old_path, &self.new_path) {
            (Some(old), Some(new)) if old != new => {
                format!("{} → {}", old.display(), new.display())
            }
            _ => self.path().display().to_string(),
        }
    }

    pub(crate) fn same_file(&self, other: &Self) -> bool {
        self.section == other.section && self.path() == other.path()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct DiffSnapshot {
    pub root: PathBuf,
    pub source: DiffSource,
    pub entries: Vec<DiffEntry>,
}

#[derive(Clone, Debug)]
pub(crate) enum FileContent {
    Missing,
    Text(String),
    Binary,
    Gitlink(String),
}

#[derive(Clone, Debug)]
pub(crate) struct FileVersion {
    pub mode: Option<u32>,
    pub content: FileContent,
}

impl FileVersion {
    pub(crate) fn missing() -> Self {
        Self {
            mode: None,
            content: FileContent::Missing,
        }
    }

    pub(crate) fn text(&self) -> Option<&str> {
        match &self.content {
            FileContent::Missing => Some(""),
            FileContent::Text(text) => Some(text),
            _ => None,
        }
    }

    pub(crate) fn description(&self) -> String {
        let kind = match &self.content {
            FileContent::Missing => return "Not present".into(),
            FileContent::Binary => "Binary / non-UTF-8 content".into(),
            FileContent::Gitlink(oid) => format!("Submodule commit {oid}"),
            FileContent::Text(text) => {
                let ending = if text.contains("\r\n") { "CRLF" } else { "LF" };
                let tail = if !text.is_empty() && !text.ends_with('\n') {
                    " · no final newline"
                } else {
                    ""
                };
                format!("{} bytes · {ending}{tail}", text.len())
            }
        };
        match self.mode {
            Some(mode) => format!("{mode:06o} · {kind}"),
            None => kind,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct DiffHunk {
    pub old_lines: Range<usize>,
    pub new_lines: Range<usize>,
}

#[derive(Clone, Debug)]
pub(crate) struct DiffDocument {
    pub before: FileVersion,
    pub after: FileVersion,
    pub hunks: Vec<DiffHunk>,
}

impl DiffDocument {
    pub(crate) fn new(before: FileVersion, after: FileVersion) -> Self {
        let hunks = match (before.text(), after.text()) {
            (Some(before), Some(after)) => build_hunks(before, after),
            _ => Vec::new(),
        };
        Self {
            before,
            after,
            hunks,
        }
    }
}

fn build_hunks(before: &str, after: &str) -> Vec<DiffHunk> {
    // Bound pathological comparisons. Similar falls back to larger (but valid)
    // replacement blocks when its deadline is reached.
    TextDiff::configure()
        .timeout(Duration::from_secs(2))
        .diff_lines(before, after)
        .ops()
        .iter()
        .filter(|op| op.tag() != DiffTag::Equal)
        .map(|op| DiffHunk {
            old_lines: op.old_range(),
            new_lines: op.new_range(),
        })
        .collect()
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum IndexAction {
    Stage,
    Unstage,
}

impl IndexAction {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Stage => "Staging…",
            Self::Unstage => "Unstaging…",
        }
    }
}
