//! Newline-delimited JSON over a stream: one JSON value per line.

use super::{BackendError, Result};
use crate::transport::TransportError;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

/// Longest line accepted (a big `pane.read` can be large).
pub const MAX_LINE: usize = 64 * 1024 * 1024;

pub struct LineConn<S> {
    reader: BufReader<tokio::io::ReadHalf<S>>,
    writer: tokio::io::WriteHalf<S>,
    buf: Vec<u8>,
}

impl<S: AsyncRead + AsyncWrite> LineConn<S> {
    pub fn new(stream: S) -> Self {
        let (r, w) = tokio::io::split(stream);
        Self {
            reader: BufReader::with_capacity(64 * 1024, r),
            writer: w,
            buf: Vec::new(),
        }
    }

    pub async fn send(&mut self, value: &Value) -> Result<()> {
        let mut line =
            serde_json::to_vec(value).map_err(|e| BackendError::Protocol(e.to_string()))?;
        line.push(b'\n');
        self.writer
            .write_all(&line)
            .await
            .map_err(|e| BackendError::Transport(TransportError::Io(e.to_string())))?;
        self.writer
            .flush()
            .await
            .map_err(|e| BackendError::Transport(TransportError::Io(e.to_string())))?;
        Ok(())
    }

    /// Next JSON line; `Ok(None)` at a clean end of stream.
    pub async fn recv(&mut self) -> Result<Option<Value>> {
        loop {
            self.buf.clear();
            let n = read_line_limited(&mut self.reader, &mut self.buf).await?;
            if n == 0 {
                return Ok(None);
            }
            let line = trim_newline(&self.buf);
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            return serde_json::from_slice(line)
                .map(Some)
                .map_err(|e| BackendError::Protocol(format!("bad JSON line: {e}")));
        }
    }
}

async fn read_line_limited<R: AsyncBufReadExt + Unpin>(
    r: &mut R,
    out: &mut Vec<u8>,
) -> Result<usize> {
    let mut total = 0;
    loop {
        let available = r
            .fill_buf()
            .await
            .map_err(|e| BackendError::Transport(TransportError::Io(e.to_string())))?;
        if available.is_empty() {
            return Ok(total);
        }
        let (chunk, done) = match available.iter().position(|&b| b == b'\n') {
            Some(i) => (&available[..=i], true),
            None => (available, false),
        };
        out.extend_from_slice(chunk);
        let used = chunk.len();
        r.consume(used);
        total += used;
        if total > MAX_LINE {
            return Err(BackendError::Protocol("line too long".into()));
        }
        if done {
            return Ok(total);
        }
    }
}

fn trim_newline(b: &[u8]) -> &[u8] {
    let b = b.strip_suffix(b"\n").unwrap_or(b);
    b.strip_suffix(b"\r").unwrap_or(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn roundtrip_and_eof() {
        let (a, b) = tokio::io::duplex(1024);
        let mut left = LineConn::new(a);
        let mut right = LineConn::new(b);
        left.send(&json!({"x": "日本語"})).await.unwrap();
        assert_eq!(right.recv().await.unwrap(), Some(json!({"x": "日本語"})));
        drop(left);
        assert_eq!(right.recv().await.unwrap(), None);
    }

    #[tokio::test]
    async fn line_split_across_reads() {
        let (a, b) = tokio::io::duplex(8);
        let mut right = LineConn::new(b);
        let writer = tokio::spawn(async move {
            let mut a = a;
            a.write_all(b"\n{\"long\":\"").await.unwrap();
            a.write_all("abcdefghijklmnopqrstuvwxyz".as_bytes())
                .await
                .unwrap();
            a.write_all(b"\"}\r\n").await.unwrap();
        });
        assert_eq!(
            right.recv().await.unwrap(),
            Some(json!({"long": "abcdefghijklmnopqrstuvwxyz"}))
        );
        writer.await.unwrap();
    }
}
