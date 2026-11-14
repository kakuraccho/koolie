//! Chat: read an agent's own transcript and shape it into turns.
//!
//! Placeholder interface; implemented in M4. The UI depends only on the
//! public items below.

use crate::model::AgentSession;
use crate::transport::Transport;

/// Agent programs whose transcripts koolie can read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Harness {
    Claude,
    Codex,
}

impl Harness {
    /// Harness for herdr's agent name (`claude`, `codex`).
    pub fn from_agent(agent: &str) -> Option<Harness> {
        match agent {
            "claude" | "claude-code" => Some(Harness::Claude),
            "codex" => Some(Harness::Codex),
            _ => None,
        }
    }
}

/// One step inside a turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Assistant text that is not the final answer.
    Text(String),
    /// A tool call: tool name and a one-line summary of its input.
    Tool { name: String, summary: String },
    /// The model was thinking (content not shown).
    Thinking,
}

/// One prompt and everything the agent did in answer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Turn {
    /// What the user (or a notification) sent; `None` for a leading partial turn.
    pub prompt: Option<String>,
    pub steps: Vec<Step>,
    /// Final assistant text of the turn, once known.
    pub answer: Option<String>,
    /// True while the turn has not ended.
    pub in_progress: bool,
    /// Unix seconds, when known.
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
}

/// A whole conversation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Conversation {
    pub turns: Vec<Turn>,
}

/// Incremental transcript parser: feed appended bytes, read the conversation.
#[derive(Debug)]
pub struct TranscriptParser {
    #[allow(dead_code)]
    harness: Harness,
    conversation: Conversation,
}

impl TranscriptParser {
    pub fn new(harness: Harness) -> Self {
        Self {
            harness,
            conversation: Conversation::default(),
        }
    }

    /// Feed bytes appended to the transcript since the last call. Partial
    /// trailing lines are buffered until completed.
    pub fn feed(&mut self, _bytes: &[u8]) {}

    pub fn conversation(&self) -> &Conversation {
        &self.conversation
    }
}

/// Why a transcript can't be shown.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TranscriptError {
    #[error("this agent program has no chat support")]
    UnsupportedAgent,
    #[error(
        "herdr reports no session for this agent; run `herdr integration install <agent>` on the host and restart the agent"
    )]
    NoSession,
    #[error("transcript not found yet")]
    NotFound,
    #[error("{0}")]
    Io(String),
}

/// Find the transcript file for a session on the host.
pub async fn locate(
    _transport: &dyn Transport,
    _session: &AgentSession,
) -> Result<String, TranscriptError> {
    Err(TranscriptError::NotFound)
}

/// Read the transcript from byte `offset` to its end.
pub async fn read_from(
    _transport: &dyn Transport,
    _path: &str,
    _offset: u64,
) -> Result<Vec<u8>, TranscriptError> {
    Ok(Vec::new())
}
