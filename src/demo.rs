//! `koolie --demo`: a fixed fleet without any herdr, for trying the UI.

use crate::backend::{
    Backend, BackendError, BackendEvent, Capabilities, ReadSource, Result, Screen, ServerInfo,
};
use crate::config::HostConfig;
use crate::model::{Agent, AgentKey, AgentStatus, Inventory, ServerKind};
use crate::session::Connector;
use crate::transport::{LocalTransport, Transport};
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

pub fn hosts() -> Vec<HostConfig> {
    ["laptop", "studio", "build-box"]
        .iter()
        .map(|n| HostConfig {
            name: n.to_string(),
            kind: ServerKind::HerdrTui,
            session: None,
            routes: vec![format!("{n}.tailnet.ts.net")],
            socket: None,
            bin: None,
            enabled: true,
        })
        .collect()
}

#[derive(Debug)]
pub struct DemoBackend {
    host: String,
    agents: Mutex<Vec<Agent>>,
    log: Mutex<Vec<String>>,
}

fn agent(
    host: &str,
    id: &str,
    prog: &str,
    title: &str,
    status: AgentStatus,
    seq: u64,
    cwd: &str,
) -> Agent {
    Agent {
        key: AgentKey {
            host: host.into(),
            id: id.into(),
        },
        agent: Some(prog.into()),
        display_agent: None,
        title: Some(title.into()),
        status,
        workspace_id: "w1".into(),
        workspace_label: Some(cwd.rsplit('/').next().unwrap_or(cwd).into()),
        tab_id: None,
        cwd: Some(cwd.into()),
        revision: 1,
        state_change_seq: seq,
        session: None,
        focused: false,
    }
}

impl DemoBackend {
    pub fn new(host: &str) -> Self {
        use AgentStatus::*;
        let agents = match host {
            "laptop" => vec![
                agent(
                    host,
                    "w1:p1",
                    "claude",
                    "ログイン画面のリファクタ",
                    Blocked,
                    9,
                    "/Users/me/dev/app",
                ),
                agent(
                    host,
                    "w1:p2",
                    "codex",
                    "API クライアントのテスト追加",
                    Working,
                    7,
                    "/Users/me/dev/api",
                ),
            ],
            "studio" => vec![
                agent(
                    host,
                    "w1:p1",
                    "claude",
                    "flake の依存更新",
                    Done,
                    8,
                    "/home/me/nix-config",
                ),
                agent(
                    host,
                    "w2:p1",
                    "claude",
                    "ドキュメント整理",
                    Idle,
                    2,
                    "/home/me/dev/docs",
                ),
            ],
            _ => vec![agent(
                host,
                "w1:p1",
                "codex",
                "ログ解析バッチ",
                Working,
                5,
                "/srv/batch",
            )],
        };
        Self {
            host: host.into(),
            agents: Mutex::new(agents),
            log: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl Backend for DemoBackend {
    fn kind(&self) -> ServerKind {
        ServerKind::HerdrTui
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            prompt: true,
            insert_text: true,
            sessions: false,
        }
    }
    async fn hello(&self) -> Result<ServerInfo> {
        Ok(ServerInfo {
            version: "demo".into(),
            protocol: None,
        })
    }
    async fn snapshot(&self) -> Result<Inventory> {
        Ok(Inventory {
            server_version: Some("demo".into()),
            agents: self.agents.lock().map(|a| a.clone()).unwrap_or_default(),
        })
    }
    async fn subscribe(
        &self,
        _ids: &[String],
    ) -> Result<mpsc::Receiver<std::result::Result<BackendEvent, BackendError>>> {
        let (tx, rx) = mpsc::channel(1);
        // Keep the stream open for the program's life.
        tokio::spawn(async move {
            let _tx = tx;
            std::future::pending::<()>().await;
        });
        Ok(rx)
    }
    async fn read(
        &self,
        id: &str,
        _source: ReadSource,
        _lines: Option<u32>,
        _ansi: bool,
    ) -> Result<Screen> {
        let a = self
            .agents
            .lock()
            .ok()
            .and_then(|v| v.iter().find(|a| a.key.id == id).cloned());
        let Some(a) = a else {
            return Err(BackendError::Server {
                code: "pane_not_found".into(),
                message: id.into(),
            });
        };
        let mut text = format!(
            "\x1b[1;36m● {}\x1b[0m  {}\r\n\r\n\x1b[2m{}\x1b[0m\r\n\r\n",
            a.program(),
            a.display_title(),
            a.cwd.clone().unwrap_or_default()
        );
        text.push_str("\x1b[32m⏺\x1b[0m Read(src/lib.rs)\r\n  ⎿  Read 120 lines\r\n\r\n");
        for l in self.log.lock().map(|l| l.clone()).unwrap_or_default() {
            text.push_str(&format!("\x1b[1m> {l}\x1b[0m\r\n"));
        }
        if a.status == AgentStatus::Blocked {
            text.push_str(
                "\r\n\x1b[33m╭──────────────────────────────────────────╮\x1b[0m\r\n\
                 \x1b[33m│\x1b[0m Bash コマンドを実行しますか？            \x1b[33m│\x1b[0m\r\n\
                 \x1b[33m│\x1b[0m   cargo test --all                      \x1b[33m│\x1b[0m\r\n\
                 \x1b[33m│\x1b[0m ❯ 1. Yes                                \x1b[33m│\x1b[0m\r\n\
                 \x1b[33m│\x1b[0m   2. Yes, and don't ask again           \x1b[33m│\x1b[0m\r\n\
                 \x1b[33m│\x1b[0m   3. No                                 \x1b[33m│\x1b[0m\r\n\
                 \x1b[33m╰──────────────────────────────────────────╯\x1b[0m\r\n",
            );
        }
        Ok(Screen {
            text,
            revision: self.log.lock().map(|l| l.len() as u64).unwrap_or(0) + a.state_change_seq,
            ansi: true,
            truncated: false,
        })
    }
    async fn prompt(&self, id: &str, text: &str) -> Result<()> {
        if let Ok(mut l) = self.log.lock() {
            l.push(text.to_string());
        }
        if let Ok(mut v) = self.agents.lock()
            && let Some(a) = v.iter_mut().find(|a| a.key.id == id)
        {
            a.status = AgentStatus::Working;
        }
        Ok(())
    }
    async fn send_keys(&self, id: &str, _keys: &[String]) -> Result<()> {
        if let Ok(mut v) = self.agents.lock()
            && let Some(a) = v.iter_mut().find(|a| a.key.id == id)
            && a.status == AgentStatus::Blocked
        {
            a.status = AgentStatus::Working;
        }
        Ok(())
    }
    async fn insert_text(&self, _id: &str, text: &str) -> Result<()> {
        if let Ok(mut l) = self.log.lock() {
            l.push(format!("(入力欄) {text}"));
        }
        Ok(())
    }
    fn attach_argv(&self, id: &str) -> Vec<String> {
        vec![
            "sh".into(),
            "-c".into(),
            format!(
                "echo 'デモ: {} の {} に入るところです'; sleep 2",
                self.host, id
            ),
        ]
    }
}

#[derive(Debug)]
pub struct DemoConnector {
    backend: Arc<DemoBackend>,
}

impl DemoConnector {
    pub fn new(host: &str) -> Self {
        Self {
            backend: Arc::new(DemoBackend::new(host)),
        }
    }
}

#[async_trait]
impl Connector for DemoConnector {
    fn transport(&self) -> Arc<dyn Transport> {
        Arc::new(LocalTransport)
    }
    async fn connect(&self) -> std::result::Result<Arc<dyn Backend>, BackendError> {
        Ok(self.backend.clone())
    }
}
