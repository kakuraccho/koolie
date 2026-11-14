//! `~/.config/koolie/config.toml`.

use crate::model::{AgentStatus, ServerKind};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Add the local herdr servers automatically (default true).
    pub local: LocalConfig,
    #[serde(rename = "host", skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<HostConfig>,
    pub notify: NotifyConfig,
    #[serde(rename = "snippet", skip_serializing_if = "Vec::is_empty")]
    pub snippets: Vec<Snippet>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalConfig {
    pub auto: bool,
}

impl Default for LocalConfig {
    fn default() -> Self {
        Self { auto: true }
    }
}

/// One server on one machine.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HostConfig {
    /// Unique name; also the host id.
    pub name: String,
    #[serde(default)]
    pub kind: ServerKind,
    /// herdr session name (`default` when unset) or herdr-core session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// SSH destinations tried in order. Empty means this machine.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<String>,
    /// Socket path on the host; found by probing when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub socket: Option<String>,
    /// herdr / herdr-core executable on the host; found by probing when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bin: Option<String>,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

impl HostConfig {
    pub fn is_local(&self) -> bool {
        self.routes.is_empty()
    }

    pub fn session_name(&self) -> &str {
        self.session.as_deref().unwrap_or("default")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sink {
    /// Desktop notification (D-Bus / macOS Notification Center).
    Desktop,
    /// Terminal bell while the TUI runs.
    Bell,
    /// Run `notify.command`.
    Command,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotifyConfig {
    /// Statuses that trigger a notification.
    pub on: Vec<AgentStatus>,
    pub sinks: Vec<Sink>,
    /// Shell command for the `command` sink. Receives `KOOLIE_HOST`,
    /// `KOOLIE_AGENT`, `KOOLIE_STATUS`, `KOOLIE_TITLE`, `KOOLIE_ID`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self {
            on: vec![AgentStatus::Blocked, AgentStatus::Done],
            sinks: vec![Sink::Desktop, Sink::Bell],
            command: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snippet {
    pub name: String,
    pub text: String,
}

/// `$KOOLIE_CONFIG`, else `$XDG_CONFIG_HOME/koolie/config.toml`.
pub fn default_path() -> PathBuf {
    if let Some(p) = std::env::var_os("KOOLIE_CONFIG") {
        return PathBuf::from(p);
    }
    config_home().join("koolie").join("config.toml")
}

pub fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"))
}

pub fn cache_home() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(|| PathBuf::from(".cache"))
        .join("koolie")
}

impl Config {
    /// Load, or the default when the file does not exist.
    pub fn load(path: &Path) -> Result<Config> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let cfg: Config =
                    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
                cfg.validate()?;
                Ok(cfg)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(self)?;
        // Write through a symlinked config to its target.
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        let mut seen = std::collections::HashSet::new();
        for h in &self.hosts {
            anyhow::ensure!(!h.name.trim().is_empty(), "a host has an empty name");
            anyhow::ensure!(
                seen.insert(h.name.as_str()),
                "duplicate host name `{}`",
                h.name
            );
        }
        Ok(())
    }

    /// Configured hosts plus the auto-detected local servers whose names are
    /// not taken by a configured host.
    pub fn effective_hosts(&self) -> Vec<HostConfig> {
        let mut hosts: Vec<HostConfig> = self.hosts.iter().filter(|h| h.enabled).cloned().collect();
        if self.local.auto {
            for h in detect_local_hosts(&config_home().join("herdr")) {
                let clash = self.hosts.iter().any(|c| {
                    c.name == h.name
                        || (c.is_local()
                            && c.kind == h.kind
                            && c.session_name() == h.session_name())
                });
                if !clash {
                    hosts.push(h);
                }
            }
        }
        hosts
    }
}

/// Local Herdr TUI servers: the default socket and every named session.
pub fn detect_local_hosts(herdr_config_dir: &Path) -> Vec<HostConfig> {
    let mut out = Vec::new();
    let default_sock = herdr_config_dir.join("herdr.sock");
    if default_sock.exists() {
        out.push(local_host("local".into(), None, &default_sock));
    }
    if let Ok(entries) = std::fs::read_dir(herdr_config_dir.join("sessions")) {
        let mut named: Vec<(String, PathBuf)> = entries
            .flatten()
            .filter_map(|e| {
                let sock = e.path().join("herdr.sock");
                let name = e.file_name().into_string().ok()?;
                sock.exists().then_some((name, sock))
            })
            .collect();
        named.sort();
        for (name, sock) in named {
            out.push(local_host(format!("local:{name}"), Some(name), &sock));
        }
    }
    out
}

fn local_host(name: String, session: Option<String>, sock: &Path) -> HostConfig {
    HostConfig {
        name,
        kind: ServerKind::HerdrTui,
        session,
        routes: Vec::new(),
        socket: Some(sock.to_string_lossy().into_owned()),
        bin: None,
        enabled: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_example() {
        let text = r#"
[[host]]
name = "studio"
kind = "herdr-tui"
routes = ["studio.tail1234.ts.net", "studio.lan"]

[[host]]
name = "core-box"
kind = "herdr-core"
routes = ["box"]
enabled = false

[notify]
on = ["blocked"]
sinks = ["desktop", "command"]
command = "echo hi"

[[snippet]]
name = "続けて"
text = "続けてください"
"#;
        let cfg: Config = toml::from_str(text).unwrap();
        assert!(cfg.local.auto);
        assert_eq!(cfg.hosts.len(), 2);
        assert_eq!(cfg.hosts[0].routes.len(), 2);
        assert_eq!(cfg.hosts[1].kind, ServerKind::HerdrCore);
        assert!(!cfg.hosts[1].enabled);
        assert_eq!(cfg.notify.on, vec![AgentStatus::Blocked]);
        assert_eq!(cfg.snippets[0].name, "続けて");
    }

    #[test]
    fn empty_is_default_and_roundtrips() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg, Config::default());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/config.toml");
        let mut cfg = Config::default();
        cfg.hosts.push(HostConfig {
            name: "a".into(),
            kind: ServerKind::HerdrTui,
            session: None,
            routes: vec!["a.lan".into()],
            socket: None,
            bin: None,
            enabled: true,
        });
        cfg.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), cfg);
    }

    #[test]
    fn duplicate_names_rejected() {
        let text = "[[host]]\nname='a'\n[[host]]\nname='a'\n";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.toml");
        std::fs::write(&path, text).unwrap();
        assert!(Config::load(&path).is_err());
    }

    #[test]
    fn detects_local_sockets() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("herdr.sock"), "").unwrap();
        std::fs::create_dir_all(dir.path().join("sessions/work")).unwrap();
        std::fs::write(dir.path().join("sessions/work/herdr.sock"), "").unwrap();
        std::fs::create_dir_all(dir.path().join("sessions/stale")).unwrap();
        let hosts = detect_local_hosts(dir.path());
        let names: Vec<&str> = hosts.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, vec!["local", "local:work"]);
        assert_eq!(hosts[1].session.as_deref(), Some("work"));
    }
}
