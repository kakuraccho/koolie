//! Agent notifications: decide when an agent crossed into a status worth
//! telling the user about, and deliver it.

use crate::config::{NotifyConfig, Sink};
use crate::model::{Agent, AgentKey, AgentStatus, HostId, Inventory};
use std::collections::{HashMap, HashSet};

/// One thing to tell the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    pub key: AgentKey,
    pub status: AgentStatus,
    pub program: String,
    pub title: String,
}

impl Notification {
    pub fn summary(&self) -> String {
        let what = match self.status {
            AgentStatus::Blocked => "入力待ち",
            AgentStatus::Done => "完了",
            other => other.as_str(),
        };
        format!("{} が{what} ({})", self.program, self.key.host)
    }
}

/// Tracks the last status of every agent and yields notifications for
/// transitions. The first inventory of a host after (re)connecting only primes
/// the tracker: what happened while koolie was away is not news.
#[derive(Debug, Default)]
pub struct Tracker {
    on: Vec<AgentStatus>,
    last: HashMap<AgentKey, AgentStatus>,
    primed: HashSet<HostId>,
}

impl Tracker {
    pub fn new(cfg: &NotifyConfig) -> Self {
        Self {
            on: cfg.on.clone(),
            ..Default::default()
        }
    }

    /// Forget a host's state (call when its connection drops) so the next
    /// inventory primes again.
    pub fn reset_host(&mut self, host: &HostId) {
        self.primed.remove(host);
        self.last.retain(|k, _| &k.host != host);
    }

    /// Observe a host's new inventory. `viewing` is the agent the user is
    /// looking at right now; it is never notified.
    pub fn observe(
        &mut self,
        host: &HostId,
        inv: &Inventory,
        viewing: Option<&AgentKey>,
    ) -> Vec<Notification> {
        let primed = self.primed.contains(host);
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for a in &inv.agents {
            seen.insert(a.key.clone());
            let prev = self.last.insert(a.key.clone(), a.status);
            if !primed || prev == Some(a.status) || !self.on.contains(&a.status) {
                continue;
            }
            // Done right after Blocked means the user answered and the turn
            // ended; still worth telling. Blocked -> Blocked never reaches here.
            if Some(&a.key) == viewing {
                continue;
            }
            out.push(notification(a));
        }
        self.last.retain(|k, _| &k.host != host || seen.contains(k));
        self.primed.insert(host.clone());
        out
    }
}

fn notification(a: &Agent) -> Notification {
    Notification {
        key: a.key.clone(),
        status: a.status,
        program: a.program().to_string(),
        title: a.display_title(),
    }
}

/// Deliver to the configured sinks other than the bell (the TUI rings that
/// itself). Never blocks the caller.
pub fn deliver(cfg: &NotifyConfig, n: &Notification) {
    if cfg.sinks.contains(&Sink::Desktop) {
        let summary = n.summary();
        let body = n.title.clone();
        tokio::task::spawn_blocking(move || {
            let _ = notify_rust::Notification::new()
                .appname("koolie")
                .summary(&summary)
                .body(&body)
                .show();
        });
    }
    if cfg.sinks.contains(&Sink::Command)
        && let Some(cmd) = cfg.command.clone()
    {
        let n = n.clone();
        tokio::spawn(async move {
            let _ = tokio::process::Command::new("sh")
                .arg("-c")
                .arg(&cmd)
                .env("KOOLIE_HOST", &n.key.host)
                .env("KOOLIE_ID", &n.key.id)
                .env("KOOLIE_AGENT", &n.program)
                .env("KOOLIE_STATUS", n.status.as_str())
                .env("KOOLIE_TITLE", &n.title)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(false)
                .status()
                .await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Agent;

    fn inv(statuses: &[(&str, AgentStatus)]) -> Inventory {
        Inventory {
            server_version: None,
            agents: statuses
                .iter()
                .map(|(id, s)| Agent {
                    key: AgentKey {
                        host: "h".into(),
                        id: (*id).into(),
                    },
                    agent: Some("claude".into()),
                    display_agent: None,
                    title: Some("t".into()),
                    status: *s,
                    workspace_id: "w".into(),
                    workspace_label: None,
                    tab_id: None,
                    cwd: None,
                    revision: 0,
                    state_change_seq: 0,
                    session: None,
                    focused: false,
                })
                .collect(),
        }
    }

    #[test]
    fn first_inventory_primes_only() {
        let mut t = Tracker::new(&NotifyConfig::default());
        let h = "h".to_string();
        assert!(
            t.observe(&h, &inv(&[("a", AgentStatus::Blocked)]), None)
                .is_empty()
        );
        assert!(
            t.observe(&h, &inv(&[("a", AgentStatus::Blocked)]), None)
                .is_empty()
        );
    }

    #[test]
    fn transitions_notify_once() {
        let mut t = Tracker::new(&NotifyConfig::default());
        let h = "h".to_string();
        t.observe(
            &h,
            &inv(&[("a", AgentStatus::Working), ("b", AgentStatus::Idle)]),
            None,
        );
        let n = t.observe(
            &h,
            &inv(&[("a", AgentStatus::Blocked), ("b", AgentStatus::Idle)]),
            None,
        );
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].status, AgentStatus::Blocked);
        assert_eq!(n[0].summary(), "claude が入力待ち (h)");
        assert!(
            t.observe(&h, &inv(&[("a", AgentStatus::Blocked)]), None)
                .is_empty()
        );
        let n = t.observe(&h, &inv(&[("a", AgentStatus::Done)]), None);
        assert_eq!(n[0].status, AgentStatus::Done);
        // Working and idle are not in the default `on` list.
        assert!(
            t.observe(&h, &inv(&[("a", AgentStatus::Working)]), None)
                .is_empty()
        );
    }

    #[test]
    fn viewing_agent_is_silent_and_new_blocked_agent_notifies() {
        let mut t = Tracker::new(&NotifyConfig::default());
        let h = "h".to_string();
        t.observe(&h, &inv(&[("a", AgentStatus::Working)]), None);
        let viewing = AgentKey {
            host: "h".into(),
            id: "a".into(),
        };
        assert!(
            t.observe(&h, &inv(&[("a", AgentStatus::Blocked)]), Some(&viewing))
                .is_empty()
        );
        let n = t.observe(
            &h,
            &inv(&[("a", AgentStatus::Blocked), ("c", AgentStatus::Blocked)]),
            None,
        );
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].key.id, "c");
    }

    #[test]
    fn reset_host_primes_again() {
        let mut t = Tracker::new(&NotifyConfig::default());
        let h = "h".to_string();
        t.observe(&h, &inv(&[("a", AgentStatus::Working)]), None);
        t.reset_host(&h);
        assert!(
            t.observe(&h, &inv(&[("a", AgentStatus::Blocked)]), None)
                .is_empty()
        );
    }
}
