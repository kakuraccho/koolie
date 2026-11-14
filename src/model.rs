//! Shared domain model: hosts, agents and their status.
//!
//! Every backend (Herdr TUI, herdr-core) projects its own data into these
//! types, and the UI only ever sees these.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

/// Stable id of a configured host (the `name` in the config file).
pub type HostId = String;

/// Agent status as herdr reports it.
///
/// The declaration order is the Console's sort priority: the agents that need
/// the user come first.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord, Default,
)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Blocked,
    Done,
    Working,
    Idle,
    #[default]
    #[serde(other)]
    Unknown,
}

impl AgentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentStatus::Blocked => "blocked",
            AgentStatus::Done => "done",
            AgentStatus::Working => "working",
            AgentStatus::Idle => "idle",
            AgentStatus::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> Option<AgentStatus> {
        match s {
            "blocked" => Some(AgentStatus::Blocked),
            "done" => Some(AgentStatus::Done),
            "working" => Some(AgentStatus::Working),
            "idle" => Some(AgentStatus::Idle),
            "unknown" => Some(AgentStatus::Unknown),
            _ => None,
        }
    }

    /// Whether entering this status is worth a notification.
    pub fn is_attention(self) -> bool {
        matches!(self, AgentStatus::Blocked | AgentStatus::Done)
    }
}

/// Which server kind a host runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ServerKind {
    #[default]
    HerdrTui,
    HerdrCore,
}

impl ServerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ServerKind::HerdrTui => "herdr-tui",
            ServerKind::HerdrCore => "herdr-core",
        }
    }
}

/// How herdr names an agent's native session (`agent_session`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionRefKind {
    Id,
    Path,
}

/// The agent program's own session reference, used to find its transcript.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSession {
    /// Agent program name as herdr reports it, e.g. `claude`, `codex`.
    pub agent: String,
    pub kind: SessionRefKind,
    pub value: String,
}

/// Globally unique agent key: host + backend-scoped id (a pane id on Herdr
/// TUI, a terminal id on herdr-core).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AgentKey {
    pub host: HostId,
    pub id: String,
}

/// One agent as the Console shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agent {
    pub key: AgentKey,
    /// Program name (`claude`, `codex`, ...), if herdr detected one.
    pub agent: Option<String>,
    /// Display name override reported via metadata.
    pub display_agent: Option<String>,
    /// User- or agent-given name/title.
    pub title: Option<String>,
    pub status: AgentStatus,
    pub workspace_id: String,
    pub workspace_label: Option<String>,
    pub tab_id: Option<String>,
    pub cwd: Option<String>,
    /// Monotonic per-pane output revision.
    pub revision: u64,
    /// Monotonic per-server state change sequence; higher = changed later.
    pub state_change_seq: u64,
    pub session: Option<AgentSession>,
    pub focused: bool,
}

impl Agent {
    /// Short name of the program for display.
    pub fn program(&self) -> &str {
        self.display_agent
            .as_deref()
            .or(self.agent.as_deref())
            .unwrap_or("terminal")
    }

    /// Best human title for the row.
    pub fn display_title(&self) -> String {
        if let Some(t) = self
            .title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            return t.to_string();
        }
        if let Some(cwd) = &self.cwd {
            return short_path(cwd);
        }
        self.key.id.clone()
    }
}

/// Everything one host currently has.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inventory {
    pub server_version: Option<String>,
    pub agents: Vec<Agent>,
}

/// Console ordering: status priority, then most recently changed first, then
/// host and id for stability.
pub fn console_order(a: &Agent, b: &Agent) -> Ordering {
    a.status
        .cmp(&b.status)
        .then_with(|| b.state_change_seq.cmp(&a.state_change_seq))
        .then_with(|| a.key.cmp(&b.key))
}

/// `/home/me/dev/proj` -> `~/dev/proj` style shortening; keeps the last two
/// components when the path is long.
pub fn short_path(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    let mut home_rel = None;
    if let Some(home) = std::env::var_os("HOME").and_then(|h| h.into_string().ok())
        && !home.is_empty()
        && let Some(rest) = trimmed.strip_prefix(home.trim_end_matches('/'))
        && (rest.is_empty() || rest.starts_with('/'))
    {
        home_rel = Some(format!("~{rest}"));
    }
    let p = home_rel.unwrap_or_else(|| trimmed.to_string());
    let parts: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() > 3 {
        format!("…/{}/{}", parts[parts.len() - 2], parts[parts.len() - 1])
    } else if p.is_empty() {
        "/".to_string()
    } else {
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str, status: AgentStatus, seq: u64) -> Agent {
        Agent {
            key: AgentKey {
                host: "h".into(),
                id: id.into(),
            },
            agent: Some("claude".into()),
            display_agent: None,
            title: None,
            status,
            workspace_id: "w1".into(),
            workspace_label: None,
            tab_id: None,
            cwd: None,
            revision: 0,
            state_change_seq: seq,
            session: None,
            focused: false,
        }
    }

    #[test]
    fn status_parses_and_unknown_falls_back() {
        let s: AgentStatus = serde_json::from_str("\"blocked\"").unwrap();
        assert_eq!(s, AgentStatus::Blocked);
        let s: AgentStatus = serde_json::from_str("\"something_new\"").unwrap();
        assert_eq!(s, AgentStatus::Unknown);
    }

    #[test]
    fn console_order_puts_blocked_then_done_first_and_recent_first() {
        let mut v = [
            agent("a", AgentStatus::Idle, 9),
            agent("b", AgentStatus::Working, 1),
            agent("c", AgentStatus::Blocked, 2),
            agent("d", AgentStatus::Done, 3),
            agent("e", AgentStatus::Blocked, 5),
            agent("f", AgentStatus::Unknown, 7),
        ];
        v.sort_by(console_order);
        let ids: Vec<&str> = v.iter().map(|a| a.key.id.as_str()).collect();
        assert_eq!(ids, vec!["e", "c", "d", "b", "a", "f"]);
    }

    #[test]
    fn short_path_shortens() {
        assert_eq!(short_path("/a/b"), "/a/b");
        assert_eq!(short_path("/a/b/c/d/e"), "…/d/e");
        assert_eq!(short_path("/"), "/");
    }
}
