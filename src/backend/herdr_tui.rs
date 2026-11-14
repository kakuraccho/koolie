//! Herdr TUI's socket API: newline-delimited JSON, one request per
//! connection, plus one long-lived connection for events.
//!
//! Reference: <https://herdr.dev/docs/socket-api/> and `herdr api schema`.

use super::jsonl::LineConn;
use super::{
    Backend, BackendError, BackendEvent, Capabilities, ReadSource, Result, Screen, ServerInfo,
};
use crate::model::{
    Agent, AgentKey, AgentSession, AgentStatus, HostId, Inventory, ServerKind, SessionRefKind,
};
use crate::transport::{Transport, TransportError};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Event types that may change the inventory; subscribed without filters.
pub const LIFECYCLE_EVENTS: &[&str] = &[
    "workspace.created",
    "workspace.updated",
    "workspace.renamed",
    "workspace.closed",
    "tab.created",
    "tab.closed",
    "tab.renamed",
    "pane.created",
    "pane.closed",
    "pane.updated",
    "pane.exited",
    "pane.agent_detected",
];

#[derive(Debug)]
pub struct HerdrTui {
    host: HostId,
    transport: Arc<dyn Transport>,
    /// Socket path on the host.
    socket: String,
    /// herdr executable on the host (for attach).
    herdr_bin: String,
    seq: AtomicU64,
}

impl HerdrTui {
    pub fn new(
        host: HostId,
        transport: Arc<dyn Transport>,
        socket: String,
        herdr_bin: String,
    ) -> Self {
        Self {
            host,
            transport,
            socket,
            herdr_bin,
            seq: AtomicU64::new(1),
        }
    }

    pub fn socket(&self) -> &str {
        &self.socket
    }

    fn next_id(&self) -> String {
        format!("koolie-{}", self.seq.fetch_add(1, Ordering::Relaxed))
    }

    /// One request on a fresh connection.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let stream = self.transport.connect(&self.socket).await?;
        let mut conn = LineConn::new(stream);
        let id = self.next_id();
        conn.send(&json!({"id": id, "method": method, "params": params}))
            .await?;
        let reply = tokio::time::timeout(REQUEST_TIMEOUT, conn.recv())
            .await
            .map_err(|_| TransportError::Timeout(format!("{method}: no answer")))??;
        let Some(reply) = reply else {
            // The socket accepted but closed at once: over SSH this is what a
            // missing remote socket looks like.
            return Err(
                TransportError::NotRunning(format!("{}: connection closed", self.socket)).into(),
            );
        };
        parse_reply(method, reply)
    }
}

/// Split a reply into result or error.
pub fn parse_reply(method: &str, reply: Value) -> Result<Value> {
    if let Some(err) = reply.get("error") {
        let code = err
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("error")
            .to_string();
        let message = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if code == "invalid_request"
            && message.contains("unknown variant")
            && message.contains(method)
        {
            return Err(BackendError::Unsupported(method.to_string()));
        }
        if code == "unknown_method" || code == "method_not_found" {
            return Err(BackendError::Unsupported(method.to_string()));
        }
        return Err(BackendError::Server { code, message });
    }
    reply.get("result").cloned().ok_or_else(|| {
        BackendError::Protocol(format!("{method}: reply has neither result nor error"))
    })
}

#[derive(Debug, Deserialize)]
struct RawSnapshot {
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    agents: Vec<RawAgent>,
    #[serde(default)]
    workspaces: Vec<RawWorkspace>,
}

#[derive(Debug, Deserialize)]
struct RawWorkspace {
    workspace_id: String,
    #[serde(default)]
    label: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawAgent {
    pane_id: String,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    display_agent: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    terminal_title_stripped: Option<String>,
    #[serde(default)]
    agent_status: AgentStatus,
    workspace_id: String,
    #[serde(default)]
    tab_id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    foreground_cwd: Option<String>,
    #[serde(default)]
    revision: u64,
    #[serde(default)]
    state_change_seq: u64,
    #[serde(default)]
    agent_session: Option<RawSession>,
    #[serde(default)]
    focused: bool,
}

#[derive(Debug, Deserialize)]
struct RawSession {
    agent: String,
    kind: SessionRefKind,
    value: String,
}

fn non_empty(s: Option<String>) -> Option<String> {
    s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

impl RawAgent {
    fn into_agent(self, host: &HostId, labels: &HashMap<String, String>) -> Agent {
        let title = non_empty(self.name)
            .or_else(|| non_empty(self.title))
            .or_else(|| non_empty(self.terminal_title_stripped));
        Agent {
            key: AgentKey {
                host: host.clone(),
                id: self.pane_id,
            },
            agent: non_empty(self.agent),
            display_agent: non_empty(self.display_agent),
            title,
            status: self.agent_status,
            workspace_label: labels.get(&self.workspace_id).cloned(),
            workspace_id: self.workspace_id,
            tab_id: self.tab_id,
            cwd: non_empty(self.foreground_cwd).or_else(|| non_empty(self.cwd)),
            revision: self.revision,
            state_change_seq: self.state_change_seq,
            session: self.agent_session.map(|s| AgentSession {
                agent: s.agent,
                kind: s.kind,
                value: s.value,
            }),
            focused: self.focused,
        }
    }
}

/// Parse a `session.snapshot` result into an inventory.
pub fn parse_snapshot(host: &HostId, result: Value) -> Result<Inventory> {
    let snap = result
        .get("snapshot")
        .cloned()
        .ok_or_else(|| BackendError::Protocol("session.snapshot: missing snapshot".into()))?;
    let raw: RawSnapshot = serde_json::from_value(snap)
        .map_err(|e| BackendError::Protocol(format!("session.snapshot: {e}")))?;
    let labels: HashMap<String, String> = raw
        .workspaces
        .into_iter()
        .filter_map(|w| non_empty(w.label).map(|l| (w.workspace_id, l)))
        .collect();
    Ok(Inventory {
        server_version: raw.version,
        agents: raw
            .agents
            .into_iter()
            .map(|a| a.into_agent(host, &labels))
            .collect(),
    })
}

/// Classify one line of an event subscription.
pub fn parse_event_line(line: &Value) -> Option<std::result::Result<BackendEvent, BackendError>> {
    if let Some(err) = line.get("error") {
        let code = err.get("code").and_then(Value::as_str).unwrap_or("");
        if code == "events_lost" {
            return Some(Err(BackendError::EventsLost));
        }
        return Some(Err(BackendError::Server {
            code: code.to_string(),
            message: err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        }));
    }
    if line
        .pointer("/result/type")
        .and_then(Value::as_str)
        .is_some_and(|t| t == "subscription_started")
    {
        return None;
    }
    let event = line.get("event").and_then(Value::as_str);
    if event == Some("pane.agent_status_changed") {
        let data = line.get("data");
        let id = data.and_then(|d| d.get("pane_id")).and_then(Value::as_str);
        let status = data
            .and_then(|d| d.get("agent_status"))
            .and_then(Value::as_str)
            .and_then(AgentStatus::parse);
        if let (Some(id), Some(status)) = (id, status) {
            return Some(Ok(BackendEvent::StatusChanged {
                id: id.to_string(),
                status,
            }));
        }
    }
    Some(Ok(BackendEvent::Changed))
}

#[async_trait]
impl Backend for HerdrTui {
    fn kind(&self) -> ServerKind {
        ServerKind::HerdrTui
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            prompt: true,
            insert_text: true,
            sessions: true,
        }
    }

    async fn hello(&self) -> Result<ServerInfo> {
        let r = self.call("ping", json!({})).await?;
        Ok(ServerInfo {
            version: r
                .get("version")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string(),
            protocol: r.get("protocol").and_then(Value::as_u64),
        })
    }

    async fn snapshot(&self) -> Result<Inventory> {
        let r = self.call("session.snapshot", json!({})).await?;
        parse_snapshot(&self.host, r)
    }

    async fn subscribe(
        &self,
        ids: &[String],
    ) -> Result<mpsc::Receiver<std::result::Result<BackendEvent, BackendError>>> {
        let stream = self.transport.connect(&self.socket).await?;
        let mut conn = LineConn::new(stream);
        let mut subs: Vec<Value> = LIFECYCLE_EVENTS
            .iter()
            .map(|t| json!({"type": t}))
            .collect();
        subs.extend(
            ids.iter()
                .map(|id| json!({"type": "pane.agent_status_changed", "pane_id": id})),
        );
        let id = self.next_id();
        conn.send(
            &json!({"id": id, "method": "events.subscribe", "params": {"subscriptions": subs}}),
        )
        .await?;
        let ack = tokio::time::timeout(REQUEST_TIMEOUT, conn.recv())
            .await
            .map_err(|_| TransportError::Timeout("events.subscribe: no answer".into()))??
            .ok_or_else(|| {
                TransportError::NotRunning(format!("{}: connection closed", self.socket))
            })?;
        let ack = parse_reply("events.subscribe", ack)?;
        if ack.get("type").and_then(Value::as_str) != Some("subscription_started") {
            return Err(BackendError::Protocol(format!(
                "events.subscribe: unexpected answer {ack}"
            )));
        }
        let (tx, rx) = mpsc::channel(256);
        tokio::spawn(async move {
            loop {
                match conn.recv().await {
                    Ok(Some(line)) => {
                        let Some(item) = parse_event_line(&line) else {
                            continue;
                        };
                        let end = item.is_err();
                        if tx.send(item).await.is_err() || end {
                            return;
                        }
                    }
                    Ok(None) => return,
                    Err(e) => {
                        let _ = tx.send(Err(e)).await;
                        return;
                    }
                }
            }
        });
        Ok(rx)
    }

    async fn read(
        &self,
        id: &str,
        source: ReadSource,
        lines: Option<u32>,
        ansi: bool,
    ) -> Result<Screen> {
        let mut params = json!({
            "pane_id": id,
            "source": source.as_api(),
            "format": if ansi { "ansi" } else { "text" },
            "strip_ansi": !ansi,
        });
        if let Some(n) = lines {
            params["lines"] = json!(n);
        }
        let r = self.call("pane.read", params).await?;
        let read = r
            .get("read")
            .ok_or_else(|| BackendError::Protocol("pane.read: missing read".into()))?;
        Ok(Screen {
            text: read
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            revision: read.get("revision").and_then(Value::as_u64).unwrap_or(0),
            ansi,
            truncated: read
                .get("truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }

    async fn prompt(&self, id: &str, text: &str) -> Result<()> {
        self.call("agent.prompt", json!({"target": id, "text": text}))
            .await
            .map(|_| ())
    }

    async fn send_keys(&self, id: &str, keys: &[String]) -> Result<()> {
        self.call("pane.send_keys", json!({"pane_id": id, "keys": keys}))
            .await
            .map(|_| ())
    }

    async fn insert_text(&self, id: &str, text: &str) -> Result<()> {
        self.call("pane.send_text", json!({"pane_id": id, "text": text}))
            .await
            .map(|_| ())
    }

    fn attach_argv(&self, id: &str) -> Vec<String> {
        vec![
            "env".into(),
            format!("HERDR_SOCKET_PATH={}", self.socket),
            self.herdr_bin.clone(),
            "agent".into(),
            "attach".into(),
            id.into(),
            "--takeover".into(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real `session.snapshot` result from herdr 0.9.5.
    const SNAPSHOT_095: &str = r#"{"snapshot":{"agents":[{"agent":"claude","agent_status":"working","cwd":"/home/u/dev","focused":true,"foreground_cwd":"/home/u/dev","pane_id":"wB:p1","revision":4,"state_change_seq":6,"tab_id":"wB:t1","terminal_id":"term_65ff90af2b6c11","terminal_title":"◐ Heeler for herdr TUIツール","terminal_title_stripped":"Heeler for herdr TUIツール","workspace_id":"wB"}],"focused_pane_id":"wB:p1","focused_tab_id":"wB:t1","focused_workspace_id":"wB","layouts":[],"panes":[],"protocol":22,"tabs":[],"version":"0.9.5","workspaces":[{"active_tab_id":"wB:t1","agent_status":"working","focused":true,"label":"dev","number":1,"pane_count":1,"tab_count":1,"workspace_id":"wB"}]},"type":"session_snapshot"}"#;

    #[test]
    fn parses_real_snapshot() {
        let inv = parse_snapshot(
            &"local".to_string(),
            serde_json::from_str(SNAPSHOT_095).unwrap(),
        )
        .unwrap();
        assert_eq!(inv.server_version.as_deref(), Some("0.9.5"));
        assert_eq!(inv.agents.len(), 1);
        let a = &inv.agents[0];
        assert_eq!(a.key.id, "wB:p1");
        assert_eq!(a.key.host, "local");
        assert_eq!(a.agent.as_deref(), Some("claude"));
        assert_eq!(a.status, AgentStatus::Working);
        assert_eq!(a.title.as_deref(), Some("Heeler for herdr TUIツール"));
        assert_eq!(a.workspace_label.as_deref(), Some("dev"));
        assert_eq!(a.state_change_seq, 6);
        assert!(a.session.is_none());
    }

    #[test]
    fn name_beats_title_and_session_parses() {
        let v = json!({"snapshot": {"version": "0.9.5", "workspaces": [], "agents": [{
            "pane_id": "w1:p2", "workspace_id": "w1", "agent_status": "blocked", "revision": 1,
            "name": "  reviewer ", "terminal_title_stripped": "ignored", "focused": false,
            "agent_session": {"agent": "claude", "kind": "id", "source": "hook", "value": "abc-123"},
            "brand_new_field": {"x": 1}
        }]}});
        let inv = parse_snapshot(&"h".into(), v).unwrap();
        let a = &inv.agents[0];
        assert_eq!(a.title.as_deref(), Some("reviewer"));
        assert_eq!(a.status, AgentStatus::Blocked);
        let s = a.session.as_ref().unwrap();
        assert_eq!(
            (s.agent.as_str(), s.kind.clone(), s.value.as_str()),
            ("claude", SessionRefKind::Id, "abc-123")
        );
    }

    #[test]
    fn replies_and_errors() {
        assert_eq!(
            parse_reply("ping", json!({"id": "1", "result": {"type": "pong"}})).unwrap(),
            json!({"type": "pong"})
        );
        let e = parse_reply(
            "agent.prompt",
            json!({"id": "1", "error": {"code": "agent_blocked", "message": "agent is blocked"}}),
        )
        .unwrap_err();
        assert_eq!(e.code(), Some("agent_blocked"));
        let e = parse_reply(
            "agent.explain",
            json!({"id": "1", "error": {"code": "invalid_request", "message": "invalid request: unknown variant `agent.explain`, expected one of ..."}}),
        )
        .unwrap_err();
        assert!(matches!(e, BackendError::Unsupported(_)));
    }

    #[test]
    fn event_lines() {
        assert_eq!(
            parse_event_line(&json!({"id": "s", "result": {"type": "subscription_started"}})),
            None
        );
        assert_eq!(
            parse_event_line(
                &json!({"event": "pane.agent_status_changed", "data": {"pane_id": "w1:p1", "workspace_id": "w1", "agent_status": "done"}})
            ),
            Some(Ok(BackendEvent::StatusChanged {
                id: "w1:p1".into(),
                status: AgentStatus::Done
            }))
        );
        assert_eq!(
            parse_event_line(&json!({"event": "pane.created", "data": {}})),
            Some(Ok(BackendEvent::Changed))
        );
        assert_eq!(
            parse_event_line(&json!({"id": "s", "error": {"code": "events_lost", "message": "x"}})),
            Some(Err(BackendError::EventsLost))
        );
    }
}
