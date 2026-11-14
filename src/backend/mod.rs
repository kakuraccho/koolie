//! Server kinds behind one interface.
//!
//! A [`Backend`] turns one server's API (Herdr TUI's socket API, herdr-core's
//! JSON-RPC) into koolie's model. Sessions and the UI only use this trait.

pub mod herdr_core;
pub mod herdr_tui;
pub mod jsonl;

use crate::model::{Inventory, ServerKind};
use crate::transport::TransportError;
use async_trait::async_trait;
use tokio::sync::mpsc;

/// Which part of the terminal to read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadSource {
    /// What is on screen now.
    Visible,
    /// Recent scrollback plus screen.
    Recent,
    /// Recent, with soft wraps joined.
    RecentUnwrapped,
}

impl ReadSource {
    pub fn as_api(self) -> &'static str {
        match self {
            ReadSource::Visible => "visible",
            ReadSource::Recent => "recent",
            ReadSource::RecentUnwrapped => "recent_unwrapped",
        }
    }
}

/// Terminal contents read from an agent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Screen {
    pub text: String,
    /// Output revision the text was read at.
    pub revision: u64,
    /// Whether `text` contains ANSI escape sequences.
    pub ansi: bool,
    pub truncated: bool,
}

/// Something changed on the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackendEvent {
    /// Anything that may change the inventory; refetch the snapshot.
    Changed,
    /// An agent's status changed (also implies [`BackendEvent::Changed`]).
    StatusChanged {
        id: String,
        status: crate::model::AgentStatus,
    },
}

/// What a server is, as learned on connect.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerInfo {
    pub version: String,
    pub protocol: Option<u64>,
}

/// Optional features a backend supports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// `prompt` submits a whole message at once.
    pub prompt: bool,
    /// `insert_text` types text without submitting.
    pub insert_text: bool,
    /// Agents carry `agent_session`, so Chat can find transcripts.
    pub sessions: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BackendError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    /// The server answered with an error.
    #[error("{code}: {message}")]
    Server { code: String, message: String },
    /// The server does not have this method (older version).
    #[error("not supported by this server: {0}")]
    Unsupported(String),
    /// The answer did not parse.
    #[error("protocol error: {0}")]
    Protocol(String),
    /// The event stream fell behind and was closed by the server.
    #[error("events lost")]
    EventsLost,
}

impl BackendError {
    pub fn code(&self) -> Option<&str> {
        match self {
            BackendError::Server { code, .. } => Some(code),
            _ => None,
        }
    }

    /// Whether a retry of the whole connection may help.
    pub fn is_retryable(&self) -> bool {
        match self {
            BackendError::Transport(t) => t.is_retryable(),
            BackendError::EventsLost => true,
            BackendError::Protocol(_) => true,
            BackendError::Server { .. } | BackendError::Unsupported(_) => false,
        }
    }
}

pub type Result<T> = std::result::Result<T, BackendError>;

#[async_trait]
pub trait Backend: Send + Sync + std::fmt::Debug {
    fn kind(&self) -> ServerKind;

    fn capabilities(&self) -> Capabilities;

    /// Check the server answers; returns its version.
    async fn hello(&self) -> Result<ServerInfo>;

    /// The full current inventory.
    async fn snapshot(&self) -> Result<Inventory>;

    /// Subscribe to change notifications. `ids` are the agent ids currently
    /// known, for servers whose status events are per agent. The stream ends
    /// (sender dropped) when the subscription breaks; the last item before the
    /// end may be an error.
    async fn subscribe(
        &self,
        ids: &[String],
    ) -> Result<mpsc::Receiver<std::result::Result<BackendEvent, BackendError>>>;

    /// Read an agent's terminal.
    async fn read(
        &self,
        id: &str,
        source: ReadSource,
        lines: Option<u32>,
        ansi: bool,
    ) -> Result<Screen>;

    /// Submit a whole prompt (text + Enter) to the agent.
    async fn prompt(&self, id: &str, text: &str) -> Result<()>;

    /// Press keys (`enter`, `esc`, `up`, `1`, `ctrl+c`, ...).
    async fn send_keys(&self, id: &str, keys: &[String]) -> Result<()>;

    /// Type text into the agent's input without submitting.
    async fn insert_text(&self, id: &str, text: &str) -> Result<()>;

    /// Command (to run *on the host*) that attaches the user's terminal to
    /// the agent.
    fn attach_argv(&self, id: &str) -> Vec<String>;
}
