//! Session discovery is file-based: pi's RPC API does not list saved sessions.
//! Actual restoration is left to pi, including migrations and model context.
use std::{
    collections::HashMap,
    env,
    fs::{self, File},
    io::{self, BufRead, BufReader},
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail, ensure};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

use crate::agent::{Message, SavedSession};

use super::PiMessage;

pub(crate) fn list_sessions(root: &Path) -> Result<Vec<SavedSession>> {
    let root = root
        .canonicalize()
        .context("Cannot resolve the workspace directory")?;
    let directory = session_directory(&root)?;
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("Cannot list {}", directory.display()));
        }
    };
    let mut sessions = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path
            .extension()
            .is_none_or(|extension| extension != "jsonl")
        {
            continue;
        }
        match read_summary(&path, &root) {
            Ok(Some(session)) => sessions.push(session),
            Ok(None) => {}
            Err(error) => eprintln!("rockstar: skipping session {}: {error:#}", path.display()),
        }
    }
    sessions.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.path.cmp(&b.path))
    });
    Ok(sessions)
}

fn session_directory(root: &Path) -> Result<PathBuf> {
    if let Some(path) = env::var_os("PI_CODING_AGENT_SESSION_DIR").filter(|path| !path.is_empty()) {
        return expand_path(Path::new(&path), root);
    }
    let agent_dir = match env::var_os("PI_CODING_AGENT_DIR").filter(|path| !path.is_empty()) {
        Some(path) => expand_path(Path::new(&path), root)?,
        None => home_directory()?.join(".pi/agent"),
    };
    // Like pi's startup settings, project settings override global settings,
    // including an explicit null/empty value restoring the default directory.
    let project = read_settings(&root.join(".pi/settings.json"))?;
    let setting = match project.get("sessionDir") {
        Some(value) => value.clone(),
        None => read_settings(&agent_dir.join("settings.json"))?
            .get("sessionDir")
            .cloned()
            .unwrap_or(Value::Null),
    };
    match setting {
        Value::String(path) if !path.is_empty() => return expand_path(Path::new(&path), root),
        Value::Null | Value::String(_) => {}
        _ => bail!("pi's sessionDir setting must be a path string"),
    }
    let encoded = root
        .to_string_lossy()
        .trim_start_matches('/')
        .replace(['/', '\\', ':'], "-");
    Ok(agent_dir.join("sessions").join(format!("--{encoded}--")))
}

fn home_directory() -> Result<PathBuf> {
    env::home_dir().context("Cannot find the home directory for pi sessions")
}

fn expand_path(path: &Path, root: &Path) -> Result<PathBuf> {
    if let Ok(suffix) = path.strip_prefix("~") {
        return Ok(home_directory()?.join(suffix));
    }
    if let Some(path) = path.to_str().filter(|path| path.starts_with("file://")) {
        return url::Url::parse(path)?
            .to_file_path()
            .map_err(|_| anyhow::anyhow!("Invalid session directory URL: {path}"));
    }
    Ok(root.join(path))
}

fn read_settings(path: &Path) -> Result<Value> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(text.trim_start_matches('\u{feff}'))
            .with_context(|| format!("Cannot read pi settings at {}", path.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Value::Null),
        Err(error) => Err(error).with_context(|| format!("Cannot read {}", path.display())),
    }
}

#[derive(Deserialize)]
struct SessionHeader {
    #[serde(rename = "type")]
    kind: String,
    id: String,
    cwd: PathBuf,
    timestamp: Option<String>,
}

fn read_header(reader: &mut impl BufRead) -> Result<SessionHeader> {
    for line in reader.lines() {
        let line = line?;
        // pi tolerates blank/malformed lines, including a partially written tail.
        let Ok(value) = serde_json::from_str::<Value>(line.trim_start_matches('\u{feff}')) else {
            continue;
        };
        let header: SessionHeader =
            serde_json::from_value(value).context("Invalid pi session header")?;
        ensure!(
            header.kind == "session" && !header.id.is_empty(),
            "Invalid pi session header"
        );
        return Ok(header);
    }
    bail!("Missing pi session header")
}

fn belongs_to(header: &SessionHeader, root: &Path) -> bool {
    header.cwd == root || header.cwd.canonicalize().is_ok_and(|cwd| cwd == root)
}

/// Validate before invoking --session, which otherwise can create a new file
/// when the selected session has disappeared. Its ID also verifies RPC startup.
pub(super) fn validate_session(path: &Path, root: &Path) -> Result<String> {
    let file =
        File::open(path).with_context(|| format!("Cannot open session {}", path.display()))?;
    let header = read_header(&mut BufReader::new(file))?;
    let root = root.canonicalize()?;
    ensure!(
        belongs_to(&header, &root),
        "The selected session belongs to another workspace"
    );
    Ok(header.id)
}

fn read_summary(path: &Path, root: &Path) -> Result<Option<SavedSession>> {
    let mut reader = BufReader::new(File::open(path)?);
    let header = read_header(&mut reader)?;
    if !belongs_to(&header, root) {
        return Ok(None);
    }
    let fallback = header
        .timestamp
        .as_deref()
        .and_then(parse_timestamp)
        .unwrap_or(DateTime::<Utc>::from(
            reader.get_ref().metadata()?.modified()?,
        ));
    let mut name = None;
    let mut first_message = None;
    let mut last_activity = None;
    for line in reader.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(&line?) else {
            continue;
        };
        match entry.get("type").and_then(Value::as_str) {
            Some("session_info") => {
                name = entry
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned);
            }
            Some("message") => {
                let message = &entry["message"];
                let role = message.get("role").and_then(Value::as_str);
                if !matches!(role, Some("user" | "assistant")) {
                    continue;
                }
                let timestamp = message
                    .get("timestamp")
                    .and_then(Value::as_i64)
                    .and_then(DateTime::<Utc>::from_timestamp_millis)
                    .or_else(|| {
                        entry
                            .get("timestamp")
                            .and_then(Value::as_str)
                            .and_then(parse_timestamp)
                    });
                if let Some(timestamp) = timestamp.filter(|time| time.timestamp_millis() > 0) {
                    last_activity = Some(
                        last_activity
                            .map_or(timestamp, |previous: DateTime<Utc>| previous.max(timestamp)),
                    );
                }
                if role == Some("user") && first_message.is_none() {
                    let text = content_text(&message["content"]);
                    if !text.trim().is_empty() {
                        first_message = Some(text);
                    }
                }
            }
            _ => {}
        }
    }
    let title = name
        .or(first_message)
        .unwrap_or_else(|| "(no messages)".into());
    // A first prompt can be a whole document; retain only a useful one-line title.
    let title = title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(160)
        .collect();
    Ok(Some(SavedSession {
        path: path.to_path_buf(),
        title,
        updated_at: last_activity.unwrap_or(fallback),
    }))
}

fn parse_timestamp(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|time| time.with_timezone(&Utc))
}

fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect(),
        _ => String::new(),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SessionEntries {
    entries: Vec<SessionEntry>,
    leaf_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionEntry {
    id: String,
    parent_id: Option<String>,
    #[serde(flatten)]
    content: EntryContent,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum EntryContent {
    Message {
        message: PiMessage,
    },
    #[serde(other)]
    Other,
}

impl SessionEntries {
    /// Reconstruct the displayed branch, not the compacted model context and
    /// not append order (which also contains abandoned branches).
    pub(super) fn into_messages(self) -> Result<Vec<Message>> {
        let by_id: HashMap<_, _> = self
            .entries
            .iter()
            .map(|entry| (entry.id.as_str(), entry))
            .collect();
        ensure!(
            by_id.len() == self.entries.len(),
            "pi returned duplicate session entry IDs"
        );
        let mut branch = Vec::new();
        let mut current = self.leaf_id.as_deref();
        while let Some(id) = current {
            ensure!(
                branch.len() < self.entries.len(),
                "pi returned a cyclic session branch"
            );
            let entry = by_id
                .get(id)
                .with_context(|| format!("Missing pi session entry: {id}"))?;
            branch.push(*entry);
            current = entry.parent_id.as_deref();
        }
        Ok(branch
            .into_iter()
            .rev()
            .filter_map(|entry| match &entry.content {
                EntryContent::Message { message } => message.display_message(),
                EntryContent::Other => None,
            })
            .collect())
    }
}
