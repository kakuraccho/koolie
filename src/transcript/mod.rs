//! Chat: read an agent's own transcript and shape it into turns.
//!
//! Claude Code and Codex both append one JSON object per line to a file on
//! the host while they work. [`locate`] finds that file for the session herdr
//! reports, [`read_from`] reads what was appended, and [`TranscriptParser`]
//! turns the bytes into a [`Conversation`]. [`Follower`] ties the three
//! together for a caller that just polls.
//!
//! Format-specific code lives in `claude` and `codex`; the UI depends only
//! on the public items of this module.

mod claude;
mod codex;
mod files;
mod util;

pub use files::{READ_CAP, file_len, locate, read_from};

use crate::transport::Transport;
use serde_json::Value;

/// A line longer than this is skipped instead of buffered. Real transcripts
/// stay far below it (the largest lines seen hold a few MB of tool output).
const MAX_LINE_BYTES: usize = 64 * 1024 * 1024;

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
    /// Final assistant text of the turn, once known. While the turn runs this
    /// is the agent's latest text, and it moves into `steps` as soon as
    /// anything follows it.
    pub answer: Option<String>,
    /// True while the turn has not ended. Only the last turn can be in
    /// progress. A program that died mid-turn leaves it true, so combine it
    /// with the agent's live status.
    pub in_progress: bool,
    /// Unix seconds, when known.
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    /// The user stopped the turn before the agent finished it.
    pub interrupted: bool,
}

impl Turn {
    /// Add assistant text. It is the answer until something follows it.
    pub(crate) fn push_text(&mut self, text: String) {
        self.demote_answer();
        self.answer = Some(text);
    }

    /// Add a step after everything so far. Consecutive thinking markers
    /// collapse into one.
    pub(crate) fn push_step(&mut self, step: Step) {
        self.demote_answer();
        if step == Step::Thinking && self.steps.last() == Some(&Step::Thinking) {
            return;
        }
        self.steps.push(step);
    }

    fn demote_answer(&mut self) {
        if let Some(text) = self.answer.take() {
            self.steps.push(Step::Text(text));
        }
    }

    /// True when nothing but the prompt has been recorded.
    pub(crate) fn is_empty(&self) -> bool {
        self.steps.is_empty() && self.answer.is_none()
    }
}

/// A whole conversation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Conversation {
    pub turns: Vec<Turn>,
    /// The conversation's title when the program records one (Claude Code:
    /// the user's custom title, else the generated one).
    pub title: Option<String>,
    /// The model that answered most recently.
    pub model: Option<String>,
}

#[derive(Debug)]
enum Format {
    Claude(claude::ClaudeParser),
    Codex(codex::CodexParser),
}

/// Incremental transcript parser: feed appended bytes, read the conversation.
///
/// Feeding a file in any chunking gives the same conversation as feeding it
/// whole. Lines that are not JSON, entry types the parser does not know and
/// fields it does not read are skipped.
#[derive(Debug)]
pub struct TranscriptParser {
    harness: Harness,
    format: Format,
    /// The bytes after the last newline: a line still being written.
    buf: Vec<u8>,
    /// Dropping an oversized line until its newline arrives.
    skipping: bool,
}

impl TranscriptParser {
    pub fn new(harness: Harness) -> Self {
        Self {
            harness,
            format: match harness {
                Harness::Claude => Format::Claude(claude::ClaudeParser::default()),
                Harness::Codex => Format::Codex(codex::CodexParser::default()),
            },
            buf: Vec::new(),
            skipping: false,
        }
    }

    pub fn harness(&self) -> Harness {
        self.harness
    }

    /// Forget everything, as when the file was replaced and is read again
    /// from its start.
    pub fn reset(&mut self) {
        *self = Self::new(self.harness);
    }

    /// Feed bytes appended to the transcript since the last call. Partial
    /// trailing lines are buffered until completed.
    pub fn feed(&mut self, bytes: &[u8]) {
        let mut rest = bytes;
        while let Some(pos) = rest.iter().position(|&b| b == b'\n') {
            let (head, tail) = rest.split_at(pos);
            rest = &tail[1..];
            if self.skipping {
                self.skipping = false;
            } else if self.buf.is_empty() {
                self.line(head);
            } else {
                self.buf.extend_from_slice(head);
                let line = std::mem::take(&mut self.buf);
                self.line(&line);
            }
        }
        if !self.skipping {
            self.buf.extend_from_slice(rest);
            if self.buf.len() > MAX_LINE_BYTES {
                self.buf = Vec::new();
                self.skipping = true;
            }
        }
    }

    fn line(&mut self, line: &[u8]) {
        if line.len() > MAX_LINE_BYTES {
            return;
        }
        let line = line.trim_ascii();
        if line.is_empty() {
            return;
        }
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            return;
        };
        match &mut self.format {
            Format::Claude(p) => p.line(&value),
            Format::Codex(p) => p.line(&value),
        }
    }

    pub fn conversation(&self) -> &Conversation {
        match &self.format {
            Format::Claude(p) => p.conversation(),
            Format::Codex(p) => p.conversation(),
        }
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

/// Follows one transcript file: the path, how far it was read, and the
/// parser, kept together so a caller only has to [`poll`](Follower::poll).
#[derive(Debug)]
pub struct Follower {
    path: String,
    offset: u64,
    parser: TranscriptParser,
}

impl Follower {
    /// Follow the file at `path` (as [`locate`] returned it) from its start.
    pub fn new(harness: Harness, path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            offset: 0,
            parser: TranscriptParser::new(harness),
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    /// Bytes of the file read so far.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn conversation(&self) -> &Conversation {
        self.parser.conversation()
    }

    /// Read everything appended since the last poll. Returns whether
    /// anything was read, so whether the conversation may have changed.
    ///
    /// A file that became shorter than what was already read was rewritten
    /// (Claude Code does that when it removes a record): the conversation is
    /// then rebuilt from the start of the file.
    pub async fn poll(&mut self, transport: &dyn Transport) -> Result<bool, TranscriptError> {
        self.poll_with(transport, !transport.is_local()).await
    }

    async fn poll_with(
        &mut self,
        transport: &dyn Transport,
        use_exec: bool,
    ) -> Result<bool, TranscriptError> {
        let mut changed = false;
        loop {
            match files::read_chunk(transport, &self.path, self.offset, use_exec).await? {
                files::Chunk::Shrunk => {
                    self.parser.reset();
                    self.offset = 0;
                    changed = true;
                }
                files::Chunk::Data(bytes) => {
                    if bytes.is_empty() {
                        return Ok(changed);
                    }
                    self.parser.feed(&bytes);
                    self.offset += bytes.len() as u64;
                    changed = true;
                    if (bytes.len() as u64) < READ_CAP {
                        return Ok(changed);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
