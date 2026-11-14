//! Host candidates (ssh config, tailnet, herdr's saved machines) and the
//! probe that finds herdr servers on a host.
//!
//! Placeholder interface; implemented in M3.

use crate::transport::{Transport, TransportError};

/// Where a candidate came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandidateSource {
    SshConfig,
    Tailnet,
    HerdrMachine,
}

/// A machine the user might add.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub source: CandidateSource,
    /// Display name.
    pub name: String,
    /// SSH destination to use.
    pub target: String,
    /// Tailnet only: whether the peer is online.
    pub online: Option<bool>,
    /// Tailnet only: OS reported by tailscale.
    pub os: Option<String>,
}

/// A herdr TUI socket found on a host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HerdrSocket {
    /// `default` or the named session.
    pub session: String,
    pub path: String,
}

/// What a probe found on a host.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProbeResult {
    pub home: Option<String>,
    pub os: Option<String>,
    pub herdr_bin: Option<String>,
    pub herdr_version: Option<String>,
    pub herdr_sockets: Vec<HerdrSocket>,
    pub core_bin: Option<String>,
    /// herdr-core socket path, from `core.lock` or the default.
    pub core_socket: Option<String>,
    pub koolie_bin: Option<String>,
}

/// Run the read-only probe on a host.
pub async fn probe(_transport: &dyn Transport) -> Result<ProbeResult, TransportError> {
    Ok(ProbeResult::default())
}

/// All candidates from every source; sources that fail are skipped.
pub async fn candidates() -> Vec<Candidate> {
    Vec::new()
}
