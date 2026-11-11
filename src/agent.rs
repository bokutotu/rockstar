use std::{io, os::unix::process::CommandExt as _, path::PathBuf, process};

use async_channel::{Receiver, Sender};
use chrono::{DateTime, Utc};

pub type AgentError = anyhow::Error;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum AgentKind {
    #[default]
    Pi,
    Agy,
    Codex,
}

impl AgentKind {
    pub(crate) const ALL: [Self; 3] = [Self::Pi, Self::Agy, Self::Codex];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Pi => "pi",
            Self::Agy => "agy",
            Self::Codex => "codex",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Pi => "Pi",
            Self::Agy => "Agy",
            Self::Codex => "Codex",
        }
    }

    pub(crate) fn disconnected_message(self) -> String {
        format!(
            "{} is no longer connected. Use :resume to reopen a saved chat or :new to start a new one.",
            self.name()
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ResumeTarget {
    Pi(PathBuf),
    Agy(String),
    Codex(String),
}

impl ResumeTarget {
    pub(crate) fn kind(&self) -> AgentKind {
        match self {
            Self::Pi(_) => AgentKind::Pi,
            Self::Agy(_) => AgentKind::Agy,
            Self::Codex(_) => AgentKind::Codex,
        }
    }
}

/// A saved conversation offered by the resume picker.
pub(crate) struct SavedSession {
    pub target: ResumeTarget,
    pub detail: String,
    pub title: String,
    pub updated_at: DateTime<Utc>,
}

impl SavedSession {
    pub(crate) fn kind(&self) -> AgentKind {
        self.target.kind()
    }
}

#[derive(Clone, Debug)]
pub(crate) enum ChatStart {
    New,
    Resume(ResumeTarget),
}

/// A connection whose initial history has been loaded successfully.
pub(crate) struct OpenedChat {
    pub agent: Agent,
    pub messages: Vec<Message>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    User(String),
    Assistant(String),
}

#[derive(Debug)]
pub struct PromptRequest {
    pub text: String,
}

/// One ordered stream keeps cancellation behind earlier submissions and ahead
/// of prompts explicitly submitted after it.
#[derive(Debug)]
pub enum AgentCommand {
    Prompt(PromptRequest),
    Cancel,
}

/// Current context-window usage, not cumulative session token consumption.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextUsage {
    pub used_tokens: u64,
    pub limit_tokens: u64,
}

#[derive(Debug)]
pub enum AgentEvent {
    UserMessage(String),
    AssistantStarted,
    AssistantTextDelta(String),
    AssistantFinished(String),
    ToolActivity(String),
    ContextUsageUpdated(Option<ContextUsage>),
    PromptFinished,
    PromptCancelled,
    PendingPromptsReturned(Vec<String>),
    Error(AgentError),
}

pub struct Agent {
    commands: Sender<AgentCommand>,
    results: Option<Receiver<AgentEvent>>,
}

pub(crate) struct AgentBackend {
    pub commands: Receiver<AgentCommand>,
    pub events: Sender<AgentEvent>,
}

pub(crate) fn channel() -> (Agent, AgentBackend) {
    let (command_sender, commands) = async_channel::unbounded();
    let (result_sender, results) = async_channel::unbounded();
    (
        Agent {
            commands: command_sender,
            results: Some(results),
        },
        AgentBackend {
            commands,
            events: result_sender,
        },
    )
}

impl Agent {
    pub fn command_sender(&self) -> Sender<AgentCommand> {
        self.commands.clone()
    }

    pub fn take_results(&mut self) -> Option<Receiver<AgentEvent>> {
        self.results.take()
    }
}

pub(crate) fn arm_parent_death_signal(command: &mut process::Command) {
    let parent_pid = process::id() as libc::pid_t;
    // SAFETY: The child-side hook only makes syscalls and constructs OS-code
    // errors; it does not allocate or acquire locks after fork.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(
                libc::PR_SET_PDEATHSIG,
                libc::SIGKILL as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
            ) == -1
            {
                return Err(io::Error::last_os_error());
            }
            // Do not start the agent if Rockstar exited before the signal was armed.
            if libc::getppid() != parent_pid {
                return Err(io::Error::from_raw_os_error(libc::ESRCH));
            }
            Ok(())
        });
    }
}

pub(crate) fn return_pending_prompts(backend: &AgentBackend, prompts: Vec<String>) {
    if !prompts.is_empty() {
        let _ = backend
            .events
            .try_send(AgentEvent::PendingPromptsReturned(prompts));
    }
}

/// Preserve ordinary spaces, including those inside quoted shell arguments,
/// while replacing line breaks and control characters so activity stays single-line.
pub(crate) fn sanitize_activity(text: &str) -> String {
    text.replace("\r\n", "\n")
        .chars()
        .map(|ch| {
            if ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}') {
                ' '
            } else {
                ch
            }
        })
        .collect()
}

pub(crate) fn summarize_title(raw: Option<&str>) -> String {
    let title = raw
        .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "(no messages)".into());
    title.chars().take(160).collect()
}
