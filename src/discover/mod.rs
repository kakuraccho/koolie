//! Host candidates (ssh config, tailnet, herdr's saved machines) and the
//! probe that finds herdr servers on a host.
//!
//! Everything here is read-only. Each candidate source is independent: one
//! that fails (missing file, missing command, odd output, timeout) is skipped.

pub mod herdr_machines;
pub mod probe;
pub mod ssh_config;
pub mod tailnet;

pub use herdr_machines::{HerdrMachine, parse_machine_list, split_target};
pub use probe::{
    CoreSocketSource, HerdrSocket, ProbeResult, parse_probe, parse_probe_checked, probe,
    probe_core_session,
};
pub use ssh_config::{SshHost, parse_ssh_config};
pub use tailnet::{TailnetPeer, parse_tailscale_status};

use crate::transport::local::run_command;
use std::path::PathBuf;
use std::time::Duration;

/// Timeout for each external command a candidate source runs.
pub const SOURCE_TIMEOUT: Duration = Duration::from_secs(5);

/// Where a candidate came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CandidateSource {
    SshConfig,
    Tailnet,
    HerdrMachine,
}

/// A machine the user might add.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// The source whose name and target are used.
    pub source: CandidateSource,
    /// Every source that pointed at this machine, `source` first.
    pub sources: Vec<CandidateSource>,
    /// Display name.
    pub name: String,
    /// SSH destination to use.
    pub target: String,
    /// The host the target connects to, when known (ssh `HostName`, MagicDNS
    /// name, the host part of a herdr target).
    pub host_name: Option<String>,
    /// Tailnet only: whether the peer is online.
    pub online: Option<bool>,
    /// Tailnet only: OS reported by tailscale.
    pub os: Option<String>,
    /// Tailnet only: the peer's Tailscale IPs.
    pub ips: Vec<String>,
    /// herdr machine only: the remote herdr session the profile uses.
    pub herdr_session: Option<String>,
    /// herdr machine only: whether the profile is enabled.
    pub enabled: Option<bool>,
}

impl Candidate {
    fn new(source: CandidateSource, name: String, target: String) -> Self {
        Candidate {
            source,
            sources: vec![source],
            name,
            target,
            host_name: None,
            online: None,
            os: None,
            ips: Vec::new(),
            herdr_session: None,
            enabled: None,
        }
    }

    fn add_source(&mut self, s: CandidateSource) {
        if !self.sources.contains(&s) {
            self.sources.push(s);
        }
    }

    fn has(&self, s: CandidateSource) -> bool {
        self.sources.contains(&s)
    }
}

/// All candidates from every source, deduplicated and sorted; sources that
/// fail are skipped.
pub async fn candidates() -> Vec<Candidate> {
    let (ssh, tail, herdr) = tokio::join!(ssh_config_hosts(), tailnet_peers(), herdr_machines());
    merge_candidates(ssh, herdr, tail)
}

/// `~/.ssh/config` hosts (following `Include`).
pub async fn ssh_config_hosts() -> Vec<SshHost> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    tokio::task::spawn_blocking(move || ssh_config::read_user_ssh_config(&home))
        .await
        .unwrap_or_default()
}

/// Tailnet peers that could run herdr, from `tailscale status --json`.
pub async fn tailnet_peers() -> Vec<TailnetPeer> {
    for bin in [
        "tailscale",
        "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
    ] {
        let argv = [bin.to_string(), "status".into(), "--json".into()];
        // A stopped tailscale exits non-zero but may still print JSON.
        if let Ok(out) = run_command(&argv, None, SOURCE_TIMEOUT).await {
            return parse_tailscale_status(&out.stdout_str());
        }
    }
    Vec::new()
}

/// herdr's saved SSH machines, from `herdr machine list --json` on this
/// machine.
pub async fn herdr_machines() -> Vec<HerdrMachine> {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut bins = vec!["herdr".to_string()];
    if !home.is_empty() {
        bins.push(format!("{home}/.local/bin/herdr"));
        bins.push(format!("{home}/.nix-profile/bin/herdr"));
    }
    bins.extend([
        "/opt/homebrew/bin/herdr".into(),
        "/usr/local/bin/herdr".into(),
    ]);
    for bin in bins {
        let argv = [bin, "machine".into(), "list".into(), "--json".into()];
        if let Ok(out) = run_command(&argv, None, SOURCE_TIMEOUT).await {
            if !out.success() {
                return Vec::new();
            }
            return parse_machine_list(&out.stdout_str());
        }
    }
    Vec::new()
}

/// Host comparison key: lowercase, without a trailing dot.
fn host_key(h: &str) -> String {
    h.trim_end_matches('.').to_ascii_lowercase()
}

/// Merge the sources into one list, one entry per machine where they agree:
///
/// - a herdr machine whose target is an ssh alias (or its `HostName`), with
///   no other user or port, joins that ssh entry;
/// - a tailnet peer whose MagicDNS name, short name or Tailscale IP is what
///   an ssh or herdr entry connects to marks those entries with its online
///   state and OS, and is not listed on its own.
///
/// Sorted: herdr machines first, then ssh config entries, then tailnet-only
/// peers; offline machines last within each group; then by name.
pub fn merge_candidates(
    ssh: Vec<SshHost>,
    herdr: Vec<HerdrMachine>,
    tailnet: Vec<TailnetPeer>,
) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    for h in &ssh {
        let mut c = Candidate::new(CandidateSource::SshConfig, h.alias.clone(), h.alias.clone());
        c.host_name = Some(h.resolved_host().to_string());
        out.push(c);
    }

    for m in herdr {
        let (user, host, port) = split_target(&m.target);
        let hk = host_key(host);
        let joined = ssh.iter().position(|h| {
            (host_key(&h.alias) == hk || host_key(h.resolved_host()) == hk)
                && user.is_none_or(|u| h.user.as_deref() == Some(u))
                && port.is_none_or(|p| h.port.unwrap_or(22) == p)
        });
        let c = match joined {
            Some(i) => {
                let c = &mut out[i];
                c.add_source(CandidateSource::HerdrMachine);
                c
            }
            None => {
                let mut c = Candidate::new(
                    CandidateSource::HerdrMachine,
                    m.label.clone(),
                    m.target.clone(),
                );
                c.host_name = Some(host.to_string());
                out.push(c);
                out.last_mut().expect("just pushed")
            }
        };
        // The first profile for a machine wins.
        if c.herdr_session.is_none() {
            c.herdr_session = m.session.clone();
            c.enabled = m.enabled;
        }
    }

    for p in tailnet {
        let mut keys = vec![host_key(&p.dns_name), host_key(p.short_name())];
        keys.extend(p.ips.iter().map(|ip| ip.to_ascii_lowercase()));
        let mut matched = false;
        for c in &mut out {
            let Some(h) = c.host_name.as_deref() else {
                continue;
            };
            if !keys.contains(&host_key(h)) {
                continue;
            }
            matched = true;
            c.add_source(CandidateSource::Tailnet);
            c.online = Some(p.online);
            c.os = p.os.clone();
            c.ips = p.ips.clone();
        }
        if !matched {
            let name = if p.short_name().is_empty() {
                p.host_name.clone()
            } else {
                p.short_name().to_string()
            };
            let mut c = Candidate::new(CandidateSource::Tailnet, name, p.dns_name.clone());
            c.host_name = Some(p.dns_name.clone());
            c.online = Some(p.online);
            c.os = p.os.clone();
            c.ips = p.ips.clone();
            out.push(c);
        }
    }

    out.sort_by_cached_key(|c| {
        let group = if c.has(CandidateSource::HerdrMachine) {
            0
        } else if c.has(CandidateSource::SshConfig) {
            1
        } else {
            2
        };
        (group, c.online == Some(false), c.name.to_lowercase())
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{LocalTransport, Transport};

    fn ssh(alias: &str, host_name: Option<&str>, user: Option<&str>) -> SshHost {
        SshHost {
            alias: alias.into(),
            host_name: host_name.map(Into::into),
            user: user.map(Into::into),
            port: None,
        }
    }

    fn peer(dns: &str, os: &str, online: bool, ip: &str) -> TailnetPeer {
        TailnetPeer {
            host_name: dns.split('.').next().unwrap().into(),
            dns_name: dns.into(),
            os: Some(os.into()),
            online,
            ips: vec![ip.into()],
        }
    }

    fn machine(label: &str, target: &str, session: &str) -> HerdrMachine {
        HerdrMachine {
            id: None,
            label: label.into(),
            target: target.into(),
            session: Some(session.into()),
            enabled: Some(true),
        }
    }

    #[test]
    fn merges_and_sorts() {
        let ssh_hosts = vec![
            ssh("mac", Some("work-mac.tail0000.ts.net."), None),
            ssh("mac-root", Some("work-mac.tail0000.ts.net"), Some("root")),
            ssh("pi", Some("100.64.0.9"), None),
            ssh("build", None, Some("dev")),
            ssh("zeta", Some("zeta.lan"), None),
        ];
        let herdr = vec![
            machine("Build", "dev@build", "agents"),
            machine("Other user", "root@zeta", "default"),
            machine("Remote", "ssh://me@far.example:2222", "default"),
        ];
        let tail = vec![
            peer("work-mac.tail0000.ts.net", "macOS", true, "100.64.0.2"),
            peer("pi.tail0000.ts.net", "linux", false, "100.64.0.9"),
            peer("gaming-pc.tail0000.ts.net", "windows", false, "100.64.0.3"),
            peer("server.tail0000.ts.net", "linux", true, "100.64.0.6"),
        ];
        let c = merge_candidates(ssh_hosts, herdr, tail);
        let names: Vec<&str> = c.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            [
                // herdr machines.
                "build",
                "Other user",
                "Remote",
                // ssh config; pi is offline.
                "mac",
                "mac-root",
                "zeta",
                "pi",
                // tailnet only; gaming-pc is offline.
                "server",
                "gaming-pc",
            ]
        );
        let build = &c[0];
        assert_eq!(
            build.sources,
            [CandidateSource::SshConfig, CandidateSource::HerdrMachine]
        );
        assert_eq!(build.target, "build");
        assert_eq!(build.herdr_session.as_deref(), Some("agents"));
        // Another user is another account: not merged.
        assert_eq!(c[1].source, CandidateSource::HerdrMachine);
        assert_eq!(c[1].target, "root@zeta");
        assert_eq!(c[2].host_name.as_deref(), Some("far.example"));
        let mac = &c[3];
        assert_eq!(
            mac.sources,
            [CandidateSource::SshConfig, CandidateSource::Tailnet]
        );
        assert_eq!(mac.online, Some(true));
        assert_eq!(mac.os.as_deref(), Some("macOS"));
        assert_eq!(c[4].online, Some(true), "both aliases get the peer's state");
        assert_eq!(c[6].online, Some(false), "matched by Tailscale IP");
        assert_eq!(c[6].os.as_deref(), Some("linux"));
        let server = &c[7];
        assert_eq!(server.source, CandidateSource::Tailnet);
        assert_eq!(server.target, "server.tail0000.ts.net");
        assert_eq!(server.os.as_deref(), Some("linux"));
        assert_eq!(c[8].os.as_deref(), Some("windows"));
    }

    #[test]
    fn short_magicdns_name_matches() {
        let c = merge_candidates(
            vec![ssh("box", None, None)],
            vec![machine("box via herdr", "box", "default")],
            vec![peer("box.tail0000.ts.net", "linux", true, "100.64.0.7")],
        );
        assert_eq!(c.len(), 1);
        assert_eq!(
            c[0].sources,
            [
                CandidateSource::SshConfig,
                CandidateSource::HerdrMachine,
                CandidateSource::Tailnet
            ]
        );
        assert_eq!(c[0].online, Some(true));
    }

    #[test]
    fn empty_sources() {
        assert!(merge_candidates(vec![], vec![], vec![]).is_empty());
    }

    // ---- the probe script, run for real against fake homes ----

    fn sh_quote_env(vars: &[(&str, String)]) -> Vec<String> {
        let mut argv = vec!["env".to_string(), "-i".to_string()];
        argv.extend(vars.iter().map(|(k, v)| format!("{k}={v}")));
        argv
    }

    async fn run_probe_env(vars: &[(&str, String)], session: &str) -> (ProbeResult, String) {
        let mut argv = sh_quote_env(vars);
        argv.extend(probe::probe_argv(session));
        let out = LocalTransport
            .exec(&argv, None, Duration::from_secs(20))
            .await
            .unwrap();
        let stdout = out.stdout_str();
        (
            parse_probe_checked(&stdout)
                .unwrap_or_else(|| panic!("no probe result: {stdout:?} {}", out.stderr_str())),
            stdout,
        )
    }

    fn write_exe(path: &std::path::Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[tokio::test]
    async fn script_finds_fake_install() {
        use std::os::unix::net::UnixListener;
        let tmp = tempfile::Builder::new()
            .prefix("kp")
            .tempdir_in("/tmp")
            .unwrap();
        let home = tmp.path().join("Jane Doe");
        let marker = tmp.path().join("core-was-run");
        // herdr on PATH only through ~/.local/bin (the PATH below lacks it).
        write_exe(
            &home.join(".local/bin/herdr"),
            "echo 'herdr 9.9.9'; echo 'second line'",
        );
        write_exe(
            &home.join(".local/bin/herdr-core"),
            &format!("touch '{}'", marker.display()),
        );
        write_exe(&home.join(".cargo/bin/koolie"), "exit 0");
        let cfg = home.join(".config/herdr");
        std::fs::create_dir_all(cfg.join("sessions/my session")).unwrap();
        std::fs::create_dir_all(cfg.join("sessions/stale")).unwrap();
        std::fs::create_dir_all(cfg.join("sessions/empty")).unwrap();
        let _l1 = UnixListener::bind(cfg.join("herdr.sock")).unwrap();
        let _l2 = UnixListener::bind(cfg.join("sessions/my session/herdr.sock")).unwrap();
        // A regular file is not a socket.
        std::fs::write(cfg.join("sessions/stale/herdr.sock"), "").unwrap();
        // core.lock names the bound socket, without a newline.
        let core_sock = tmp.path().join("run dir/core.sock");
        std::fs::create_dir_all(core_sock.parent().unwrap()).unwrap();
        let _l3 = UnixListener::bind(&core_sock).unwrap();
        let state = home.join(".local/state/herdr-core/default");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(state.join("core.lock"), core_sock.to_str().unwrap()).unwrap();

        let vars = [
            ("HOME", home.display().to_string()),
            ("PATH", "/usr/bin:/bin".to_string()),
            (
                "XDG_RUNTIME_DIR",
                tmp.path().join("xdg").display().to_string(),
            ),
        ];
        let (r, stdout) = run_probe_env(&vars, "default").await;
        let home_s = home.display().to_string();
        assert_eq!(r.home.as_deref(), Some(home_s.as_str()));
        assert_eq!(
            r.os.as_deref(),
            Some(if cfg!(target_os = "macos") {
                "Darwin"
            } else {
                "Linux"
            })
        );
        assert!(r.arch.is_some());
        let system_herdr = [
            "/usr/bin/herdr",
            "/bin/herdr",
            "/usr/local/bin/herdr",
            "/opt/homebrew/bin/herdr",
        ]
        .iter()
        .any(|p| std::path::Path::new(p).exists());
        if !system_herdr {
            assert_eq!(r.herdr_bin, Some(format!("{home_s}/.local/bin/herdr")));
            assert_eq!(r.herdr_version.as_deref(), Some("herdr 9.9.9"));
            assert_eq!(r.herdr_version_number(), Some("9.9.9"));
            assert_eq!(r.core_bin, Some(format!("{home_s}/.local/bin/herdr-core")));
        }
        assert_eq!(
            r.herdr_sockets,
            [
                HerdrSocket {
                    session: "default".into(),
                    path: format!("{home_s}/.config/herdr/herdr.sock"),
                },
                HerdrSocket {
                    session: "my session".into(),
                    path: format!("{home_s}/.config/herdr/sessions/my session/herdr.sock"),
                },
            ],
            "{stdout}"
        );
        assert_eq!(r.core_session.as_deref(), Some("default"));
        assert_eq!(r.core_socket.as_deref(), core_sock.to_str());
        assert_eq!(r.core_socket_source, Some(CoreSocketSource::Lock));
        assert!(r.core_socket_present);
        assert_eq!(r.koolie_bin, Some(format!("{home_s}/.cargo/bin/koolie")));
        assert!(!marker.exists(), "the probe ran herdr-core");
    }

    #[tokio::test]
    async fn script_default_core_socket_paths() {
        let tmp = tempfile::Builder::new()
            .prefix("kp")
            .tempdir_in("/tmp")
            .unwrap();
        let home = tmp.path().join("h");
        std::fs::create_dir_all(&home).unwrap();
        let run = tmp.path().join("run");
        let base = vec![
            ("HOME", home.display().to_string()),
            ("PATH", "/usr/bin:/bin".to_string()),
            ("XDG_RUNTIME_DIR", run.display().to_string()),
        ];

        // No core.lock: the runtime dir default, for the named session.
        let (r, _) = run_probe_env(&base, "scratch").await;
        assert_eq!(r.core_session.as_deref(), Some("scratch"));
        assert_eq!(
            r.core_socket,
            Some(format!("{}/herdr-core/scratch.sock", run.display()))
        );
        assert_eq!(r.core_socket_source, Some(CoreSocketSource::Default));
        assert!(!r.core_socket_present);
        assert!(r.herdr_sockets.is_empty());

        // `socket` in config.toml, top level only.
        let conf = home.join(".config/herdr-core/config.toml");
        std::fs::create_dir_all(conf.parent().unwrap()).unwrap();
        std::fs::write(
            &conf,
            "# x\nsocket = \"/tmp/my core.sock\" # c\n[other]\nsocket = \"/no\"\n",
        )
        .unwrap();
        let (r, _) = run_probe_env(&base, "default").await;
        assert_eq!(r.core_socket.as_deref(), Some("/tmp/my core.sock"));
        assert_eq!(r.core_socket_source, Some(CoreSocketSource::Config));

        // HC_SOCKET beats config.
        let mut env = base.clone();
        env.push(("HC_SOCKET", "/tmp/env.sock".into()));
        let (r, _) = run_probe_env(&env, "default").await;
        assert_eq!(r.core_socket.as_deref(), Some("/tmp/env.sock"));
        assert_eq!(r.core_socket_source, Some(CoreSocketSource::Env));

        // XDG_STATE_HOME / XDG_CONFIG_HOME move the lock and the herdr dir.
        std::fs::remove_file(&conf).unwrap();
        let state = tmp.path().join("state/herdr-core/default");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(state.join("core.lock"), "/tmp/locked.sock").unwrap();
        let xcfg = tmp.path().join("xcfg");
        std::fs::create_dir_all(xcfg.join("herdr")).unwrap();
        let _l = std::os::unix::net::UnixListener::bind(xcfg.join("herdr/herdr.sock")).unwrap();
        let mut env = base.clone();
        env.push((
            "XDG_STATE_HOME",
            tmp.path().join("state").display().to_string(),
        ));
        env.push(("XDG_CONFIG_HOME", xcfg.display().to_string()));
        let (r, _) = run_probe_env(&env, "default").await;
        assert_eq!(r.core_socket.as_deref(), Some("/tmp/locked.sock"));
        assert_eq!(r.core_socket_source, Some(CoreSocketSource::Lock));
        assert_eq!(r.herdr_sockets.len(), 1);
        assert_eq!(r.herdr_sockets[0].session, "default");
    }

    #[tokio::test]
    async fn script_output_survives_chatty_shell() {
        let tmp = tempfile::tempdir().unwrap();
        // What an rc file or MOTD might print before the probe, without a
        // trailing newline, plus noise after it.
        let argv = vec![
            "env".to_string(),
            "-i".into(),
            format!("HOME={}", tmp.path().display()),
            "PATH=/usr/bin:/bin".into(),
            "sh".into(),
            "-c".into(),
            "printf 'Welcome!\\nhome=/evil\\nno newline'; sh -c \"$1\" koolie-probe default; echo bye; echo home=/after".into(),
            "outer".into(),
            probe::SCRIPT.into(),
        ];
        let out = LocalTransport
            .exec(&argv, None, Duration::from_secs(20))
            .await
            .unwrap();
        let r = parse_probe_checked(&out.stdout_str()).expect("probe result");
        assert_eq!(r.home, Some(tmp.path().display().to_string()));
    }

    #[tokio::test]
    async fn probe_refuses_bad_session() {
        let err = probe_core_session(&LocalTransport, "a/b")
            .await
            .unwrap_err();
        assert!(matches!(err, crate::transport::TransportError::Io(_)));
    }

    /// Runs the real probe on this machine (needs herdr installed and
    /// running).
    #[tokio::test]
    #[ignore]
    async fn real_local_probe() {
        let r = probe(&LocalTransport).await.unwrap();
        eprintln!("{r:#?}");
        assert!(r.herdr_bin.is_some());
        assert!(r.herdr_version_number().is_some());
        let home = std::env::var("HOME").unwrap();
        assert!(r.herdr_sockets.iter().any(
            |s| s.session == "default" && s.path == format!("{home}/.config/herdr/herdr.sock")
        ));
    }

    /// Runs the real candidate sources on this machine.
    #[tokio::test]
    #[ignore]
    async fn real_candidates() {
        for c in candidates().await {
            eprintln!(
                "{:?} {} -> {} {:?} {:?}",
                c.sources, c.name, c.target, c.online, c.os
            );
        }
    }
}
