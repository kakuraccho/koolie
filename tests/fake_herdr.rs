//! End-to-end tests of the Herdr TUI backend and host sessions against a fake
//! herdr server speaking the real newline-JSON protocol over a Unix socket.

use koolie::backend::herdr_tui::HerdrTui;
use koolie::backend::{Backend, BackendEvent, ReadSource};
use koolie::model::{AgentStatus, ServerKind};
use koolie::session::{self, ConnState, Connector, HostUpdate};
use koolie::transport::{LocalTransport, Transport};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::{broadcast, mpsc};

#[derive(Default)]
struct State {
    /// pane id -> status
    agents: Vec<(String, String)>,
    prompts: Vec<(String, String)>,
    keys: Vec<(String, Vec<String>)>,
    texts: Vec<(String, String)>,
    requests: Vec<String>,
}

struct Fake {
    path: PathBuf,
    state: Arc<Mutex<State>>,
    events: broadcast::Sender<Value>,
    _dir: tempfile::TempDir,
}

fn snapshot(st: &State) -> Value {
    let agents: Vec<Value> = st
        .agents
        .iter()
        .enumerate()
        .map(|(i, (id, status))| {
            json!({
                "pane_id": id, "workspace_id": "w1", "tab_id": "w1:t1", "terminal_id": format!("term_{i}"),
                "agent": "claude", "agent_status": status, "focused": false, "revision": 3,
                "state_change_seq": i, "cwd": "/tmp/proj", "terminal_title_stripped": format!("task {i}"),
                "future_field": [1, 2, 3]
            })
        })
        .collect();
    json!({"type": "session_snapshot", "snapshot": {
        "version": "0.9.5", "protocol": 22, "agents": agents, "panes": [], "tabs": [], "layouts": [],
        "workspaces": [{"workspace_id": "w1", "label": "proj", "number": 1, "focused": true,
                         "pane_count": 1, "tab_count": 1, "active_tab_id": "w1:t1", "agent_status": "idle"}]
    }})
}

async fn start_fake() -> Fake {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("herdr.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let state = Arc::new(Mutex::new(State {
        agents: vec![("w1:p1".into(), "working".into())],
        ..Default::default()
    }));
    let (events, _) = broadcast::channel(64);
    let (st, ev) = (state.clone(), events.clone());
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let (st, ev) = (st.clone(), ev.clone());
            tokio::spawn(async move {
                let (r, mut w) = stream.into_split();
                let mut lines = BufReader::new(r).lines();
                let Ok(Some(line)) = lines.next_line().await else {
                    return;
                };
                let req: Value = serde_json::from_str(&line).unwrap();
                let id = req["id"].clone();
                let method = req["method"].as_str().unwrap_or("").to_string();
                let p = req["params"].clone();
                st.lock().unwrap().requests.push(method.clone());
                let result = match method.as_str() {
                    "ping" => json!({"type": "pong", "version": "0.9.5", "protocol": 22}),
                    "session.snapshot" => {
                        let guard = st.lock().unwrap();
                        snapshot(&guard)
                    }
                    "pane.read" => json!({"type": "pane_read", "read": {
                        "pane_id": p["pane_id"], "workspace_id": "w1", "tab_id": "w1:t1",
                        "source": p["source"], "format": p["format"], "text": "\u{1b}[1mhello\u{1b}[0m\r\nworld",
                        "revision": 7, "truncated": false}}),
                    "agent.prompt" => {
                        let target = p["target"].as_str().unwrap().to_string();
                        let blocked = {
                            let mut s = st.lock().unwrap();
                            let blocked = s
                                .agents
                                .iter()
                                .any(|(i, st)| *i == target && st == "blocked");
                            if !blocked {
                                s.prompts
                                    .push((target, p["text"].as_str().unwrap().to_string()));
                            }
                            blocked
                        };
                        if blocked {
                            let reply = json!({"id": id, "error": {"code": "agent_blocked", "message": "agent is blocked"}});
                            let _ = w.write_all(format!("{reply}\n").as_bytes()).await;
                            return;
                        }
                        json!({"type": "agent_prompted"})
                    }
                    "pane.send_keys" => {
                        let keys = p["keys"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|k| k.as_str().unwrap().to_string())
                            .collect();
                        st.lock()
                            .unwrap()
                            .keys
                            .push((p["pane_id"].as_str().unwrap().to_string(), keys));
                        json!({"type": "ok"})
                    }
                    "pane.send_text" => {
                        st.lock().unwrap().texts.push((
                            p["pane_id"].as_str().unwrap().to_string(),
                            p["text"].as_str().unwrap().to_string(),
                        ));
                        json!({"type": "ok"})
                    }
                    "events.subscribe" => {
                        let mut rx = ev.subscribe();
                        let ack = json!({"id": id, "result": {"type": "subscription_started"}});
                        if w.write_all(format!("{ack}\n").as_bytes()).await.is_err() {
                            return;
                        }
                        while let Ok(e) = rx.recv().await {
                            if e == json!("close") {
                                return;
                            }
                            if w.write_all(format!("{e}\n").as_bytes()).await.is_err() {
                                return;
                            }
                        }
                        return;
                    }
                    other => {
                        let reply = json!({"id": id, "error": {"code": "invalid_request",
                            "message": format!("invalid request: unknown variant `{other}`, expected one of ...")}});
                        let _ = w.write_all(format!("{reply}\n").as_bytes()).await;
                        return;
                    }
                };
                let reply = json!({"id": id, "result": result});
                let _ = w.write_all(format!("{reply}\n").as_bytes()).await;
            });
        }
    });
    Fake {
        path,
        state,
        events,
        _dir: dir,
    }
}

fn backend(f: &Fake) -> HerdrTui {
    HerdrTui::new(
        "fake".into(),
        Arc::new(LocalTransport),
        f.path.to_string_lossy().into_owned(),
        "herdr".into(),
    )
}

#[tokio::test]
async fn backend_round_trips() {
    let f = start_fake().await;
    let b = backend(&f);
    assert_eq!(b.hello().await.unwrap().version, "0.9.5");
    let inv = b.snapshot().await.unwrap();
    assert_eq!(inv.agents.len(), 1);
    assert_eq!(inv.agents[0].workspace_label.as_deref(), Some("proj"));
    assert_eq!(inv.agents[0].title.as_deref(), Some("task 0"));

    let s = b
        .read("w1:p1", ReadSource::Recent, Some(50), true)
        .await
        .unwrap();
    assert_eq!(s.revision, 7);
    assert!(s.text.contains("hello"));

    b.prompt("w1:p1", "テストして").await.unwrap();
    b.send_keys("w1:p1", &["esc".into()]).await.unwrap();
    b.insert_text("w1:p1", "draft").await.unwrap();
    {
        let st = f.state.lock().unwrap();
        assert_eq!(
            st.prompts,
            vec![("w1:p1".to_string(), "テストして".to_string())]
        );
        assert_eq!(
            st.keys,
            vec![("w1:p1".to_string(), vec!["esc".to_string()])]
        );
        assert_eq!(st.texts, vec![("w1:p1".to_string(), "draft".to_string())]);
    }

    f.state.lock().unwrap().agents[0].1 = "blocked".into();
    let e = b.prompt("w1:p1", "x").await.unwrap_err();
    assert_eq!(e.code(), Some("agent_blocked"));
}

#[tokio::test]
async fn subscription_streams_events_and_ends_on_close() {
    let f = start_fake().await;
    let b = backend(&f);
    let mut rx = b.subscribe(&["w1:p1".into()]).await.unwrap();
    f.events
        .send(json!({"event": "pane.agent_status_changed", "data": {"pane_id": "w1:p1", "workspace_id": "w1", "agent_status": "blocked"}}))
        .unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        ev,
        BackendEvent::StatusChanged {
            id: "w1:p1".into(),
            status: AgentStatus::Blocked
        }
    );
    f.events.send(json!("close")).unwrap();
    let end = tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .unwrap();
    assert!(end.is_none(), "stream must end when the server closes it");
}

#[derive(Debug)]
struct Fixed(Arc<HerdrTui>);

#[async_trait::async_trait]
impl Connector for Fixed {
    fn transport(&self) -> Arc<dyn Transport> {
        Arc::new(LocalTransport)
    }
    async fn connect(&self) -> Result<Arc<dyn Backend>, koolie::backend::BackendError> {
        Ok(self.0.clone())
    }
}

#[tokio::test]
async fn session_follows_status_changes() {
    let f = start_fake().await;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let h = session::spawn(
        "fake".into(),
        ServerKind::HerdrTui,
        Arc::new(Fixed(Arc::new(backend(&f)))),
        tx,
    );
    let mut connected = false;
    let mut first = None;
    while first.is_none() {
        match tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap()
        {
            HostUpdate::State {
                state: ConnState::Connected,
                ..
            } => connected = true,
            HostUpdate::Inventory { inventory, .. } => first = Some(inventory),
            _ => {}
        }
    }
    assert!(connected);
    assert_eq!(first.unwrap().agents[0].status, AgentStatus::Working);

    f.state.lock().unwrap().agents[0].1 = "done".into();
    f.events
        .send(json!({"event": "pane.agent_status_changed", "data": {"pane_id": "w1:p1", "workspace_id": "w1", "agent_status": "done"}}))
        .unwrap();
    loop {
        if let HostUpdate::Inventory { inventory, .. } =
            tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .unwrap()
                .unwrap()
        {
            assert_eq!(inventory.agents[0].status, AgentStatus::Done);
            break;
        }
    }
    h.shutdown().await;
}
