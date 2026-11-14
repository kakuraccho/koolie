//! A host reached through the system OpenSSH client.
//!
//! One background master connection per host (`ssh -M -N`); herdr sockets are
//! forwarded on demand to local Unix sockets with `ssh -O forward -L`, and
//! commands reuse the master with `ssh -S`. Authentication is whatever the
//! user's `~/.ssh/config`, agent and known_hosts say; koolie never prompts
//! (`BatchMode=yes`), so a host that needs a password reports
//! [`TransportError::AuthNeeded`] instead of hanging.

use super::{ExecOutput, Transport, TransportError, runtime_dir, shell_join};
use crate::transport::local::run_command;
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub struct SshTransport {
    /// Host id, used only to name runtime files.
    host: String,
    /// SSH destinations to try in order (aliases, `user@host`, `ssh://...`).
    routes: Vec<String>,
    ssh_bin: String,
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    master: Option<Child>,
    /// Route the master is connected to.
    target: Option<String>,
    /// host socket path -> local forwarded socket path
    forwards: HashMap<String, PathBuf>,
}

impl SshTransport {
    pub fn new(host: impl Into<String>, routes: Vec<String>) -> Self {
        let ssh_bin = std::env::var("KOOLIE_SSH").unwrap_or_else(|_| "ssh".to_string());
        Self::with_ssh_bin(host, routes, ssh_bin)
    }

    pub fn with_ssh_bin(host: impl Into<String>, routes: Vec<String>, ssh_bin: String) -> Self {
        Self {
            host: host.into(),
            routes,
            ssh_bin,
            state: Mutex::new(State::default()),
        }
    }

    fn file_stem(&self) -> String {
        let safe: String = self
            .host
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .take(24)
            .collect();
        // The hash keeps names that sanitize alike (`自宅` / `会社`,
        // `box.lan` / `box_lan`) apart.
        format!(
            "{safe}-{:08x}-{}",
            fnv1a(&self.host) as u32,
            std::process::id()
        )
    }

    fn control_path(&self) -> PathBuf {
        runtime_dir().join(format!("{}.ctl", self.file_stem()))
    }

    fn forward_path(&self, host_socket: &str) -> PathBuf {
        runtime_dir().join(format!(
            "{}-{:016x}.sock",
            self.file_stem(),
            fnv1a(host_socket)
        ))
    }

    fn base_opts(&self) -> Vec<String> {
        vec![
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            format!("ControlPath={}", self.control_path().display()),
        ]
    }

    /// argv for the background master.
    fn master_argv(&self, target: &str) -> Vec<String> {
        let mut v = vec![self.ssh_bin.clone()];
        v.extend(self.base_opts());
        v.extend(
            [
                "-M",
                "-N",
                "-o",
                "ControlPersist=no",
                "-o",
                "ConnectTimeout=10",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=3",
                "-o",
                "StreamLocalBindUnlink=yes",
                "-o",
                "ExitOnForwardFailure=yes",
            ]
            .map(String::from),
        );
        v.push(target.into());
        v
    }

    fn control_argv(&self, target: &str, op: &str, extra: &[String]) -> Vec<String> {
        let mut v = vec![self.ssh_bin.clone()];
        v.extend(self.base_opts());
        v.push("-O".into());
        v.push(op.into());
        v.extend(extra.iter().cloned());
        v.push(target.into());
        v
    }

    fn exec_argv(&self, target: &str, argv: &[String]) -> Vec<String> {
        let mut v = vec![self.ssh_bin.clone()];
        v.extend(self.base_opts());
        v.push("-T".into());
        v.push(target.into());
        v.push("--".into());
        v.push(shell_join(argv));
        v
    }

    async fn master_alive(&self, st: &mut State) -> bool {
        let Some(child) = st.master.as_mut() else {
            return false;
        };
        if !matches!(child.try_wait(), Ok(None)) {
            return false;
        }
        let Some(target) = st.target.clone() else {
            return false;
        };
        matches!(
            run_command(&self.control_argv(&target, "check", &[]), None, CONTROL_TIMEOUT).await,
            Ok(out) if out.success()
        )
    }

    async fn start_master(&self, target: &str) -> Result<Child, TransportError> {
        std::fs::create_dir_all(runtime_dir())?;
        let _ = std::fs::remove_file(self.control_path());
        let argv = self.master_argv(target);
        let mut child = Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| TransportError::Io(format!("{}: {e}", self.ssh_bin)))?;
        let deadline = tokio::time::Instant::now() + CONNECT_TIMEOUT;
        loop {
            if let Ok(Some(_status)) = child.try_wait() {
                let mut err = String::new();
                if let Some(mut pipe) = child.stderr.take() {
                    let _ = pipe.read_to_string(&mut err).await;
                }
                return Err(classify_ssh_error(&err));
            }
            if self.control_path().exists() {
                let check = run_command(
                    &self.control_argv(target, "check", &[]),
                    None,
                    CONTROL_TIMEOUT,
                )
                .await;
                if matches!(check, Ok(ref out) if out.success()) {
                    return Ok(child);
                }
            }
            if tokio::time::Instant::now() >= deadline {
                let _ = child.kill().await;
                return Err(TransportError::Timeout(format!(
                    "ssh {target} did not connect"
                )));
            }
            tokio::time::sleep(Duration::from_millis(120)).await;
        }
    }

    async fn ensure_locked(&self, st: &mut State) -> Result<String, TransportError> {
        if self.master_alive(st).await {
            return Ok(st.target.clone().expect("alive master has a target"));
        }
        if let Some(mut old) = st.master.take() {
            let _ = old.kill().await;
        }
        for path in st.forwards.drain().map(|(_, p)| p) {
            let _ = std::fs::remove_file(path);
        }
        st.target = None;
        if self.routes.is_empty() {
            return Err(TransportError::Unreachable(
                "no SSH route configured".into(),
            ));
        }
        let mut errors: Vec<TransportError> = Vec::new();
        for route in &self.routes {
            match self.start_master(route).await {
                Ok(child) => {
                    st.master = Some(child);
                    st.target = Some(route.clone());
                    return Ok(route.clone());
                }
                Err(e) => errors.push(e),
            }
        }
        // Prefer the error that needs the user over a generic network error.
        let pick = errors
            .iter()
            .find(|e| !e.is_retryable())
            .or_else(|| errors.last())
            .cloned()
            .expect("at least one route was tried");
        Err(pick)
    }
}

#[async_trait]
impl Transport for SshTransport {
    fn label(&self) -> String {
        match self.state.try_lock() {
            Ok(st) => st.target.clone().unwrap_or_else(|| self.routes.join(" | ")),
            Err(_) => self.routes.join(" | "),
        }
    }

    fn is_local(&self) -> bool {
        false
    }

    async fn ensure(&self) -> Result<(), TransportError> {
        let mut st = self.state.lock().await;
        self.ensure_locked(&mut st).await.map(|_| ())
    }

    async fn connect(&self, host_socket: &str) -> Result<UnixStream, TransportError> {
        let mut st = self.state.lock().await;
        let target = self.ensure_locked(&mut st).await?;
        if let Some(local) = st.forwards.get(host_socket)
            && let Ok(stream) = UnixStream::connect(local).await
        {
            return Ok(stream);
        }
        let local = self.forward_path(host_socket);
        // A forward this master already has (an earlier call was cancelled
        // after `-O forward` but before recording it) is simply reused:
        // unlinking it would break it for good.
        if let Ok(stream) = UnixStream::connect(&local).await {
            st.forwards.insert(host_socket.to_string(), local.clone());
            return Ok(stream);
        }
        let _ = std::fs::remove_file(&local);
        let spec = format!("{}:{}", local.display(), host_socket);
        let out = run_command(
            &self.control_argv(&target, "forward", &["-L".into(), spec]),
            None,
            CONTROL_TIMEOUT,
        )
        .await?;
        if !out.success() {
            return Err(TransportError::Io(format!(
                "ssh forward to {host_socket} failed: {}",
                out.stderr_str().trim()
            )));
        }
        st.forwards.insert(host_socket.to_string(), local.clone());
        wait_for_path(&local, Duration::from_secs(3)).await;
        UnixStream::connect(&local)
            .await
            .map_err(|e| TransportError::Io(format!("forwarded {host_socket}: {e}")))
    }

    async fn exec(
        &self,
        argv: &[String],
        stdin: Option<&[u8]>,
        timeout: Duration,
    ) -> Result<ExecOutput, TransportError> {
        let target = {
            let mut st = self.state.lock().await;
            self.ensure_locked(&mut st).await?
        };
        let out = run_command(&self.exec_argv(&target, argv), stdin, timeout).await?;
        // 255 is ssh's own failure, not the remote command's.
        if out.code == Some(255) {
            return Err(classify_ssh_error(&out.stderr_str()));
        }
        Ok(out)
    }

    fn interactive_argv(&self, argv: &[String]) -> Vec<String> {
        let target = self
            .state
            .try_lock()
            .ok()
            .and_then(|st| st.target.clone())
            .or_else(|| self.routes.first().cloned())
            .unwrap_or_default();
        let mut v = vec![self.ssh_bin.clone()];
        v.extend([
            "-o".to_string(),
            format!("ControlPath={}", self.control_path().display()),
            "-t".to_string(),
            target,
            "--".to_string(),
            shell_join(argv),
        ]);
        v
    }

    async fn shutdown(&self) {
        let mut st = self.state.lock().await;
        if let Some(target) = st.target.clone() {
            let _ = run_command(
                &self.control_argv(&target, "exit", &[]),
                None,
                CONTROL_TIMEOUT,
            )
            .await;
        }
        if let Some(mut child) = st.master.take() {
            let _ = child.kill().await;
        }
        for path in st.forwards.drain().map(|(_, p)| p) {
            let _ = std::fs::remove_file(path);
        }
        let _ = std::fs::remove_file(self.control_path());
        st.target = None;
    }
}

impl Drop for SshTransport {
    fn drop(&mut self) {
        // The master is killed by kill_on_drop; clean up the files we own.
        if let Ok(st) = self.state.try_lock() {
            for path in st.forwards.values() {
                let _ = std::fs::remove_file(path);
            }
        }
        let _ = std::fs::remove_file(self.control_path());
    }
}

async fn wait_for_path(path: &Path, max: Duration) {
    let deadline = tokio::time::Instant::now() + max;
    while !path.exists() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

/// Map OpenSSH's stderr to a [`TransportError`].
pub fn classify_ssh_error(stderr: &str) -> TransportError {
    let msg = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("Warning: Permanently added"))
        .collect::<Vec<_>>()
        .join(" / ");
    let lower = msg.to_ascii_lowercase();
    let msg = if msg.is_empty() {
        "ssh failed".to_string()
    } else {
        msg
    };
    if lower.contains("host key verification failed")
        || lower.contains("remote host identification has changed")
        || lower.contains("no matching host key")
    {
        TransportError::HostKey(msg)
    } else if lower.contains("permission denied")
        || lower.contains("too many authentication failures")
        || lower.contains("authentication failed")
        || lower.contains("passphrase")
    {
        TransportError::AuthNeeded(msg)
    } else if lower.contains("timed out") {
        TransportError::Timeout(msg)
    } else {
        // DNS failures, refused, no route, connection closed, ...
        TransportError::Unreachable(msg)
    }
}

fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify() {
        assert!(matches!(
            classify_ssh_error("user@h: Permission denied (publickey,password).\n"),
            TransportError::AuthNeeded(_)
        ));
        assert!(matches!(
            classify_ssh_error("Host key verification failed.\n"),
            TransportError::HostKey(_)
        ));
        assert!(matches!(
            classify_ssh_error("ssh: connect to host x port 22: Connection timed out"),
            TransportError::Timeout(_)
        ));
        assert!(matches!(
            classify_ssh_error("ssh: Could not resolve hostname nope: Name or service not known"),
            TransportError::Unreachable(_)
        ));
        assert!(matches!(
            classify_ssh_error(""),
            TransportError::Unreachable(_)
        ));
    }

    #[test]
    fn argv_shapes() {
        let t = SshTransport::with_ssh_bin("my host", vec!["box".into()], "ssh".into());
        let m = t.master_argv("box");
        assert!(m.contains(&"-M".to_string()) && m.contains(&"-N".to_string()));
        assert!(m.contains(&"BatchMode=yes".to_string()));
        assert_eq!(m.last().unwrap(), "box");
        let e = t.exec_argv(
            "box",
            &["herdr".into(), "agent".into(), "read".into(), "a b".into()],
        );
        assert_eq!(e.last().unwrap(), "herdr agent read 'a b'");
        assert_eq!(e[e.len() - 2], "--");
        let i = t.interactive_argv(&[
            "herdr".into(),
            "agent".into(),
            "attach".into(),
            "w1:p1".into(),
        ]);
        assert!(i.contains(&"-t".to_string()));
        assert_eq!(i.last().unwrap(), "herdr agent attach w1:p1");
        assert!(t.control_path().to_string_lossy().contains("my_host-"));
        // Names that sanitize alike still get their own files.
        let a = SshTransport::with_ssh_bin("自宅", vec!["a".into()], "ssh".into());
        let b = SshTransport::with_ssh_bin("会社", vec!["b".into()], "ssh".into());
        assert_ne!(a.control_path(), b.control_path());
        let c = SshTransport::with_ssh_bin("box.lan", vec!["a".into()], "ssh".into());
        let d = SshTransport::with_ssh_bin("box_lan", vec!["b".into()], "ssh".into());
        assert_ne!(c.control_path(), d.control_path());
        assert_ne!(c.forward_path("/s"), d.forward_path("/s"));
    }

    /// A fake `ssh` that runs the remote command locally, so exec can be
    /// tested end to end without a server.
    fn fake_ssh(dir: &Path) -> String {
        let path = dir.join("fake-ssh");
        std::fs::write(
            &path,
            r#"#!/bin/sh
# Parse just enough of ssh's argv.
ctl=""; master=0; op=""; cmd=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) case "$2" in ControlPath=*) ctl="${2#ControlPath=}";; esac; shift 2;;
    -M) master=1; shift;;
    -N|-T|-t) shift;;
    -O) op="$2"; shift 2;;
    -L) shift 2;;
    --) shift; cmd="$*"; break;;
    *) host="$1"; shift;;
  esac
done
if [ "$host" = "denied" ]; then echo "denied: Permission denied (publickey)." >&2; exit 255; fi
if [ $master = 1 ]; then : > "$ctl"; exec sleep 30; fi
if [ "$op" = "check" ]; then [ -e "$ctl" ] && exit 0 || exit 255; fi
if [ "$op" = "exit" ]; then rm -f "$ctl"; exit 0; fi
exec sh -c "$cmd"
"#,
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[tokio::test]
    async fn exec_through_fake_ssh() {
        let dir = tempfile::tempdir().unwrap();
        let t = SshTransport::with_ssh_bin("fake-exec", vec!["box".into()], fake_ssh(dir.path()));
        let out = t
            .exec(
                &[
                    "sh".into(),
                    "-c".into(),
                    "printf '%s' \"$0\"".into(),
                    "it's ok".into(),
                ],
                None,
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        assert_eq!(out.stdout_str(), "it's ok");
        assert_eq!(t.label(), "box");
        t.shutdown().await;
        assert!(!t.control_path().exists());
    }

    #[tokio::test]
    async fn falls_back_to_next_route_and_reports_auth() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = fake_ssh(dir.path());
        let t = SshTransport::with_ssh_bin(
            "fake-route",
            vec!["denied".into(), "box".into()],
            ssh.clone(),
        );
        t.ensure().await.unwrap();
        assert_eq!(t.label(), "box");
        t.shutdown().await;

        let t = SshTransport::with_ssh_bin("fake-auth", vec!["denied".into()], ssh);
        let err = t.ensure().await.unwrap_err();
        assert!(matches!(err, TransportError::AuthNeeded(_)), "{err:?}");
    }
}
