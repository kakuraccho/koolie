//! Finding and reading transcript files on a host.
//!
//! On this machine the files are read with `std::fs`. On any other host
//! everything goes through `Transport::exec` and a small POSIX `sh` script;
//! values are passed as positional arguments and never spliced into the
//! script text.

use super::{Harness, TranscriptError};
use crate::model::{AgentSession, SessionRefKind};
use crate::transport::Transport;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Most bytes one [`read_from`] call returns. A caller reads again from the
/// new offset while a call returns this many.
pub const READ_CAP: u64 = 16 * 1024 * 1024;

const LOCATE_TIMEOUT: Duration = Duration::from_secs(15);
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Exit code the scripts use for "no such file".
const EXIT_MISSING: i32 = 44;
/// Exit code of the read script for "the file is shorter than the offset".
const EXIT_SHRUNK: i32 = 45;

/// `$1` path. Prints the path when it is a file; a leading `~/` is the home
/// directory.
const PATH_SCRIPT: &str = r#"
p=$1
case $p in
  "~/"*) p=$HOME/${p#"~/"} ;;
esac
[ -f "$p" ] || exit 44
printf '%s\n' "$p"
"#;

/// `$1` session id, `$2` config directory override (may be empty). Prints
/// the newest `projects/*/<id>.jsonl`.
const CLAUDE_SCRIPT: &str = r#"
id=$1
root=${2:-${CLAUDE_CONFIG_DIR:-$HOME/.claude}}
best=
for f in "$root"/projects/*/"$id".jsonl; do
  [ -f "$f" ] || continue
  if [ -z "$best" ] || [ "$f" -nt "$best" ]; then best=$f; fi
done
[ -n "$best" ] || exit 44
printf '%s\n' "$best"
"#;

/// `$1` thread id, `$2` Codex home override (may be empty). Prints the
/// rollout of that thread whose file name sorts last: names start with the
/// time the file was begun, so that is the newest one.
const CODEX_SCRIPT: &str = r#"
LC_ALL=C
export LC_ALL
id=$1
root=${2:-${CODEX_HOME:-$HOME/.codex}}
best=
bestname=
for f in "$root"/sessions/*/*/*/rollout-*-"$id".jsonl \
         "$root"/sessions/*/*/*/rollout-*-"$id"_*.jsonl \
         "$root"/archived_sessions/rollout-*-"$id".jsonl \
         "$root"/archived_sessions/rollout-*-"$id"_*.jsonl; do
  [ -f "$f" ] || continue
  n=${f##*/}
  if [ -z "$best" ] || expr "x$n" \> "x$bestname" >/dev/null; then
    best=$f
    bestname=$n
  fi
done
[ -n "$best" ] || exit 44
printf '%s\n' "$best"
"#;

/// `$1` path, `$2` offset, `$3` offset + 1, `$4` byte cap.
const READ_SCRIPT: &str = r#"
[ -f "$1" ] || exit 44
size=$(wc -c < "$1") || exit 1
[ "$size" -ge "$2" ] || exit 45
tail -c +"$3" -- "$1" | head -c "$4"
"#;

/// `$1` path. Prints the file's size in bytes.
const LEN_SCRIPT: &str = r#"
[ -f "$1" ] || exit 44
wc -c < "$1"
"#;

/// Find the transcript file for a session on the host.
///
/// A `Path` session is the file itself. An `Id` session is looked up where
/// the program keeps its transcripts: Claude Code in
/// `<config>/projects/*/<id>.jsonl` (config is `$CLAUDE_CONFIG_DIR` or
/// `~/.claude`), Codex in `rollout-*-<id>.jsonl` under
/// `$CODEX_HOME/sessions` (or `~/.codex/sessions`). On a remote host the
/// variables are those of a non-interactive shell there.
///
/// Returns `NotFound` while the file does not exist: both programs write it
/// with the first prompt, not when the session starts.
pub async fn locate(
    transport: &dyn Transport,
    session: &AgentSession,
) -> Result<String, TranscriptError> {
    locate_in(transport, session, !transport.is_local(), None).await
}

/// [`locate`] with the route chosen by the caller: `use_exec` runs the
/// script even on this machine, and `root` replaces the program's
/// configuration directory.
pub(super) async fn locate_in(
    transport: &dyn Transport,
    session: &AgentSession,
    use_exec: bool,
    root: Option<&str>,
) -> Result<String, TranscriptError> {
    let harness = Harness::from_agent(&session.agent).ok_or(TranscriptError::UnsupportedAgent)?;
    let value = session.value.trim();
    if value.is_empty() {
        return Err(TranscriptError::NoSession);
    }
    if session.kind == SessionRefKind::Id && !is_safe_id(value) {
        // Not something either program names a file after.
        return Err(TranscriptError::NotFound);
    }
    if use_exec {
        let (script, timeout) = match (&session.kind, harness) {
            (SessionRefKind::Path, _) => (PATH_SCRIPT, LOCATE_TIMEOUT),
            (SessionRefKind::Id, Harness::Claude) => (CLAUDE_SCRIPT, LOCATE_TIMEOUT),
            (SessionRefKind::Id, Harness::Codex) => (CODEX_SCRIPT, LOCATE_TIMEOUT),
        };
        let out = run_script(transport, script, &[value, root.unwrap_or("")], timeout).await?;
        let path = String::from_utf8_lossy(&out);
        let path = path.trim_end_matches(['\n', '\r']);
        return if path.is_empty() {
            Err(TranscriptError::NotFound)
        } else {
            Ok(path.to_string())
        };
    }
    let value = value.to_string();
    let kind = session.kind.clone();
    let root = root.map(PathBuf::from);
    blocking(move || locate_local(harness, &kind, &value, root)).await
}

/// Ids become part of a file name and a glob: keep to what session ids are
/// made of.
fn is_safe_id(id: &str) -> bool {
    id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// `$var` when set and not empty, else `~/<fallback>`.
fn config_root(var: &str, fallback: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|h| h.join(fallback)))
}

fn locate_local(
    harness: Harness,
    kind: &SessionRefKind,
    value: &str,
    root: Option<PathBuf>,
) -> Result<String, TranscriptError> {
    let found = match (kind, harness) {
        (SessionRefKind::Path, _) => {
            let path = match (value.strip_prefix("~/"), home_dir()) {
                (Some(rest), Some(home)) => home.join(rest),
                _ => PathBuf::from(value),
            };
            path.is_file().then_some(path)
        }
        (SessionRefKind::Id, Harness::Claude) => root
            .or_else(|| config_root("CLAUDE_CONFIG_DIR", ".claude"))
            .and_then(|root| claude_file(&root, value)),
        (SessionRefKind::Id, Harness::Codex) => root
            .or_else(|| config_root("CODEX_HOME", ".codex"))
            .and_then(|root| codex_file(&root, value)),
    };
    found
        .map(|p| p.to_string_lossy().into_owned())
        .ok_or(TranscriptError::NotFound)
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect()
}

/// The newest `projects/*/<id>.jsonl`. `/cd` moves a session's file to
/// another project directory, so more than one can exist for a while.
fn claude_file(root: &Path, id: &str) -> Option<PathBuf> {
    let name = format!("{id}.jsonl");
    subdirs(&root.join("projects"))
        .into_iter()
        .map(|dir| dir.join(&name))
        .filter(|p| p.is_file())
        .max_by_key(|p| p.metadata().and_then(|m| m.modified()).ok())
}

/// The rollout of thread `id` whose name sorts last. An undo starts a new
/// file `rollout-<time>-<id>_<rollout>.jsonl`, and the thread goes on there.
fn codex_file(root: &Path, id: &str) -> Option<PathBuf> {
    let plain = format!("-{id}.jsonl");
    let sibling = format!("-{id}_");
    let is_rollout = |path: &Path| {
        path.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|name| {
                name.starts_with("rollout-")
                    && name.ends_with(".jsonl")
                    && (name.ends_with(&plain) || name.contains(&sibling))
            })
    };
    let mut dirs = vec![root.join("archived_sessions")];
    for year in subdirs(&root.join("sessions")) {
        for month in subdirs(&year) {
            dirs.extend(subdirs(&month));
        }
    }
    let mut best: Option<PathBuf> = None;
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for path in entries.filter_map(Result::ok).map(|e| e.path()) {
            if !is_rollout(&path) || !path.is_file() {
                continue;
            }
            if best
                .as_ref()
                .is_none_or(|b| path.file_name() > b.file_name())
            {
                best = Some(path);
            }
        }
    }
    best
}

/// Read the transcript from byte `offset` to its end, at most [`READ_CAP`]
/// bytes per call.
///
/// The usual loop keeps an offset, feeds what comes back to the parser and
/// adds its length to the offset; a call that returns `READ_CAP` bytes has
/// more to give. The file can also become *shorter* (Claude Code rewrites it
/// when it removes a record): the call then returns nothing, like a file
/// with nothing new. To tell the two apart, compare [`file_len`] with the
/// offset; when the file is shorter, call `TranscriptParser::reset`, set the
/// offset to 0 and read again. `Follower` does all of this.
pub async fn read_from(
    transport: &dyn Transport,
    path: &str,
    offset: u64,
) -> Result<Vec<u8>, TranscriptError> {
    match read_chunk(transport, path, offset, !transport.is_local()).await? {
        Chunk::Data(bytes) => Ok(bytes),
        Chunk::Shrunk => Ok(Vec::new()),
    }
}

/// What one read of a transcript gave.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Chunk {
    /// The bytes from the offset on; empty when nothing was appended.
    Data(Vec<u8>),
    /// The file is shorter than the offset: it was rewritten.
    Shrunk,
}

pub(super) async fn read_chunk(
    transport: &dyn Transport,
    path: &str,
    offset: u64,
    use_exec: bool,
) -> Result<Chunk, TranscriptError> {
    if !use_exec {
        let path = PathBuf::from(path);
        return blocking(move || read_local(&path, offset)).await;
    }
    let args = [
        path,
        &offset.to_string(),
        &offset.saturating_add(1).to_string(),
        &READ_CAP.to_string(),
    ];
    match run_script(transport, READ_SCRIPT, &args, READ_TIMEOUT).await {
        Ok(bytes) => Ok(Chunk::Data(bytes)),
        Err(ScriptError::Exit(EXIT_SHRUNK, _)) => Ok(Chunk::Shrunk),
        Err(e) => Err(e.into()),
    }
}

fn read_local(path: &Path, offset: u64) -> Result<Chunk, TranscriptError> {
    let mut file = std::fs::File::open(path).map_err(io_error)?;
    let len = file.metadata().map_err(io_error)?.len();
    if len < offset {
        return Ok(Chunk::Shrunk);
    }
    file.seek(SeekFrom::Start(offset)).map_err(io_error)?;
    let mut bytes = Vec::new();
    file.take(READ_CAP)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    Ok(Chunk::Data(bytes))
}

/// Size of the transcript in bytes; see [`read_from`] for its use.
pub async fn file_len(transport: &dyn Transport, path: &str) -> Result<u64, TranscriptError> {
    file_len_in(transport, path, !transport.is_local()).await
}

pub(super) async fn file_len_in(
    transport: &dyn Transport,
    path: &str,
    use_exec: bool,
) -> Result<u64, TranscriptError> {
    if !use_exec {
        let path = PathBuf::from(path);
        return blocking(move || std::fs::metadata(&path).map(|m| m.len()).map_err(io_error)).await;
    }
    let out = run_script(transport, LEN_SCRIPT, &[path], LOCATE_TIMEOUT).await?;
    let text = String::from_utf8_lossy(&out);
    text.trim().parse::<u64>().map_err(|_| {
        TranscriptError::Io(format!("unexpected size from the host: {:?}", text.trim()))
    })
}

fn io_error(e: std::io::Error) -> TranscriptError {
    match e.kind() {
        std::io::ErrorKind::NotFound => TranscriptError::NotFound,
        _ => TranscriptError::Io(e.to_string()),
    }
}

/// Run file work off the async threads.
async fn blocking<T, F>(work: F) -> Result<T, TranscriptError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, TranscriptError> + Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| TranscriptError::Io(e.to_string()))?
}

/// How a script on the host failed.
#[derive(Debug)]
enum ScriptError {
    /// The host could not be reached or the command not run.
    Transport(String),
    /// The script ran and exited with this code and stderr.
    Exit(i32, String),
}

impl From<ScriptError> for TranscriptError {
    fn from(e: ScriptError) -> Self {
        match e {
            ScriptError::Exit(EXIT_MISSING, _) => TranscriptError::NotFound,
            ScriptError::Exit(code, stderr) => {
                let stderr = stderr.trim();
                TranscriptError::Io(if stderr.is_empty() {
                    format!("reading the transcript failed on the host (exit {code})")
                } else {
                    stderr.to_string()
                })
            }
            ScriptError::Transport(message) => TranscriptError::Io(message),
        }
    }
}

/// Run `script` with `sh` on the host, `args` as `$1`, `$2`, ... Returns its
/// stdout when it exits 0.
async fn run_script(
    transport: &dyn Transport,
    script: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<Vec<u8>, ScriptError> {
    let mut argv: Vec<String> = vec!["sh".into(), "-c".into(), script.into(), "sh".into()];
    argv.extend(args.iter().map(|a| (*a).to_string()));
    let out = transport
        .exec(&argv, None, timeout)
        .await
        .map_err(|e| ScriptError::Transport(e.to_string()))?;
    match out.code {
        Some(0) => Ok(out.stdout),
        // Killed by a signal.
        code => Err(ScriptError::Exit(code.unwrap_or(-1), out.stderr_str())),
    }
}
