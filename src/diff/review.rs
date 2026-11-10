//! Review drafts use immutable source fingerprints, never just a moving line number.
use super::{git, model::*};
use anyhow::{Context as _, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Side {
    Left,
    Right,
}

impl Side {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Left => "LEFT",
            Self::Right => "RIGHT",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Target {
    pub entry: DiffEntry,
    pub side: Side,
    pub line: usize,
    pub source_line: String,
    fingerprint: u64,
}

fn fingerprint(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

impl Target {
    pub(super) fn new(entry: DiffEntry, side: Side, index: usize, text: &str) -> Self {
        Self {
            entry,
            side,
            line: index + 1,
            source_line: text.lines().nth(index).unwrap_or_default().to_owned(),
            fingerprint: fingerprint(text),
        }
    }

    fn path(&self) -> &PathBuf {
        match self.side {
            Side::Left => self.entry.old_path.as_ref(),
            Side::Right => self.entry.new_path.as_ref(),
        }
        .expect("comment target has a source path")
    }

    pub(super) fn label(&self) -> String {
        format!(
            "{}:{} · {} · {}",
            self.path().display(),
            self.line,
            self.side.label(),
            self.entry.section.label()
        )
    }
}

#[derive(Clone, Debug)]
pub(super) struct Comment {
    pub id: u64,
    pub target: Target,
    pub body: String,
}

pub(super) fn markdown(comments: &[Comment]) -> String {
    let mut body = String::new();
    for comment in comments {
        body.push_str(&format!(
            "### {}\n\n{}\n\n",
            comment.target.label(),
            comment.body
        ));
        // Indented code cannot be terminated by a source line containing backticks.
        body.push_str(&format!("    {}\n\n", comment.target.source_line));
    }
    body
}

/// Only attach to GitHub lines if the *whole* reviewed side still matches the
/// remote source, and the line belongs to a displayed patch hunk. Other comments
/// remain in the review body, explicitly labeled as local/snapshot comments.
fn inline(comment: &Comment, entry: &DiffEntry, document: &DiffDocument) -> Option<Value> {
    let target = &comment.target;
    let (path, text) = match target.side {
        Side::Left => (entry.old_path.as_ref()?, document.before.text()?),
        Side::Right => (entry.new_path.as_ref()?, document.after.text()?),
    };
    if path != target.path() || fingerprint(text) != target.fingerprint || target.line == 0 {
        return None;
    }
    let index = target.line - 1;
    if index >= text.lines().count()
        || !document.hunks.iter().any(|hunk| {
            let lines = match target.side {
                Side::Left => &hunk.old_lines,
                Side::Right => &hunk.new_lines,
            };
            (lines.start.saturating_sub(3)..lines.end.saturating_add(3)).contains(&index)
        })
    {
        return None;
    }
    Some(json!({
        "path": entry.path().to_str()?, "line": target.line,
        "side": target.side.label(), "body": comment.body,
    }))
}

pub(super) fn submit(snapshot: &DiffSnapshot, comments: &[Comment]) -> Result<String> {
    ensure!(!comments.is_empty(), "Add a review comment first");
    ensure!(
        comments.len() <= 100,
        "Submit at most 100 review comments at once"
    );
    let original_url = match &snapshot.source {
        DiffSource::PullRequest { url, .. } => Some(url.as_str()),
        _ => None,
    };
    // Resolve again immediately before submission; a local diff uses the PR for
    // the current branch, while a PR diff stays tied to the PR that was opened.
    let remote = git::load_pr_diff(&snapshot.root, original_url)?;
    let DiffSource::PullRequest {
        number,
        merge_base_oid,
        head_oid,
        url,
    } = &remote.source
    else {
        unreachable!()
    };
    if let DiffSource::PullRequest {
        head_oid: original, ..
    } = &snapshot.source
    {
        ensure!(
            original == head_oid,
            "The PR head changed. Reopen :prdiff before submitting; your draft is retained."
        );
    }
    let entries = git::changes(
        &snapshot.root,
        &[merge_base_oid, head_oid],
        DiffSection::PullRequest,
    )?;
    let mut documents = Vec::new();
    for entry in &entries {
        // Don't fetch blobs unrelated to the draft.
        if !comments.iter().any(|comment| {
            entry.old_path.as_ref() == Some(comment.target.path())
                || entry.new_path.as_ref() == Some(comment.target.path())
        }) {
            continue;
        }
        let before = entry
            .old_path
            .as_ref()
            .map(|path| git::tree_file(&snapshot.root, merge_base_oid, path))
            .transpose()?
            .unwrap_or_else(FileVersion::missing);
        let after = entry
            .new_path
            .as_ref()
            .map(|path| git::tree_file(&snapshot.root, head_oid, path))
            .transpose()?
            .unwrap_or_else(FileVersion::missing);
        documents.push((entry, DiffDocument::new(before, after)));
    }
    let mut inline_comments = Vec::new();
    let mut local_comments = Vec::new();
    for comment in comments {
        if let Some(value) = documents
            .iter()
            .find_map(|(entry, doc)| inline(comment, entry, doc))
        {
            inline_comments.push(value);
        } else {
            local_comments.push(comment.clone());
        }
    }
    let mut body = "Review from Rockstar.".to_owned();
    if !local_comments.is_empty() {
        body.push_str("\n\n## Local / snapshot comments\n\nThese comments do not match an inline location in the current PR. Line numbers refer to the reviewed snapshot.\n\n");
        body.push_str(&markdown(&local_comments));
    }
    let payload = json!({"commit_id": head_oid, "event": "COMMENT", "body": body, "comments": inline_comments});
    let parsed = url::Url::parse(url)?;
    let host = parsed.host_str().context("PR URL has no host")?;
    let parts: Vec<_> = parsed.path().trim_matches('/').split('/').collect();
    ensure!(parts.len() == 4 && parts[2] == "pull", "Invalid PR URL");
    let endpoint = format!("repos/{}/{}/pulls/{number}/reviews", parts[0], parts[1]);
    let mut command = Command::new("gh");
    command
        .current_dir(&snapshot.root)
        .env("GH_PROMPT_DISABLED", "1")
        .args([
            "api",
            "--hostname",
            host,
            "--method",
            "POST",
            &endpoint,
            "--input",
            "-",
        ]);
    post_review(&mut command, &payload, url)
}

fn post_review(command: &mut Command, payload: &Value, url: &str) -> Result<String> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Cannot start gh to submit review")?;
    let write = child
        .stdin
        .take()
        .context("Missing gh stdin")?
        .write_all(&serde_json::to_vec(payload)?);
    let output = child.wait_with_output()?;
    write?;
    ensure!(
        output.status.success(),
        "Review submission failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    // Once POST succeeds, never report a parse failure as a failed submission:
    // retrying could post the same review twice.
    let response: Value = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
    Ok(response["html_url"].as_str().unwrap_or(url).to_owned())
}
