//! herdr-core (`hc`) backend. Placeholder: implemented in M6.

use super::{
    Backend, BackendError, BackendEvent, Capabilities, ReadSource, Result, Screen, ServerInfo,
};
use crate::model::{HostId, Inventory, ServerKind};
use crate::transport::Transport;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::mpsc;

#[derive(Debug)]
pub struct HerdrCore {
    #[allow(dead_code)]
    host: HostId,
    #[allow(dead_code)]
    transport: Arc<dyn Transport>,
    #[allow(dead_code)]
    socket: String,
    core_bin: String,
}

impl HerdrCore {
    pub fn new(
        host: HostId,
        transport: Arc<dyn Transport>,
        socket: String,
        core_bin: String,
    ) -> Self {
        Self {
            host,
            transport,
            socket,
            core_bin,
        }
    }
}

fn todo_err() -> BackendError {
    BackendError::Unsupported("herdr-core backend not implemented yet".into())
}

#[async_trait]
impl Backend for HerdrCore {
    fn kind(&self) -> ServerKind {
        ServerKind::HerdrCore
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }
    async fn hello(&self) -> Result<ServerInfo> {
        Err(todo_err())
    }
    async fn snapshot(&self) -> Result<Inventory> {
        Err(todo_err())
    }
    async fn subscribe(
        &self,
        _ids: &[String],
    ) -> Result<mpsc::Receiver<std::result::Result<BackendEvent, BackendError>>> {
        Err(todo_err())
    }
    async fn read(
        &self,
        _id: &str,
        _source: ReadSource,
        _lines: Option<u32>,
        _ansi: bool,
    ) -> Result<Screen> {
        Err(todo_err())
    }
    async fn prompt(&self, _id: &str, _text: &str) -> Result<()> {
        Err(todo_err())
    }
    async fn send_keys(&self, _id: &str, _keys: &[String]) -> Result<()> {
        Err(todo_err())
    }
    async fn insert_text(&self, _id: &str, _text: &str) -> Result<()> {
        Err(todo_err())
    }
    fn attach_argv(&self, id: &str) -> Vec<String> {
        vec![self.core_bin.clone(), "attach".into(), id.into()]
    }
}
