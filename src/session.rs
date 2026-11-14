//! One host's connection: keeps a backend connected, its inventory fresh and
//! reports both to the app.
//!
//! The inventory is replaced wholesale on every snapshot. Events are only
//! signals to refetch: herdr does not order events against snapshots.

use crate::backend::herdr_core::HerdrCore;
use crate::backend::herdr_tui::HerdrTui;
use crate::backend::{Backend, BackendError, BackendEvent, ServerInfo};
use crate::config::HostConfig;
use crate::discover;
use crate::model::{AgentStatus, HostId, Inventory, ServerKind};
use crate::transport::{LocalTransport, SshTransport, Transport, TransportError};
use async_trait::async_trait;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::mpsc;

/// How often the inventory is refetched without any event.
const SAFETY_POLL: Duration = Duration::from_secs(10);
/// Events arriving within this window cause one refetch.
const DEBOUNCE: Duration = Duration::from_millis(150);
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// Where one host's connection stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnState {
    Connecting,
    Connected,
    /// Automatic recovery after a retryable failure.
    Reconnecting {
        attempt: u32,
        error: String,
    },
    /// Stopped: needs the user (auth, host key, unsupported server, ...).
    Failed {
        error: String,
        auth: bool,
    },
}

impl ConnState {
    pub fn is_connected(&self) -> bool {
        matches!(self, ConnState::Connected)
    }
    pub fn label(&self) -> String {
        match self {
            ConnState::Connecting => "接続中".into(),
            ConnState::Connected => "接続済み".into(),
            ConnState::Reconnecting { attempt, .. } => format!("再接続中 ({attempt})"),
            ConnState::Failed { auth: true, .. } => "要認証".into(),
            ConnState::Failed { .. } => "失敗".into(),
        }
    }
    pub fn error(&self) -> Option<&str> {
        match self {
            ConnState::Reconnecting { error, .. } | ConnState::Failed { error, .. } => Some(error),
            _ => None,
        }
    }
}

/// What a session tells the app.
#[derive(Clone, Debug)]
pub enum HostUpdate {
    State {
        host: HostId,
        state: ConnState,
    },
    Connected {
        host: HostId,
        info: ServerInfo,
        route: String,
    },
    Inventory {
        host: HostId,
        inventory: Inventory,
    },
    StatusChanged {
        host: HostId,
        id: String,
        status: AgentStatus,
    },
}

#[derive(Debug)]
enum Cmd {
    Refresh,
    Reconnect,
}

/// Builds a connected backend for a host.
#[async_trait]
pub trait Connector: Send + Sync + std::fmt::Debug {
    fn transport(&self) -> Arc<dyn Transport>;
    async fn connect(&self) -> Result<Arc<dyn Backend>, BackendError>;
}

/// The real connector: transport from the config, socket and binary from the
/// config or a probe.
#[derive(Debug)]
pub struct HostConnector {
    cfg: HostConfig,
    transport: Arc<dyn Transport>,
}

impl HostConnector {
    pub fn new(cfg: HostConfig) -> Self {
        let transport: Arc<dyn Transport> = if cfg.is_local() {
            Arc::new(LocalTransport)
        } else {
            Arc::new(SshTransport::new(cfg.name.clone(), cfg.routes.clone()))
        };
        Self { cfg, transport }
    }
}

#[async_trait]
impl Connector for HostConnector {
    fn transport(&self) -> Arc<dyn Transport> {
        self.transport.clone()
    }

    async fn connect(&self) -> Result<Arc<dyn Backend>, BackendError> {
        self.transport.ensure().await?;
        let cfg = &self.cfg;
        let (socket, bin) = match (&cfg.socket, &cfg.bin) {
            (Some(s), Some(b)) => (s.clone(), b.clone()),
            _ => {
                let probe =
                    if cfg.is_local() && cfg.kind == ServerKind::HerdrTui && cfg.socket.is_some() {
                        discover::ProbeResult::default()
                    } else if cfg.kind == ServerKind::HerdrCore {
                        discover::probe_core_session(self.transport.as_ref(), cfg.session_name())
                            .await?
                    } else {
                        discover::probe(self.transport.as_ref()).await?
                    };
                resolve_endpoint(cfg, &probe)?
            }
        };
        let backend: Arc<dyn Backend> = match cfg.kind {
            ServerKind::HerdrTui => Arc::new(HerdrTui::new(
                cfg.name.clone(),
                self.transport.clone(),
                socket,
                bin,
            )),
            ServerKind::HerdrCore => Arc::new(HerdrCore::new(
                cfg.name.clone(),
                self.transport.clone(),
                socket,
                bin,
            )),
        };
        Ok(backend)
    }
}

/// Pick the socket and binary for a host from its config and a probe.
pub fn resolve_endpoint(
    cfg: &HostConfig,
    probe: &discover::ProbeResult,
) -> Result<(String, String), BackendError> {
    match cfg.kind {
        ServerKind::HerdrTui => {
            let socket = match &cfg.socket {
                Some(s) => s.clone(),
                None => probe
                    .herdr_sockets
                    .iter()
                    .find(|s| s.session == cfg.session_name())
                    .map(|s| s.path.clone())
                    .ok_or_else(|| {
                        TransportError::NotRunning(format!(
                            "herdr session `{}` is not running on this host",
                            cfg.session_name()
                        ))
                    })?,
            };
            let bin = cfg
                .bin
                .clone()
                .or_else(|| probe.herdr_bin.clone())
                .unwrap_or_else(|| "herdr".to_string());
            Ok((socket, bin))
        }
        ServerKind::HerdrCore => {
            let socket = cfg
                .socket
                .clone()
                .or_else(|| probe.core_socket.clone())
                .ok_or_else(|| {
                    TransportError::NotRunning("herdr-core is not running on this host".into())
                })?;
            let bin = cfg
                .bin
                .clone()
                .or_else(|| probe.core_bin.clone())
                .ok_or_else(|| {
                    BackendError::Unsupported("herdr-core is not installed on this host".into())
                })?;
            Ok((socket, bin))
        }
    }
}

/// Handle the app keeps per host.
#[derive(Clone, Debug)]
pub struct HostHandle {
    pub id: HostId,
    pub kind: ServerKind,
    transport: Arc<dyn Transport>,
    backend: Arc<RwLock<Option<Arc<dyn Backend>>>>,
    cmd: mpsc::UnboundedSender<Cmd>,
    task: Arc<tokio::task::JoinHandle<()>>,
}

impl HostHandle {
    /// The connected backend, if any.
    pub fn backend(&self) -> Option<Arc<dyn Backend>> {
        self.backend.read().ok().and_then(|b| b.clone())
    }

    pub fn transport(&self) -> Arc<dyn Transport> {
        self.transport.clone()
    }

    pub fn refresh(&self) {
        let _ = self.cmd.send(Cmd::Refresh);
    }

    pub fn reconnect(&self) {
        let _ = self.cmd.send(Cmd::Reconnect);
    }

    /// Stop the session and tear its route down.
    pub async fn shutdown(&self) {
        self.task.abort();
        self.transport.shutdown().await;
    }
}

/// Start a session for one host.
pub fn spawn(
    id: HostId,
    kind: ServerKind,
    connector: Arc<dyn Connector>,
    tx: mpsc::UnboundedSender<HostUpdate>,
) -> HostHandle {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let backend = Arc::new(RwLock::new(None));
    let transport = connector.transport();
    let task = tokio::spawn(run(id.clone(), connector, backend.clone(), tx, cmd_rx));
    HostHandle {
        id,
        kind,
        transport,
        backend,
        cmd: cmd_tx,
        task: Arc::new(task),
    }
}

/// Why the connected phase ended.
enum End {
    Retry(String),
    Fail { error: String, auth: bool },
    Reconnect,
    AppGone,
}

fn classify(e: &BackendError) -> End {
    match e {
        BackendError::Transport(TransportError::AuthNeeded(m)) => End::Fail {
            error: format!("SSH の認証が必要です: {m}"),
            auth: true,
        },
        BackendError::Transport(TransportError::HostKey(m)) => End::Fail {
            error: format!("ホスト鍵を確認してください: {m}"),
            auth: true,
        },
        e if e.is_retryable() => End::Retry(e.to_string()),
        e => End::Fail {
            error: e.to_string(),
            auth: false,
        },
    }
}

async fn run(
    host: HostId,
    connector: Arc<dyn Connector>,
    slot: Arc<RwLock<Option<Arc<dyn Backend>>>>,
    tx: mpsc::UnboundedSender<HostUpdate>,
    mut cmd: mpsc::UnboundedReceiver<Cmd>,
) {
    let mut attempt: u32 = 0;
    let send = |u: HostUpdate| tx.send(u).is_ok();
    loop {
        let state = if attempt == 0 {
            ConnState::Connecting
        } else {
            ConnState::Reconnecting {
                attempt,
                error: String::new(),
            }
        };
        if attempt == 0
            && !send(HostUpdate::State {
                host: host.clone(),
                state,
            })
        {
            return;
        }
        let end = match connector.connect().await {
            Ok(backend) => {
                let route = connector.transport().label();
                connected(&host, backend, route, &slot, &tx, &mut cmd, &mut attempt).await
            }
            Err(e) => classify(&e),
        };
        if let Ok(mut s) = slot.write() {
            *s = None;
        }
        match end {
            End::AppGone => return,
            End::Reconnect => {
                attempt = 0;
                continue;
            }
            End::Retry(error) => {
                attempt += 1;
                let state = ConnState::Reconnecting { attempt, error };
                if !send(HostUpdate::State {
                    host: host.clone(),
                    state,
                }) {
                    return;
                }
                let wait = backoff(attempt);
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    c = cmd.recv() => match c {
                        None => return,
                        Some(_) => attempt = 0,
                    },
                }
            }
            End::Fail { error, auth } => {
                if !send(HostUpdate::State {
                    host: host.clone(),
                    state: ConnState::Failed { error, auth },
                }) {
                    return;
                }
                // Wait for the user to ask for a reconnect.
                if cmd.recv().await.is_none() {
                    return;
                }
                attempt = 0;
            }
        }
    }
}

fn backoff(attempt: u32) -> Duration {
    let secs = BACKOFF_MIN.as_secs_f64() * 2f64.powi(attempt.saturating_sub(1).min(10) as i32);
    Duration::from_secs_f64(secs.min(BACKOFF_MAX.as_secs_f64()))
}

fn agent_ids(inv: &Inventory) -> Vec<String> {
    let mut ids: Vec<String> = inv.agents.iter().map(|a| a.key.id.clone()).collect();
    ids.sort();
    ids
}

async fn connected(
    host: &HostId,
    backend: Arc<dyn Backend>,
    route: String,
    slot: &Arc<RwLock<Option<Arc<dyn Backend>>>>,
    tx: &mpsc::UnboundedSender<HostUpdate>,
    cmd: &mut mpsc::UnboundedReceiver<Cmd>,
    attempt: &mut u32,
) -> End {
    let send = |u: HostUpdate| tx.send(u).is_ok();
    let info = match backend.hello().await {
        Ok(i) => i,
        Err(e) => return classify(&e),
    };
    // Subscribe before the first snapshot so no change falls in between.
    let mut events = match backend.subscribe(&[]).await {
        Ok(rx) => rx,
        Err(e) => return classify(&e),
    };
    let mut inv = match backend.snapshot().await {
        Ok(i) => i,
        Err(e) => return classify(&e),
    };
    let mut ids = agent_ids(&inv);
    if !ids.is_empty() {
        match backend.subscribe(&ids).await {
            Ok(rx) => events = rx,
            Err(e) => return classify(&e),
        }
        // A status may have changed between the two subscriptions.
        match backend.snapshot().await {
            Ok(i) => inv = i,
            Err(e) => return classify(&e),
        }
        ids = agent_ids(&inv);
    }
    if let Ok(mut s) = slot.write() {
        *s = Some(backend.clone());
    }
    *attempt = 0;
    if !send(HostUpdate::Connected {
        host: host.clone(),
        info,
        route,
    }) || !send(HostUpdate::State {
        host: host.clone(),
        state: ConnState::Connected,
    }) || !send(HostUpdate::Inventory {
        host: host.clone(),
        inventory: inv,
    }) {
        return End::AppGone;
    }

    let mut poll = tokio::time::interval(SAFETY_POLL);
    poll.tick().await;
    let mut pending: Option<tokio::time::Instant> = None;
    loop {
        let debounce = async {
            match pending {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending::<()>().await,
            }
        };
        let refetch = tokio::select! {
            ev = events.recv() => match ev {
                Some(Ok(BackendEvent::StatusChanged { id, status })) => {
                    if !send(HostUpdate::StatusChanged { host: host.clone(), id, status }) {
                        return End::AppGone;
                    }
                    pending.get_or_insert(tokio::time::Instant::now() + DEBOUNCE);
                    false
                }
                Some(Ok(BackendEvent::Changed)) => {
                    pending.get_or_insert(tokio::time::Instant::now() + DEBOUNCE);
                    false
                }
                Some(Err(BackendError::EventsLost)) => {
                    // Resubscribe, then refetch.
                    match backend.subscribe(&ids).await {
                        Ok(rx) => { events = rx; true }
                        Err(e) => return classify(&e),
                    }
                }
                Some(Err(e)) => return classify(&e),
                None => return End::Retry("イベントの接続が切れました".into()),
            },
            _ = debounce => { pending = None; true }
            _ = poll.tick() => true,
            c = cmd.recv() => match c {
                None => return End::AppGone,
                Some(Cmd::Refresh) => true,
                Some(Cmd::Reconnect) => return End::Reconnect,
            },
        };
        if !refetch {
            continue;
        }
        let inv = match backend.snapshot().await {
            Ok(i) => i,
            Err(e) => return classify(&e),
        };
        let new_ids = agent_ids(&inv);
        if new_ids != ids {
            ids = new_ids;
            match backend.subscribe(&ids).await {
                Ok(rx) => events = rx,
                Err(e) => return classify(&e),
            }
        }
        if !send(HostUpdate::Inventory {
            host: host.clone(),
            inventory: inv,
        }) {
            return End::AppGone;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{Capabilities, ReadSource, Screen};
    use crate::model::{Agent, AgentKey};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Scriptable backend for session tests.
    #[derive(Debug, Default)]
    pub struct FakeBackend {
        pub inventory: Mutex<Inventory>,
        pub event_tx: Mutex<Option<mpsc::Sender<Result<BackendEvent, BackendError>>>>,
        pub subscribes: AtomicUsize,
        pub snapshots: AtomicUsize,
        pub fail_snapshot: Mutex<Option<BackendError>>,
    }

    pub fn agent(id: &str, status: AgentStatus) -> Agent {
        Agent {
            key: AgentKey {
                host: "h".into(),
                id: id.into(),
            },
            agent: Some("claude".into()),
            display_agent: None,
            title: None,
            status,
            workspace_id: "w".into(),
            workspace_label: None,
            tab_id: None,
            cwd: None,
            revision: 0,
            state_change_seq: 0,
            session: None,
            focused: false,
        }
    }

    #[async_trait]
    impl Backend for FakeBackend {
        fn kind(&self) -> ServerKind {
            ServerKind::HerdrTui
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::default()
        }
        async fn hello(&self) -> crate::backend::Result<ServerInfo> {
            Ok(ServerInfo {
                version: "fake".into(),
                protocol: None,
            })
        }
        async fn snapshot(&self) -> crate::backend::Result<Inventory> {
            self.snapshots.fetch_add(1, Ordering::SeqCst);
            if let Some(e) = self.fail_snapshot.lock().unwrap().take() {
                return Err(e);
            }
            Ok(self.inventory.lock().unwrap().clone())
        }
        async fn subscribe(
            &self,
            _ids: &[String],
        ) -> crate::backend::Result<mpsc::Receiver<Result<BackendEvent, BackendError>>> {
            self.subscribes.fetch_add(1, Ordering::SeqCst);
            let (tx, rx) = mpsc::channel(16);
            *self.event_tx.lock().unwrap() = Some(tx);
            Ok(rx)
        }
        async fn read(
            &self,
            _: &str,
            _: ReadSource,
            _: Option<u32>,
            _: bool,
        ) -> crate::backend::Result<Screen> {
            Ok(Screen::default())
        }
        async fn prompt(&self, _: &str, _: &str) -> crate::backend::Result<()> {
            Ok(())
        }
        async fn send_keys(&self, _: &str, _: &[String]) -> crate::backend::Result<()> {
            Ok(())
        }
        async fn insert_text(&self, _: &str, _: &str) -> crate::backend::Result<()> {
            Ok(())
        }
        fn attach_argv(&self, id: &str) -> Vec<String> {
            vec!["true".into(), id.into()]
        }
    }

    #[derive(Debug)]
    struct FakeConnector {
        backend: Arc<FakeBackend>,
        fails: Mutex<Vec<BackendError>>,
    }

    #[async_trait]
    impl Connector for FakeConnector {
        fn transport(&self) -> Arc<dyn Transport> {
            Arc::new(LocalTransport)
        }
        async fn connect(&self) -> Result<Arc<dyn Backend>, BackendError> {
            if let Some(e) = self.fails.lock().unwrap().pop() {
                return Err(e);
            }
            Ok(self.backend.clone())
        }
    }

    async fn next_inventory(rx: &mut mpsc::UnboundedReceiver<HostUpdate>) -> Inventory {
        loop {
            match tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .unwrap()
                .unwrap()
            {
                HostUpdate::Inventory { inventory, .. } => return inventory,
                _ => continue,
            }
        }
    }

    async fn next_state(rx: &mut mpsc::UnboundedReceiver<HostUpdate>) -> ConnState {
        loop {
            match tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .unwrap()
                .unwrap()
            {
                HostUpdate::State { state, .. } => return state,
                _ => continue,
            }
        }
    }

    fn setup(
        fails: Vec<BackendError>,
    ) -> (
        Arc<FakeBackend>,
        HostHandle,
        mpsc::UnboundedReceiver<HostUpdate>,
    ) {
        let backend = Arc::new(FakeBackend::default());
        *backend.inventory.lock().unwrap() = Inventory {
            server_version: None,
            agents: vec![agent("p1", AgentStatus::Working)],
        };
        let connector = Arc::new(FakeConnector {
            backend: backend.clone(),
            fails: Mutex::new(fails),
        });
        let (tx, rx) = mpsc::unbounded_channel();
        let h = spawn("h".into(), ServerKind::HerdrTui, connector, tx);
        (backend, h, rx)
    }

    #[tokio::test]
    async fn connects_and_refetches_on_event() {
        let (backend, h, mut rx) = setup(vec![]);
        let inv = next_inventory(&mut rx).await;
        assert_eq!(inv.agents.len(), 1);
        assert!(h.backend().is_some());
        // Agents known -> per-agent subscription was installed.
        assert_eq!(backend.subscribes.load(Ordering::SeqCst), 2);

        backend.inventory.lock().unwrap().agents[0].status = AgentStatus::Blocked;
        let tx = backend.event_tx.lock().unwrap().clone().unwrap();
        tx.send(Ok(BackendEvent::StatusChanged {
            id: "p1".into(),
            status: AgentStatus::Blocked,
        }))
        .await
        .unwrap();
        let inv = next_inventory(&mut rx).await;
        assert_eq!(inv.agents[0].status, AgentStatus::Blocked);
        h.shutdown().await;
    }

    #[tokio::test]
    async fn new_agent_triggers_resubscribe() {
        let (backend, h, mut rx) = setup(vec![]);
        next_inventory(&mut rx).await;
        let before = backend.subscribes.load(Ordering::SeqCst);
        backend
            .inventory
            .lock()
            .unwrap()
            .agents
            .push(agent("p2", AgentStatus::Idle));
        let tx = backend.event_tx.lock().unwrap().clone().unwrap();
        tx.send(Ok(BackendEvent::Changed)).await.unwrap();
        let inv = next_inventory(&mut rx).await;
        assert_eq!(inv.agents.len(), 2);
        assert_eq!(backend.subscribes.load(Ordering::SeqCst), before + 1);
        h.shutdown().await;
    }

    #[tokio::test]
    async fn events_lost_resubscribes_and_refetches() {
        let (backend, h, mut rx) = setup(vec![]);
        next_inventory(&mut rx).await;
        let subs = backend.subscribes.load(Ordering::SeqCst);
        let tx = backend.event_tx.lock().unwrap().clone().unwrap();
        tx.send(Err(BackendError::EventsLost)).await.unwrap();
        next_inventory(&mut rx).await;
        assert_eq!(backend.subscribes.load(Ordering::SeqCst), subs + 1);
        h.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn retryable_connect_failure_backs_off_then_connects() {
        let (_backend, h, mut rx) = setup(vec![BackendError::Transport(
            TransportError::Unreachable("down".into()),
        )]);
        assert_eq!(next_state(&mut rx).await, ConnState::Connecting);
        match next_state(&mut rx).await {
            ConnState::Reconnecting { attempt: 1, error } => assert!(error.contains("down")),
            s => panic!("unexpected {s:?}"),
        }
        assert_eq!(next_state(&mut rx).await, ConnState::Connected);
        h.shutdown().await;
    }

    #[tokio::test]
    async fn auth_failure_waits_for_reconnect() {
        let (_backend, h, mut rx) = setup(vec![BackendError::Transport(
            TransportError::AuthNeeded("pw".into()),
        )]);
        assert_eq!(next_state(&mut rx).await, ConnState::Connecting);
        match next_state(&mut rx).await {
            ConnState::Failed { auth: true, .. } => {}
            s => panic!("unexpected {s:?}"),
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(300), rx.recv())
                .await
                .is_err(),
            "must not retry on its own"
        );
        h.reconnect();
        assert_eq!(next_state(&mut rx).await, ConnState::Connecting);
        assert_eq!(next_state(&mut rx).await, ConnState::Connected);
        h.shutdown().await;
    }

    #[tokio::test]
    async fn event_stream_end_reconnects() {
        let (backend, h, mut rx) = setup(vec![]);
        next_inventory(&mut rx).await;
        // Dropping the only sender ends the stream.
        backend.event_tx.lock().unwrap().take();
        match next_state(&mut rx).await {
            ConnState::Reconnecting { attempt: 1, .. } => {}
            s => panic!("unexpected {s:?}"),
        }
        h.shutdown().await;
    }

    #[test]
    fn resolve_picks_session_socket() {
        let cfg = HostConfig {
            name: "box".into(),
            kind: ServerKind::HerdrTui,
            session: Some("work".into()),
            routes: vec!["box".into()],
            socket: None,
            bin: None,
            enabled: true,
        };
        let probe = discover::ProbeResult {
            herdr_bin: Some("/home/u/.local/bin/herdr".into()),
            herdr_sockets: vec![
                discover::HerdrSocket {
                    session: "default".into(),
                    path: "/home/u/.config/herdr/herdr.sock".into(),
                },
                discover::HerdrSocket {
                    session: "work".into(),
                    path: "/home/u/.config/herdr/sessions/work/herdr.sock".into(),
                },
            ],
            ..Default::default()
        };
        let (sock, bin) = resolve_endpoint(&cfg, &probe).unwrap();
        assert!(sock.ends_with("sessions/work/herdr.sock"));
        assert_eq!(bin, "/home/u/.local/bin/herdr");
        let mut missing = cfg.clone();
        missing.session = Some("nope".into());
        let err = resolve_endpoint(&missing, &probe).unwrap_err();
        assert!(matches!(
            err,
            BackendError::Transport(TransportError::NotRunning(_))
        ));
    }
}
