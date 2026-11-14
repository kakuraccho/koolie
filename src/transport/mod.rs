//! Routes to a host: a way to reach its herdr socket, run a command on it, and
//! build an interactive command line for it.
//!
//! UI and backends talk to [`Transport`] only; SSH details stay in [`ssh`].

pub mod local;
pub mod ssh;

use async_trait::async_trait;
use std::time::Duration;
use tokio::net::UnixStream;

pub use local::LocalTransport;
pub use ssh::SshTransport;

/// Result of a command run on a host.
#[derive(Clone, Debug, Default)]
pub struct ExecOutput {
    /// Exit code, `None` when killed by a signal.
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl ExecOutput {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
    pub fn stdout_str(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
    pub fn stderr_str(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

/// Why a host could not be reached. The variants decide whether automatic
/// retry makes sense.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    /// Network level: DNS, refused, timed out, host down. Retryable.
    #[error("unreachable: {0}")]
    Unreachable(String),
    /// SSH needs interactive authentication (password, passphrase, MFA).
    #[error("authentication needed: {0}")]
    AuthNeeded(String),
    /// Host key unknown or changed. Never retried automatically.
    #[error("host key problem: {0}")]
    HostKey(String),
    /// The socket on the host is missing or refuses connections (server not running).
    #[error("server not running: {0}")]
    NotRunning(String),
    #[error("timed out: {0}")]
    Timeout(String),
    #[error("{0}")]
    Io(String),
}

impl TransportError {
    /// Whether automatic retry may fix this without the user acting.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            TransportError::Unreachable(_)
                | TransportError::Timeout(_)
                | TransportError::Io(_)
                | TransportError::NotRunning(_)
        )
    }
}

impl From<std::io::Error> for TransportError {
    fn from(e: std::io::Error) -> Self {
        TransportError::Io(e.to_string())
    }
}

#[async_trait]
pub trait Transport: Send + Sync + std::fmt::Debug {
    /// Human label, e.g. `local` or the SSH target in use.
    fn label(&self) -> String;

    /// True when commands run on this machine.
    fn is_local(&self) -> bool;

    /// Bring the route up (start the SSH master, ...). Idempotent and cheap
    /// when already up.
    async fn ensure(&self) -> Result<(), TransportError>;

    /// Open a stream to a Unix socket path *on the host*.
    async fn connect(&self, host_socket: &str) -> Result<UnixStream, TransportError>;

    /// Run a command on the host. `argv[0]` is the program; arguments are
    /// passed verbatim (quoted for the remote shell where needed).
    async fn exec(
        &self,
        argv: &[String],
        stdin: Option<&[u8]>,
        timeout: Duration,
    ) -> Result<ExecOutput, TransportError>;

    /// The local argv that runs `argv` on the host with a TTY, for handing the
    /// terminal over (attach).
    fn interactive_argv(&self, argv: &[String]) -> Vec<String>;

    /// Tear the route down (stop the SSH master, remove forwarded sockets).
    async fn shutdown(&self);
}

/// Quote one word for a POSIX shell.
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./:=@%+,".contains(&b))
    {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Join argv into one shell command line.
pub fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|a| shell_quote(a))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Directory for koolie's runtime files (sockets, status file).
pub fn runtime_dir() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| {
            // SAFETY: getuid has no preconditions and cannot fail.
            let uid = unsafe { libc::getuid() };
            std::env::temp_dir().join(format!("koolie-{uid}"))
        });
    base.join("koolie")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(shell_quote("abc"), "abc");
        assert_eq!(shell_quote("/a/b.sock"), "/a/b.sock");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote("$HOME"), "'$HOME'");
        assert_eq!(
            shell_join(&[
                "herdr".into(),
                "agent".into(),
                "read".into(),
                "w1:p1".into()
            ]),
            "herdr agent read w1:p1"
        );
    }

    #[tokio::test]
    async fn quoting_roundtrips_through_sh() {
        let tricky = vec![
            "plain".to_string(),
            "with space".into(),
            "quote'in".into(),
            "$(echo no)".into(),
            "日本語".into(),
            "".into(),
        ];
        let script = format!("printf '%s\\n' {}", shell_join(&tricky));
        let out = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&script)
            .output()
            .await
            .unwrap();
        let got: Vec<String> = String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect();
        assert_eq!(got, tricky);
    }
}
