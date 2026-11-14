//! herdr-core (`hc`) backend: the headless terminal runtime under the Herdr
//! desktop app.
//!
//! Protocol: JSON-RPC 2.0, one message per line ("plain mode"), on the core's
//! Unix socket. Reference: herdr-core `schema/README.md` and
//! `schema/hc.openrpc.json` (written against 0.1.21, protocol generation 1).
//! One request per connection, plus one long-lived connection for events.
//!
//! Decisions (see also [`project`] for how terminals become agents):
//!
//! * **Never start a core.** Everything goes through the socket; no `hc`
//!   subcommand is run, because every one of them (`connect` included) starts
//!   a core when none listens (Heeler ADR 0031). A missing socket reads as
//!   "not running".
//! * **Plain mode only.** `terminal.attach` needs framed mode, so attach is
//!   [`Backend::attach_argv`]: `herdr-core attach <t>` run interactively on
//!   the host (detach with ctrl+\).
//! * **`hello`** carries only `generation`; never `core`, which would register
//!   koolie as a peer core, and never `framed`.
//! * **`prompt`** is `terminal.send`: the core pastes the text and presses
//!   Enter itself, after the person at the terminal has been quiet for ~3 s
//!   (up to 60 s while they have an unsent draft), and answers once it saw
//!   whether a turn started. A blocked agent refuses it (`blocked`).
//! * **`insert_text`** has no API: the core has no "type without Enter". It is
//!   done with `terminal.keys`, one key per character, which only reaches
//!   ASCII letters, digits, space and a few punctuation marks; anything else
//!   is [`BackendError::Unsupported`] before a key is pressed. The capability
//!   is therefore reported as off.
//! * **`read`**: `terminal.read` returns plain text only (never ANSI) and has
//!   no unwrapped form. `Visible` reads the screen; `Recent` adds history.
//!   Note that the core reads the history of a full-screen (alt screen)
//!   program *by scrolling it* unless it is working, blocked or errored, so
//!   periodic previews should use `Visible`.
//! * **Events**: `missed` (the subscriber fell behind) arrives with a fresh
//!   snapshot and the stream goes on, so it maps to [`BackendEvent::Changed`],
//!   not to [`BackendError::EventsLost`]. A connection that stays silent
//!   through several heartbeats ends with a timeout error.

mod project;

use super::jsonl::LineConn;
use super::{
    Backend, BackendError, BackendEvent, Capabilities, ReadSource, Result, Screen, ServerInfo,
};
use crate::model::{HostId, Inventory, ServerKind};
use crate::transport::{Transport, TransportError};
use async_trait::async_trait;
use project::Terminals;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::sync::mpsc;

/// The protocol generation koolie speaks.
pub const GENERATION: u64 = 1;

/// `hello` features plain mode needs. `framed` and `attach` are not: attach
/// goes through the CLI.
pub const REQUIRED_FEATURES: &[&str] = &["events", "status"];

/// Methods this backend calls.
pub const REQUIRED_METHODS: &[&str] = &[
    "terminal.list",
    "terminal.read",
    "terminal.send",
    "terminal.keys",
    "events.subscribe",
];

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// `terminal.send` waits for the person's draft (60 s by default), the
/// program to settle (5 s) and the turn to start (5 s) before it answers.
const SEND_TIMEOUT: Duration = Duration::from_secs(90);
/// `terminal.keys` waits ~3 s of quiet, then 30 ms per key.
const KEYS_TIMEOUT: Duration = Duration::from_secs(30);
/// The core sends a heartbeat every 15 s; this many silent seconds mean the
/// connection is dead.
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(60);
/// History lines for `Recent` when the caller names none.
const DEFAULT_HISTORY: u32 = 200;

/// (terminal, source) -> (hash of the last text read, its revision).
type ReadCache = HashMap<(String, &'static str), (u64, u64)>;

#[derive(Debug)]
pub struct HerdrCore {
    host: HostId,
    transport: Arc<dyn Transport>,
    /// Socket path on the host.
    socket: String,
    /// `herdr-core` executable on the host (for attach), never bare `hc`.
    core_bin: String,
    seq: AtomicU64,
    /// Version from the last `hello`, for the inventory.
    version: Mutex<Option<String>>,
    /// Per terminal and source: hash of the last text read and its revision.
    reads: Mutex<ReadCache>,
    heartbeat_timeout: Duration,
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
            seq: AtomicU64::new(1),
            version: Mutex::new(None),
            reads: Mutex::new(HashMap::new()),
            heartbeat_timeout: HEARTBEAT_TIMEOUT,
        }
    }

    pub fn socket(&self) -> &str {
        &self.socket
    }

    fn next_id(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    fn closed(&self) -> BackendError {
        // The socket accepted but closed at once: over SSH this is what a
        // missing remote socket looks like.
        TransportError::NotRunning(format!("{}: connection closed", self.socket)).into()
    }

    /// Connect and send one request; returns the connection and the request id.
    async fn request(&self, method: &str, params: Value) -> Result<(LineConn<UnixStream>, u64)> {
        let stream = self.transport.connect(&self.socket).await?;
        let mut conn = LineConn::new(stream);
        let id = self.next_id();
        conn.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await?;
        Ok((conn, id))
    }

    /// One request on a fresh connection.
    pub async fn call(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let (mut conn, id) = self.request(method, params).await?;
        let reply = tokio::time::timeout(timeout, recv_reply(&mut conn, id))
            .await
            .map_err(|_| TransportError::Timeout(format!("{method}: no answer")))??
            .ok_or_else(|| self.closed())?;
        parse_reply(method, &reply)
    }

    /// A revision for text read from a terminal: herdr-core has none, so it
    /// counts the times the text of one terminal and source changed.
    fn read_revision(&self, id: &str, source: ReadSource, text: &str) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut hasher);
        let hash = hasher.finish();
        let mut reads = self.reads.lock().unwrap_or_else(|p| p.into_inner());
        let entry = reads
            .entry((id.to_string(), source.as_api()))
            .or_insert((hash, 1));
        if entry.0 != hash {
            *entry = (hash, entry.1 + 1);
        }
        entry.1
    }
}

/// The answer to request `id`, skipping notifications; `None` at end of stream.
async fn recv_reply(conn: &mut LineConn<UnixStream>, id: u64) -> Result<Option<Value>> {
    loop {
        let Some(line) = conn.recv().await? else {
            return Ok(None);
        };
        let ours = line.get("id").and_then(Value::as_u64) == Some(id);
        // An error without an id answers a request the core could not parse.
        let unaddressed = line.get("method").is_none() && line.get("error").is_some();
        if ours || unaddressed {
            return Ok(Some(line));
        }
    }
}

/// A JSON-RPC error object as a backend error. Every domain error shares one
/// code, so errors are told apart by `data.kind`.
pub fn rpc_error(method: &str, err: &Value) -> BackendError {
    let code = err.get("code").and_then(Value::as_i64);
    let kind = err
        .pointer("/data/kind")
        .and_then(Value::as_str)
        .filter(|k| !k.is_empty());
    let text = |pointer: &str| {
        err.pointer(pointer)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    };
    let message = text("/message")
        .or_else(|| text("/data/hint"))
        .unwrap_or("")
        .to_string();
    if code == Some(-32601) || kind == Some("unknown_method") {
        return BackendError::Unsupported(method.to_string());
    }
    if kind == Some("unsupported_generation") {
        return BackendError::Unsupported(format!("herdr-core protocol: {message}"));
    }
    BackendError::Server {
        code: kind
            .map(str::to_string)
            .or_else(|| code.map(|c| c.to_string()))
            .unwrap_or_else(|| "error".to_string()),
        message,
    }
}

/// Split a reply into result or error.
pub fn parse_reply(method: &str, reply: &Value) -> Result<Value> {
    if let Some(err) = reply.get("error").filter(|e| !e.is_null()) {
        return Err(rpc_error(method, err));
    }
    reply.get("result").cloned().ok_or_else(|| {
        BackendError::Protocol(format!("{method}: reply has neither result nor error"))
    })
}

/// Check a `hello` result against what plain mode needs.
pub fn parse_hello(result: &Value) -> Result<ServerInfo> {
    let version = result
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();
    let generation = result.get("generation").and_then(Value::as_u64);
    if generation != Some(GENERATION) {
        return Err(BackendError::Unsupported(format!(
            "herdr-core {version} speaks protocol generation {}, koolie speaks {GENERATION}",
            generation.map_or("?".to_string(), |g| g.to_string()),
        )));
    }
    let names = |key: &str| -> Vec<&str> {
        result
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect()
    };
    let features = names("features");
    if let Some(missing) = REQUIRED_FEATURES.iter().find(|f| !features.contains(f)) {
        return Err(BackendError::Unsupported(format!(
            "herdr-core {version} lacks the `{missing}` feature; update herdr-core on that machine"
        )));
    }
    let methods = names("methods");
    if !methods.is_empty()
        && let Some(missing) = REQUIRED_METHODS.iter().find(|m| !methods.contains(m))
    {
        return Err(BackendError::Unsupported(format!(
            "herdr-core {version} lacks `{missing}`; update herdr-core on that machine"
        )));
    }
    Ok(ServerInfo {
        version,
        protocol: generation,
    })
}

/// Classify one line of an event subscription; `None` for lines that change
/// nothing (heartbeats, events about sizes, documents or other machines).
fn parse_event_line(
    terminals: &mut Terminals,
    line: &Value,
) -> Option<std::result::Result<BackendEvent, BackendError>> {
    if let Some(err) = line.get("error").filter(|e| !e.is_null()) {
        return Some(Err(rpc_error("events.subscribe", err)));
    }
    let params = line.get("params").unwrap_or(&Value::Null);
    match line.get("method").and_then(Value::as_str) {
        Some("event") => terminals.apply(params).map(Ok),
        Some("missed") => Some(Ok(terminals.missed(params))),
        _ => None,
    }
}

/// A herdr-style key name (`enter`, `esc`, `up`, `ctrl+c`, `1`) in
/// herdr-core's vocabulary. The two mostly agree; this folds case, aliases
/// (`escape`, `return`, `pgup`, `C-c`, `^c`, `ctrl-c`) and capital letters
/// (`A` is `shift+a`). The core checks every name before it presses any.
pub fn translate_key(key: &str) -> String {
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return match c {
            'A'..='Z' => format!("shift+{}", c.to_ascii_lowercase()),
            ' ' => "space".into(),
            '\t' => "tab".into(),
            '\r' | '\n' => "enter".into(),
            '\x1b' => "esc".into(),
            '\x7f' | '\x08' => "backspace".into(),
            c => c.to_string(),
        };
    }
    let key = key.trim();
    // tmux/emacs forms, where the capital letter is the modifier.
    if let Some(rest) = key.strip_prefix('^')
        && rest.chars().count() == 1
    {
        return format!("ctrl+{}", rest.to_ascii_lowercase());
    }
    for (prefix, modifier) in [("C-", "ctrl"), ("M-", "alt"), ("S-", "shift")] {
        if let Some(rest) = key.strip_prefix(prefix)
            && !rest.is_empty()
        {
            let rest = translate_key(rest);
            return format!("{modifier}+{rest}");
        }
    }
    let lower = key.to_ascii_lowercase();
    let mut parts: Vec<&str> = lower.split('+').collect();
    if parts.len() == 1 {
        // `ctrl-c`, `shift-tab`: dashes join only when modifiers lead.
        let dashed: Vec<&str> = lower.split('-').collect();
        if dashed.len() > 1
            && dashed[..dashed.len() - 1]
                .iter()
                .all(|m| modifier_name(m).is_some())
            && dashed.last().is_some_and(|k| !k.is_empty())
        {
            parts = dashed;
        }
    }
    let Some((name, modifiers)) = parts.split_last() else {
        return lower;
    };
    let mut out: Vec<&str> = modifiers
        .iter()
        .map(|m| modifier_name(m).unwrap_or(m))
        .collect();
    let name = match *name {
        "escape" => "esc",
        "return" | "ret" | "cr" => "enter",
        "bs" | "bksp" => "backspace",
        "del" => "delete",
        "ins" => "insert",
        "spc" => "space",
        "pgup" | "page_up" | "page-up" | "prior" => "pageup",
        "pgdn" | "pgdown" | "page_down" | "page-down" | "next" => "pagedown",
        "arrowup" | "uparrow" => "up",
        "arrowdown" | "downarrow" => "down",
        "arrowleft" | "leftarrow" => "left",
        "arrowright" | "rightarrow" => "right",
        "backtab" | "btab" => {
            out.push("shift");
            "tab"
        }
        other => other,
    };
    out.push(name);
    out.join("+")
}

fn modifier_name(word: &str) -> Option<&'static str> {
    match word {
        "ctrl" | "control" | "ctl" => Some("ctrl"),
        "alt" | "meta" | "opt" | "option" => Some("alt"),
        "shift" => Some("shift"),
        "super" | "cmd" | "command" | "win" => Some("super"),
        _ => None,
    }
}

/// The `terminal.keys` names that type `text`, or the first character
/// herdr-core's keys cannot type.
pub fn text_as_keys(text: &str) -> std::result::Result<Vec<String>, char> {
    text.chars()
        .map(|c| match c {
            'a'..='z' | '0'..='9' => Ok(c.to_string()),
            'A'..='Z' => Ok(format!("shift+{}", c.to_ascii_lowercase())),
            ' ' => Ok("space".to_string()),
            '\t' => Ok("tab".to_string()),
            '`' | '\\' | '[' | ']' | ',' | '=' | '-' | '.' | '\'' | ';' | '/' => Ok(c.to_string()),
            other => Err(other),
        })
        .collect()
}

fn session_name(session: &str) -> &str {
    match session.trim() {
        "" => "default",
        name => name,
    }
}

/// Where a core's `core.lock` is: in its state directory,
/// `<state home>/herdr-core/<session>/core.lock`, where the state home is
/// `$XDG_STATE_HOME` or `~/.local/state` (pass that as `state_home`) and the
/// session is `HC_SESSION`, `default` when blank.
pub fn core_lock_path(state_home: &str, session: &str) -> String {
    let session = session_name(session);
    format!(
        "{}/herdr-core/{session}/core.lock",
        state_home.trim_end_matches('/')
    )
}

/// The socket path a `core.lock` file names.
///
/// A running core holds `core.lock` under a file lock and writes into it the
/// socket path it bound, as raw bytes with no newline (herdr-core
/// `server.rs`, `docs/security.md`, Heeler ADR 0031). The file is at
/// `${XDG_STATE_HOME:-$HOME/.local/state}/herdr-core/<session>/core.lock`
/// (see [`core_lock_path`]); reading it starts nothing (`cat` is enough).
///
/// The file outlives its core, so the path may be stale: nothing listening
/// there means the core is not running. Returns `None` for an empty file or
/// one that does not hold an absolute Unix path (a Windows core writes a
/// named pipe, which koolie cannot reach).
pub fn socket_from_core_lock(contents: &str) -> Option<String> {
    let path = contents
        .lines()
        .next()?
        .trim_matches(|c: char| c.is_whitespace() || c == '\0');
    (path.starts_with('/') && !path.contains('\0')).then(|| path.to_string())
}

/// The session's socket when herdr-core finds no runtime directory: beside
/// its state, `<home>/.local/state/herdr-core/<session>/<session>.sock`.
///
/// For when no `core.lock` exists. herdr-core puts the socket in
/// `$XDG_RUNTIME_DIR/herdr-core/`, else `/run/user/<uid>/herdr-core/` when
/// that directory exists (see [`runtime_core_socket`]), and only then here,
/// so a probe that knows the host's runtime directory should try that first.
/// `HC_SOCKET`, `socket` in the core's config and `XDG_STATE_HOME` move it;
/// only `core.lock` knows about those.
pub fn default_core_socket(home: &str, session: &str) -> String {
    let session = session_name(session);
    format!(
        "{}/.local/state/herdr-core/{session}/{session}.sock",
        home.trim_end_matches('/')
    )
}

/// The session's socket under a runtime directory (`$XDG_RUNTIME_DIR` or
/// `/run/user/<uid>`): `<runtime dir>/herdr-core/<session>.sock`.
pub fn runtime_core_socket(runtime_dir: &str, session: &str) -> String {
    let session = session_name(session);
    format!(
        "{}/herdr-core/{session}.sock",
        runtime_dir.trim_end_matches('/')
    )
}

/// Run by `sh -c` with the socket, the executable and the terminal as `$1`,
/// `$2`, `$3`. `herdr-core attach` starts a core when nothing listens, so it
/// only runs while the socket exists; `HC_SOCKET` makes it reach the core
/// koolie talks to, whatever the login environment says.
const ATTACH_SCRIPT: &str = r#"if [ -S "$1" ]; then HC_SOCKET="$1"; export HC_SOCKET; exec "$2" attach "$3"; else echo "herdr-core is not running (no socket at $1)" >&2; exit 1; fi"#;

#[async_trait]
impl Backend for HerdrCore {
    fn kind(&self) -> ServerKind {
        ServerKind::HerdrCore
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            prompt: true,
            // Only what `terminal.keys` can type; see the module docs.
            insert_text: false,
            // herdr-core does not know an agent's native session.
            sessions: false,
        }
    }

    async fn hello(&self) -> Result<ServerInfo> {
        let r = self
            .call("hello", json!({"generation": GENERATION}), REQUEST_TIMEOUT)
            .await?;
        let info = parse_hello(&r)?;
        *self.version.lock().unwrap_or_else(|p| p.into_inner()) = Some(info.version.clone());
        Ok(info)
    }

    async fn snapshot(&self) -> Result<Inventory> {
        let r = self
            .call("terminal.list", json!({}), REQUEST_TIMEOUT)
            .await?;
        let terminals = r
            .get("terminals")
            .and_then(Value::as_array)
            .ok_or_else(|| BackendError::Protocol("terminal.list: missing terminals".into()))?;
        Ok(Inventory {
            server_version: self
                .version
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone(),
            agents: project::project(&self.host, terminals),
        })
    }

    /// herdr-core's subscription is not per agent: `ids` is not used.
    async fn subscribe(
        &self,
        _ids: &[String],
    ) -> Result<mpsc::Receiver<std::result::Result<BackendEvent, BackendError>>> {
        let (mut conn, id) = self.request("events.subscribe", json!({})).await?;
        let ack = tokio::time::timeout(REQUEST_TIMEOUT, recv_reply(&mut conn, id))
            .await
            .map_err(|_| TransportError::Timeout("events.subscribe: no answer".into()))??
            .ok_or_else(|| self.closed())?;
        let snapshot = parse_reply("events.subscribe", &ack)?;
        // From here on the connection carries only notifications.
        let mut terminals = Terminals::from_snapshot(&snapshot);
        let idle = self.heartbeat_timeout;
        let (tx, rx) = mpsc::channel(256);
        tokio::spawn(async move {
            loop {
                let line = tokio::select! {
                    _ = tx.closed() => return,
                    line = tokio::time::timeout(idle, conn.recv()) => line,
                };
                let item = match line {
                    Err(_) => Some(Err(TransportError::Timeout(format!(
                        "events: no heartbeat for {} s",
                        idle.as_secs()
                    ))
                    .into())),
                    Ok(Err(e)) => Some(Err(e)),
                    Ok(Ok(None)) => return,
                    Ok(Ok(Some(line))) => parse_event_line(&mut terminals, &line),
                };
                let Some(item) = item else { continue };
                let end = item.is_err();
                if tx.send(item).await.is_err() || end {
                    return;
                }
            }
        });
        Ok(rx)
    }

    /// `ansi` is not honored: herdr-core returns plain text. `lines` is the
    /// history asked for above the screen, and the text is cut to its last
    /// `lines` lines.
    async fn read(
        &self,
        id: &str,
        source: ReadSource,
        lines: Option<u32>,
        _ansi: bool,
    ) -> Result<Screen> {
        let history = match source {
            ReadSource::Visible => 0,
            ReadSource::Recent | ReadSource::RecentUnwrapped => lines.unwrap_or(DEFAULT_HISTORY),
        };
        let r = self
            .call(
                "terminal.read",
                json!({"terminal": id, "lines": history}),
                REQUEST_TIMEOUT,
            )
            .await?;
        let text = r
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| BackendError::Protocol("terminal.read: missing text".into()))?;
        let text = match lines {
            Some(n) => last_lines(text, n as usize),
            None => text,
        };
        Ok(Screen {
            text: text.to_string(),
            revision: self.read_revision(id, source, text),
            ansi: false,
            truncated: !r.get("complete").and_then(Value::as_bool).unwrap_or(true),
        })
    }

    async fn prompt(&self, id: &str, text: &str) -> Result<()> {
        // No `force`: a blocked agent's dialog is not answered by a prompt.
        self.call(
            "terminal.send",
            json!({"terminal": id, "text": text}),
            SEND_TIMEOUT,
        )
        .await
        .map(|_| ())
    }

    async fn send_keys(&self, id: &str, keys: &[String]) -> Result<()> {
        if keys.is_empty() {
            return Ok(());
        }
        let keys: Vec<String> = keys.iter().map(|k| translate_key(k)).collect();
        let timeout = KEYS_TIMEOUT + Duration::from_millis(50) * keys.len() as u32;
        self.call(
            "terminal.keys",
            json!({"terminal": id, "keys": keys}),
            timeout,
        )
        .await
        .map(|_| ())
    }

    async fn insert_text(&self, id: &str, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        let keys = text_as_keys(text).map_err(|c| {
            BackendError::Unsupported(format!(
                "insert_text: herdr-core cannot type {c:?} without submitting"
            ))
        })?;
        let timeout = KEYS_TIMEOUT + Duration::from_millis(50) * keys.len() as u32;
        self.call(
            "terminal.keys",
            json!({"terminal": id, "keys": keys}),
            timeout,
        )
        .await
        .map(|_| ())
    }

    fn attach_argv(&self, id: &str) -> Vec<String> {
        vec![
            "sh".into(),
            "-c".into(),
            ATTACH_SCRIPT.into(),
            "sh".into(),
            self.socket.clone(),
            self.core_bin.clone(),
            id.into(),
        ]
    }
}

/// The last `n` lines of `text`.
fn last_lines(text: &str, n: usize) -> &str {
    if n == 0 {
        return "";
    }
    let body = text.strip_suffix('\n').unwrap_or(text);
    match body.rmatch_indices('\n').nth(n - 1) {
        Some((at, _)) => &text[at + 1..],
        None => text,
    }
}

#[cfg(test)]
mod tests;
