//! The machine koolie runs on.

use super::{ExecOutput, Transport, TransportError};
use async_trait::async_trait;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::process::Command;

#[derive(Debug, Default, Clone)]
pub struct LocalTransport;

#[async_trait]
impl Transport for LocalTransport {
    fn label(&self) -> String {
        "local".into()
    }

    fn is_local(&self) -> bool {
        true
    }

    async fn ensure(&self) -> Result<(), TransportError> {
        Ok(())
    }

    async fn connect(&self, host_socket: &str) -> Result<UnixStream, TransportError> {
        UnixStream::connect(host_socket)
            .await
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
                    TransportError::NotRunning(format!("{host_socket}: {e}"))
                }
                _ => TransportError::Io(format!("{host_socket}: {e}")),
            })
    }

    async fn exec(
        &self,
        argv: &[String],
        stdin: Option<&[u8]>,
        timeout: Duration,
    ) -> Result<ExecOutput, TransportError> {
        run_command(argv, stdin, timeout).await
    }

    fn interactive_argv(&self, argv: &[String]) -> Vec<String> {
        argv.to_vec()
    }

    async fn shutdown(&self) {}
}

/// Run a local process with optional stdin and a timeout; the child is killed
/// when the timeout expires.
pub async fn run_command(
    argv: &[String],
    stdin: Option<&[u8]>,
    timeout: Duration,
) -> Result<ExecOutput, TransportError> {
    let (prog, args) = argv
        .split_first()
        .ok_or_else(|| TransportError::Io("empty command".into()))?;
    let mut cmd = Command::new(prog);
    cmd.args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd
        .spawn()
        .map_err(|e| TransportError::Io(format!("{prog}: {e}")))?;
    if let Some(input) = stdin {
        let mut pipe = child.stdin.take().expect("stdin piped");
        let input = input.to_vec();
        tokio::spawn(async move {
            let _ = pipe.write_all(&input).await;
            let _ = pipe.shutdown().await;
        });
    }
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(out)) => Ok(ExecOutput {
            code: out.status.code(),
            stdout: out.stdout,
            stderr: out.stderr,
        }),
        Ok(Err(e)) => Err(TransportError::Io(format!("{prog}: {e}"))),
        Err(_) => Err(TransportError::Timeout(format!(
            "{prog} did not finish within {}s",
            timeout.as_secs()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn exec_captures_output_and_stdin() {
        let t = LocalTransport;
        let out = t
            .exec(
                &["sh".into(), "-c".into(), "cat; echo err >&2; exit 3".into()],
                Some(b"hello"),
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        assert_eq!(out.stdout_str(), "hello");
        assert_eq!(out.stderr_str(), "err\n");
        assert_eq!(out.code, Some(3));
    }

    #[tokio::test]
    async fn exec_times_out() {
        let t = LocalTransport;
        let err = t
            .exec(
                &["sleep".into(), "5".into()],
                None,
                Duration::from_millis(100),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, TransportError::Timeout(_)));
    }

    #[tokio::test]
    async fn connect_missing_socket_is_not_running() {
        let t = LocalTransport;
        let err = t
            .connect("/nonexistent/koolie-test.sock")
            .await
            .unwrap_err();
        assert!(matches!(err, TransportError::NotRunning(_)), "{err:?}");
    }
}
