use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use koolie::app::{self, App, ConnectorFactory};
use koolie::config::{self, Config, HostConfig};
use koolie::model::ServerKind;
use koolie::transport::{LocalTransport, SshTransport, Transport};
use koolie::{demo, discover, tui, usage, watch};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

#[derive(Parser, Debug)]
#[command(
    name = "koolie",
    version,
    about = "Console for coding agents running in herdr, here and on other machines"
)]
struct Cli {
    /// Config file (default: ~/.config/koolie/config.toml)
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Show a fixed demo fleet instead of real hosts
    #[arg(long)]
    demo: bool,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run without a screen: notify on blocked/done and keep the status file current
    Watch,
    /// Print a short agent summary (for tmux status lines and prompts)
    Status {
        #[arg(long, default_value = "plain", value_parser = ["plain", "tmux", "json"])]
        format: String,
    },
    /// Token usage and estimated cost of Claude Code and Codex on this machine
    Usage {
        #[arg(long)]
        json: bool,
        #[arg(long, default_value_t = 30)]
        days: u32,
    },
    /// Manage hosts
    Hosts {
        #[command(subcommand)]
        cmd: HostsCmd,
    },
    /// Show what herdr servers a host runs (`local` for this machine)
    Probe { target: String },
}

#[derive(Subcommand, Debug)]
enum HostsCmd {
    /// List configured and auto-detected hosts
    List,
    /// List machines you could add (ssh config, tailnet, herdr machines)
    Discover,
    /// Probe an SSH target and add the herdr servers found on it
    Add {
        target: String,
        #[arg(long)]
        name: Option<String>,
        /// Only add this herdr session
        #[arg(long)]
        session: Option<String>,
        /// Add herdr-core instead of Herdr TUI
        #[arg(long)]
        core: bool,
    },
    /// Remove a configured host
    Remove { name: String },
}

fn main() -> Result<()> {
    // Before any thread exists: `time` can only read the local offset then.
    let utc_offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
    let cli = Cli::parse();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(run(cli, utc_offset))
}

async fn run(cli: Cli, utc_offset: time::UtcOffset) -> Result<()> {
    let cfg_path = cli.config.clone().unwrap_or_else(config::default_path);
    let cfg = Config::load(&cfg_path)?;
    match cli.cmd {
        None => {
            let (hosts, connectors): (Vec<HostConfig>, ConnectorFactory) = if cli.demo {
                (
                    demo::hosts(),
                    Arc::new(|h: &HostConfig| {
                        Arc::new(demo::DemoConnector::new(&h.name))
                            as Arc<dyn koolie::session::Connector>
                    }),
                )
            } else {
                (cfg.effective_hosts(), app::real_connectors())
            };
            let (tx, rx) = mpsc::unbounded_channel();
            let app = App::new(cfg, cfg_path, hosts, connectors, tx.clone(), utc_offset);
            tui::run(app, rx, tx).await
        }
        Some(Cmd::Watch) => {
            let hosts = cfg.effective_hosts();
            watch::watch(cfg, hosts, None).await
        }
        Some(Cmd::Status { format }) => {
            let s = match watch::read_live_status() {
                Some(s) => s,
                None => watch::local_status(&cfg.effective_hosts()).await,
            };
            println!("{}", watch::render_status(&s, &format));
            Ok(())
        }
        Some(Cmd::Usage { json, days }) => {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .context("HOME is not set")?;
            let report =
                tokio::task::spawn_blocking(move || usage::scan(&home, days, utc_offset)).await??;
            if json {
                println!("{}", serde_json::to_string(&report)?);
            } else {
                print!("{}", usage::render_text(&report));
            }
            Ok(())
        }
        Some(Cmd::Probe { target }) => {
            let t: Arc<dyn Transport> = if target == "local" {
                Arc::new(LocalTransport)
            } else {
                Arc::new(SshTransport::new(
                    format!("probe-{}", std::process::id()),
                    vec![target.clone()],
                ))
            };
            let r = discover::probe(t.as_ref()).await;
            t.shutdown().await;
            let p = r?;
            println!("home:          {}", p.home.as_deref().unwrap_or("?"));
            println!("os:            {}", p.os.as_deref().unwrap_or("?"));
            println!(
                "herdr:         {} {}",
                p.herdr_bin.as_deref().unwrap_or("(not found)"),
                p.herdr_version.as_deref().unwrap_or("")
            );
            for s in &p.herdr_sockets {
                println!("  session {:<10} {}", s.session, s.path);
            }
            println!(
                "herdr-core:    {}",
                p.core_bin.as_deref().unwrap_or("(not found)")
            );
            if let (Some(_), Some(s)) = (&p.core_bin, &p.core_socket) {
                println!("  socket       {s}");
            }
            println!(
                "koolie:        {}",
                p.koolie_bin.as_deref().unwrap_or("(not found)")
            );
            Ok(())
        }
        Some(Cmd::Hosts { cmd }) => hosts_cmd(cmd, cfg, &cfg_path).await,
    }
}

async fn hosts_cmd(cmd: HostsCmd, mut cfg: Config, cfg_path: &std::path::Path) -> Result<()> {
    match cmd {
        HostsCmd::List => {
            for h in cfg.effective_hosts() {
                let auto = !cfg.hosts.iter().any(|c| c.name == h.name);
                println!(
                    "{:<20} {:<10} {:<10} {}{}",
                    h.name,
                    h.kind.as_str(),
                    h.session_name(),
                    if h.is_local() {
                        "local".to_string()
                    } else {
                        h.routes.join(" | ")
                    },
                    if auto { "  (auto)" } else { "" }
                );
            }
            Ok(())
        }
        HostsCmd::Discover => {
            for c in discover::candidates().await {
                let online = match c.online {
                    Some(true) => "online",
                    Some(false) => "offline",
                    None => "",
                };
                println!(
                    "{:<12} {:<24} {:<40} {} {}",
                    format!("{:?}", c.source),
                    c.name,
                    c.target,
                    c.os.unwrap_or_default(),
                    online
                );
            }
            Ok(())
        }
        HostsCmd::Add {
            target,
            name,
            session,
            core,
        } => {
            let t = SshTransport::new(
                format!("probe-{}", std::process::id()),
                vec![target.clone()],
            );
            let probe = discover::probe(&t).await;
            t.shutdown().await;
            let probe = probe?;
            let mut options = app::host_options(&target, &probe);
            options.retain(|(_, h, _)| {
                (h.kind == ServerKind::HerdrCore) == core
                    && session.as_deref().is_none_or(|s| h.session_name() == s)
            });
            if options.is_empty() {
                bail!("no matching herdr server found on {target} (try `koolie probe {target}`)");
            }
            let base = name.unwrap_or_else(|| app::short_host(&target));
            for (i, (_, mut h, _)) in options.into_iter().enumerate() {
                let suffix = match h.session.as_deref() {
                    Some(s) => format!(":{s}"),
                    None if h.kind == ServerKind::HerdrCore => "-core".into(),
                    None if i == 0 => String::new(),
                    None => format!("-{i}"),
                };
                h.name = format!("{base}{suffix}");
                if cfg.hosts.iter().any(|c| c.name == h.name) {
                    bail!("a host named `{}` already exists", h.name);
                }
                println!(
                    "added {} ({} {})",
                    h.name,
                    h.kind.as_str(),
                    h.session_name()
                );
                cfg.hosts.push(h);
            }
            cfg.save(cfg_path)
        }
        HostsCmd::Remove { name } => {
            let before = cfg.hosts.len();
            cfg.hosts.retain(|h| h.name != name);
            if cfg.hosts.len() == before {
                bail!("no configured host named `{name}`");
            }
            cfg.save(cfg_path)?;
            println!("removed {name}");
            Ok(())
        }
    }
}
