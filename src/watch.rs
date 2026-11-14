//! `koolie watch` (headless notifier + status file) and `koolie status`.

use crate::app::{ConnectorFactory, real_connectors};
use crate::backend::Backend;
use crate::backend::herdr_tui::HerdrTui;
use crate::config::{Config, HostConfig};
use crate::model::{AgentStatus, HostId, Inventory, console_order};
use crate::notify::{self, Tracker};
use crate::session::{self, ConnState, HostUpdate};
use crate::transport::{LocalTransport, runtime_dir};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StatusFile {
    pub version: u32,
    pub pid: u32,
    /// Unix seconds.
    pub updated_at: i64,
    pub hosts: Vec<HostStatus>,
    pub counts: Counts,
    pub agents: Vec<AgentLine>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HostStatus {
    pub name: String,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    pub blocked: usize,
    pub done: usize,
    pub working: usize,
    pub idle: usize,
    pub unknown: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentLine {
    pub host: String,
    pub id: String,
    pub program: String,
    pub title: String,
    pub status: AgentStatus,
}

/// How long `watch` waits before retrying a host that stopped on its own.
const FAILED_RETRY: Duration = Duration::from_secs(60);
/// The same for authentication / host-key failures.
const FAILED_AUTH_RETRY: Duration = Duration::from_secs(600);

pub fn status_path() -> PathBuf {
    runtime_dir().join("status.json")
}

pub fn build_status(
    hosts: &[(String, ConnState)],
    inventories: &BTreeMap<HostId, Inventory>,
) -> StatusFile {
    let mut agents: Vec<_> = inventories.values().flat_map(|i| i.agents.iter()).collect();
    agents.sort_by(|a, b| console_order(a, b));
    let mut counts = Counts::default();
    for a in &agents {
        match a.status {
            AgentStatus::Blocked => counts.blocked += 1,
            AgentStatus::Done => counts.done += 1,
            AgentStatus::Working => counts.working += 1,
            AgentStatus::Idle => counts.idle += 1,
            AgentStatus::Unknown => counts.unknown += 1,
        }
    }
    StatusFile {
        version: 1,
        pid: std::process::id(),
        updated_at: time::OffsetDateTime::now_utc().unix_timestamp(),
        hosts: hosts
            .iter()
            .map(|(name, s)| HostStatus {
                name: name.clone(),
                state: match s {
                    ConnState::Connected => "connected",
                    ConnState::Connecting => "connecting",
                    ConnState::Reconnecting { .. } => "reconnecting",
                    ConnState::Failed { .. } => "failed",
                }
                .into(),
                error: s.error().map(str::to_string).filter(|e| !e.is_empty()),
            })
            .collect(),
        counts,
        agents: agents
            .iter()
            .map(|a| AgentLine {
                host: a.key.host.clone(),
                id: a.key.id.clone(),
                program: a.program().to_string(),
                title: a.display_title(),
                status: a.status,
            })
            .collect(),
    }
}

fn write_status(s: &StatusFile) {
    let path = status_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("json.tmp");
    if let Ok(text) = serde_json::to_vec_pretty(s)
        && std::fs::write(&tmp, text).is_ok()
    {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Run headless until Ctrl+C: keep every host connected, notify, and keep
/// the status file current.
pub async fn watch(
    cfg: Config,
    hosts: Vec<HostConfig>,
    connectors: Option<ConnectorFactory>,
) -> anyhow::Result<()> {
    let connectors = connectors.unwrap_or_else(real_connectors);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let handles: Vec<_> = hosts
        .iter()
        .map(|h| session::spawn(h.name.clone(), h.kind, connectors(h), tx.clone()))
        .collect();
    drop(tx);
    let mut states: Vec<(String, ConnState)> = hosts
        .iter()
        .map(|h| (h.name.clone(), ConnState::Connecting))
        .collect();
    let mut inventories: BTreeMap<HostId, Inventory> = BTreeMap::new();
    let mut tracker = Tracker::new(&cfg.notify);
    let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
    eprintln!(
        "koolie watch: {} 台のホストを監視します (Ctrl+C で終了)",
        hosts.len()
    );
    write_status(&build_status(&states, &inventories));
    // When each Failed host last failed, for the periodic retry.
    let mut failed_at: BTreeMap<HostId, (tokio::time::Instant, bool)> = BTreeMap::new();
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut hup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    loop {
        tokio::select! {
            u = rx.recv() => {
                let Some(u) = u else { break };
                match u {
                    HostUpdate::State { host, state } => {
                        if !state.is_connected() {
                            tracker.reset_host(&host);
                            // Agents of a host we cannot see are not counted.
                            inventories.remove(&host);
                        }
                        match &state {
                            ConnState::Failed { auth, .. } => {
                                failed_at.insert(host.clone(), (tokio::time::Instant::now(), *auth));
                            }
                            _ => {
                                failed_at.remove(&host);
                            }
                        }
                        if let Some(e) = state.error().filter(|e| !e.is_empty()) {
                            eprintln!("[{host}] {}: {e}", state.label());
                        } else {
                            eprintln!("[{host}] {}", state.label());
                        }
                        if let Some(s) = states.iter_mut().find(|(n, _)| *n == host) {
                            s.1 = state;
                        }
                    }
                    HostUpdate::Inventory { host, inventory } => {
                        for n in tracker.observe(&host, &inventory, None) {
                            println!("{} — {}", n.summary(), n.title);
                            notify::deliver(&cfg.notify, &n);
                        }
                        inventories.insert(host, inventory);
                    }
                    _ => continue,
                }
                write_status(&build_status(&states, &inventories));
            }
            _ = heartbeat.tick() => {
                // Nobody is there to press Reconnect: retry failed hosts on a
                // slow clock (authentication failures much slower, so a
                // locked-out key does not trip fail2ban).
                let now = tokio::time::Instant::now();
                for (host, (at, auth)) in failed_at.iter_mut() {
                    let wait = if *auth { FAILED_AUTH_RETRY } else { FAILED_RETRY };
                    if now.duration_since(*at) >= wait
                        && let Some(h) = handles.iter().find(|h| &h.id == host)
                    {
                        *at = now;
                        h.reconnect();
                    }
                }
                write_status(&build_status(&states, &inventories));
            }
            _ = &mut ctrl_c => break,
            _ = term.recv() => break,
            _ = hup.recv() => break,
        }
    }
    for h in &handles {
        h.shutdown().await;
    }
    let _ = std::fs::remove_file(status_path());
    Ok(())
}

fn pid_alive(pid: u32) -> bool {
    // SAFETY: kill with signal 0 only checks for existence/permission.
    pid != 0 && unsafe { libc::kill(pid as i32, 0) } == 0
}

/// Read the watcher's status file if a watcher is running.
pub fn read_live_status() -> Option<StatusFile> {
    let text = std::fs::read(status_path()).ok()?;
    let s: StatusFile = serde_json::from_slice(&text).ok()?;
    let fresh = time::OffsetDateTime::now_utc().unix_timestamp() - s.updated_at < 60;
    (pid_alive(s.pid) && fresh).then_some(s)
}

/// One-shot status of the local servers (when no watcher runs).
pub async fn local_status(hosts: &[HostConfig]) -> StatusFile {
    let mut states = Vec::new();
    let mut inventories = BTreeMap::new();
    for h in hosts
        .iter()
        .filter(|h| h.is_local() && h.kind == crate::model::ServerKind::HerdrTui)
    {
        let Some(sock) = h.socket.clone() else {
            continue;
        };
        let b = HerdrTui::new(
            h.name.clone(),
            Arc::new(LocalTransport),
            sock,
            "herdr".into(),
        );
        match tokio::time::timeout(Duration::from_secs(3), b.snapshot()).await {
            Ok(Ok(inv)) => {
                states.push((h.name.clone(), ConnState::Connected));
                inventories.insert(h.name.clone(), inv);
            }
            Ok(Err(e)) => states.push((
                h.name.clone(),
                ConnState::Failed {
                    error: e.to_string(),
                    auth: false,
                },
            )),
            Err(_) => states.push((
                h.name.clone(),
                ConnState::Failed {
                    error: "timeout".into(),
                    auth: false,
                },
            )),
        }
    }
    build_status(&states, &inventories)
}

pub fn render_status(s: &StatusFile, format: &str) -> String {
    let c = s.counts;
    match format {
        "json" => serde_json::to_string_pretty(s).unwrap_or_default(),
        "tmux" => {
            let mut parts = Vec::new();
            if c.blocked > 0 {
                parts.push(format!("#[fg=red,bold]●{}#[default]", c.blocked));
            }
            if c.done > 0 {
                parts.push(format!("#[fg=green]✔{}#[default]", c.done));
            }
            if c.working > 0 {
                parts.push(format!("#[fg=yellow]◐{}#[default]", c.working));
            }
            parts.join(" ")
        }
        _ => {
            let mut parts = Vec::new();
            for (n, label) in [
                (c.blocked, "blocked"),
                (c.done, "done"),
                (c.working, "working"),
                (c.idle, "idle"),
            ] {
                if n > 0 {
                    parts.push(format!("{label} {n}"));
                }
            }
            if parts.is_empty() {
                "no agents".into()
            } else {
                parts.join(" · ")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Agent, AgentKey};

    fn inv(host: &str, st: &[AgentStatus]) -> Inventory {
        Inventory {
            server_version: None,
            agents: st
                .iter()
                .enumerate()
                .map(|(i, s)| Agent {
                    key: AgentKey {
                        host: host.into(),
                        id: format!("p{i}"),
                    },
                    agent: Some("claude".into()),
                    display_agent: None,
                    title: Some(format!("t{i}")),
                    status: *s,
                    workspace_id: "w".into(),
                    workspace_label: None,
                    tab_id: None,
                    cwd: None,
                    revision: 0,
                    state_change_seq: i as u64,
                    session: None,
                    focused: false,
                })
                .collect(),
        }
    }

    #[test]
    fn counts_and_formats() {
        let mut m = BTreeMap::new();
        m.insert(
            "a".to_string(),
            inv("a", &[AgentStatus::Blocked, AgentStatus::Working]),
        );
        m.insert(
            "b".to_string(),
            inv(
                "b",
                &[AgentStatus::Done, AgentStatus::Blocked, AgentStatus::Idle],
            ),
        );
        let s = build_status(&[("a".into(), ConnState::Connected)], &m);
        assert_eq!(
            s.counts,
            Counts {
                blocked: 2,
                done: 1,
                working: 1,
                idle: 1,
                unknown: 0
            }
        );
        assert_eq!(s.agents[0].status, AgentStatus::Blocked);
        assert_eq!(
            render_status(&s, "plain"),
            "blocked 2 · done 1 · working 1 · idle 1"
        );
        assert_eq!(
            render_status(&s, "tmux"),
            "#[fg=red,bold]●2#[default] #[fg=green]✔1#[default] #[fg=yellow]◐1#[default]"
        );
        let back: StatusFile = serde_json::from_str(&render_status(&s, "json")).unwrap();
        assert_eq!(back, s);
        assert_eq!(render_status(&StatusFile::default(), "plain"), "no agents");
    }
}
