//! Tests against a fake herdr-core speaking plain-mode JSON-RPC. Payloads
//! follow `schema/hc.openrpc.json` 0.1.21 and the server's own serialization;
//! no real core was available when this was written.

use super::*;
use crate::model::AgentStatus;
use crate::transport::LocalTransport;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

const METHODS: &[&str] = &[
    "hello",
    "server.status",
    "terminal.run",
    "terminal.list",
    "terminal.get",
    "terminal.read",
    "terminal.attach",
    "terminal.send",
    "terminal.wait",
    "terminal.keys",
    "terminal.interrupt",
    "events.subscribe",
    "rpc.discover",
];

const FEATURES: &[&str] = &[
    "framed",
    "attach",
    "events",
    "status",
    "shell",
    "resize_notifications",
    "automatic_input",
    "restart_environment",
];

/// A `Terminal` record as the core serializes it.
fn terminal(address: &str, argv: &[&str], cwd: &str) -> Value {
    json!({
        "address": address, "name": null, "labels": {}, "argv": argv, "cwd": cwd,
        "keep": false, "state": "live", "pid": 4100, "exit_code": null, "signal": null,
        "reason": null, "created_ms": 1_794_000_000_000u64, "finished_ms": null,
        "screen": "exact", "cols": 120, "rows": 40, "identity": "ghostty 1.2.0",
        "status": null, "status_ms": null, "turn": 0, "reported": false, "title": "",
        "parent": null, "notify": false, "die_with_parent": false, "rm": false,
        "news": 0, "seen": 0, "noted": 0, "seen_ms": null, "template": null,
    })
}

fn with(mut record: Value, fields: Value) -> Value {
    if let (Some(record), Some(fields)) = (record.as_object_mut(), fields.as_object()) {
        for (k, v) in fields {
            record.insert(k.clone(), v.clone());
        }
    }
    record
}

fn foreground(name: &str, argv: &[&str], cwd: &str) -> Value {
    json!({"pid": 4100, "name": name, "argv": argv, "cwd": cwd})
}

/// What the fake core holds: a shell, agents in a desktop space, a derived
/// workspace with a subagent, and terminals that must not show as agents.
fn terminals() -> Vec<Value> {
    vec![
        // A plain shell.
        with(
            terminal("t1", &["/bin/zsh"], "/home/u"),
            json!({"keep": true, "title": "~",
                "foreground": foreground("zsh", &["/bin/zsh"], "/home/u")}),
        ),
        // Claude Code started by the desktop app in space s1.
        with(
            terminal(
                "t2",
                &["/bin/zsh", "-ic", "exec \"$@\"", "/bin/zsh", "claude"],
                "/home/u/dev/app",
            ),
            json!({"keep": true, "labels": {"herdr.space": "s1", "herdr.tab": "tab1"},
                "created_ms": 1_794_000_001_000u64, "reported": true, "turn": 4,
                "status": {"state": "working", "progress": 40, "app": "claude-code", "title": "Fix the build"},
                "status_ms": 1_794_000_090_000u64, "title": "✳ Fix the build",
                "foreground": foreground("2.1.286", &["claude"], "/home/u/dev/app/crates")}),
        ),
        // Codex in the same space, blocked on a permission dialog.
        with(
            terminal("t3", &["codex", "--no-daemon"], "/home/u/dev/other"),
            json!({"name": "reviewer", "labels": {"herdr.space": "s1"},
                "created_ms": 1_794_000_002_000u64, "reported": true, "turn": 1,
                "status": {"state": "blocked", "kind": "permission", "app": "codex"},
                "status_ms": 1_794_000_080_000u64, "title": "codex",
                "foreground": foreground("codex", &["codex", "--no-daemon"], "/home/u/dev/other")}),
        ),
        // A subagent started from inside t5, in another folder; it errored.
        with(
            terminal("t4", &["claude", "-p", "check the tests"], "/tmp/scratch"),
            json!({"parent": "t5", "notify": true, "created_ms": 1_794_000_004_000u64,
                "reported": true, "status": {"state": "error", "app": "claude-code", "msg": "rate limited"},
                "status_ms": 1_794_000_070_000u64,
                "foreground": foreground("claude", &["claude", "-p", "check the tests"], "/tmp/scratch")}),
        ),
        // OpenCode from a shell, no foreground sample yet, never reported.
        with(
            terminal("t5", &["opencode"], "/home/u/dev/lib"),
            json!({"created_ms": 1_794_000_003_000u64}),
        ),
        // A program koolie does not know that speaks 7501 without an app.
        with(
            terminal("t6", &["myagent", "--serve"], "/srv/work"),
            json!({"created_ms": 1_794_000_005_000u64, "reported": true, "turn": 2,
                "status": {"state": "done", "title": "nightly run"},
                "status_ms": 1_794_000_060_000u64,
                "foreground": foreground("myagent", &["myagent", "--serve"], "/srv/work")}),
        ),
        // The agent left; its wrapper shell keeps the last status.
        with(
            terminal(
                "t7",
                &["/bin/zsh", "-ic", "claude; exec zsh"],
                "/home/u/dev/app",
            ),
            json!({"reported": true, "status": {"state": "idle", "app": "claude-code"},
                "status_ms": 1_794_000_050_000u64,
                "foreground": foreground("zsh", &["zsh"], "/home/u/dev/app")}),
        ),
        // Another program took the terminal of an agent koolie knows.
        with(
            terminal("t8", &["bash", "./work.sh"], "/home/u/dev/app"),
            json!({"reported": true, "status": {"state": "idle", "app": "codex"},
                "foreground": foreground("vim", &["vim", "notes.md"], "/home/u/dev/app")}),
        ),
        // A kept terminal that lost its program and waits for a new shell.
        with(
            terminal("t9", &["claude"], "/home/u/dev/app"),
            json!({"keep": true, "pid": null, "reason": "signaled"}),
        ),
        // Finished.
        with(
            terminal("t10", &["claude"], "/home/u/dev/app"),
            json!({"state": "finished", "pid": null, "exit_code": 0, "reason": "exited",
                "finished_ms": 1_794_000_040_000u64}),
        ),
        // On a machine this core reaches.
        with(
            terminal("rohan/t2", &["claude"], "/home/u"),
            json!({"reported": true, "status": {"state": "working", "app": "claude-code"},
                "foreground": foreground("claude", &["claude"], "/home/u")}),
        ),
    ]
}

fn rpc_err(code: i64, kind: &str, message: &str, hint: &str) -> Value {
    json!({"code": code, "message": message, "data": {"kind": kind, "hint": hint}})
}

enum Push {
    Line(Value),
    Close,
}

struct FakeState {
    terminals: Vec<Value>,
    screen: String,
    requests: Vec<Value>,
}

impl FakeState {
    fn find(&self, params: &Value) -> std::result::Result<&Value, Value> {
        let name = params["terminal"].as_str().unwrap_or("");
        self.terminals
            .iter()
            .find(|t| t["address"] == name || t["name"] == name)
            .ok_or_else(|| {
                rpc_err(
                    -32000,
                    "not_found",
                    &format!("no terminal {name}"),
                    "see `hc ls`",
                )
            })
    }

    fn answer(&self, method: &str, params: &Value) -> std::result::Result<Value, Value> {
        match method {
            "hello" => {
                if let Some(g) = params["generation"].as_u64().filter(|g| *g != 1) {
                    return Err(rpc_err(
                        -32000,
                        "unsupported_generation",
                        &format!("protocol generation {g} not supported"),
                        "this core speaks generation 1",
                    ));
                }
                Ok(json!({
                    "generation": 1, "version": "0.1.21",
                    "core": {"id": "5f1c9a3e7b2d4c60", "name": "devbox"},
                    "framed": params["framed"].as_bool().unwrap_or(false),
                    "methods": METHODS, "features": FEATURES,
                    "keeps_running_after_logout": true, "logout_linger_user": null,
                }))
            }
            "terminal.list" => {
                let all = params["all"].as_bool().unwrap_or(false);
                let (live, finished): (Vec<&Value>, Vec<&Value>) =
                    self.terminals.iter().partition(|t| t["state"] == "live");
                let mut terminals = live;
                if all {
                    terminals.extend(finished.iter().copied());
                }
                Ok(json!({
                    "terminals": terminals,
                    "hidden_finished": if all { 0 } else { finished.len() },
                    "machines": [{"name": "rohan", "target": "u@rohan", "id": "aa11", "host": "rohan",
                        "version": "0.1.21", "state": "connected", "session": null, "since_ms": null,
                        "why": null, "temporary": false, "via": false}],
                }))
            }
            "terminal.read" => {
                self.find(params)?;
                let lines = params["lines"].as_u64().unwrap_or(0);
                let mut text: Vec<String> = (1..=lines.min(500))
                    .rev()
                    .map(|i| format!("history {i}"))
                    .collect();
                text.push(self.screen.clone());
                Ok(json!({
                    "text": text.join("\n"),
                    "complete": lines <= 500,
                    "how": if lines == 0 { "screen" } else { "history" },
                    "why": null,
                }))
            }
            "terminal.send" => {
                let t = self.find(params)?;
                params["text"]
                    .as_str()
                    .ok_or_else(|| rpc_err(-32602, "invalid_params", "missing field `text`", ""))?;
                let word = t["status"]["state"].as_str();
                if word == Some("blocked") && params["force"] != true {
                    let address = t["address"].as_str().unwrap_or("");
                    return Err(rpc_err(
                        -32000,
                        "blocked",
                        &format!("{address} is blocked (permission)"),
                        "typing would answer its dialog; read its screen first, or --force",
                    ));
                }
                Ok(
                    json!({"status": word, "turn": t["turn"], "started": word.map(|_| true), "wait": null}),
                )
            }
            "terminal.keys" => {
                let t = self.find(params)?;
                let known = [
                    "enter",
                    "esc",
                    "tab",
                    "space",
                    "up",
                    "down",
                    "backspace",
                    "pageup",
                ];
                for key in params["keys"].as_array().into_iter().flatten() {
                    let key = key.as_str().unwrap_or("");
                    let name = key.rsplit('+').next().unwrap_or("");
                    let single = name.chars().count() == 1
                        && name
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
                    if !single && !known.contains(&name) {
                        return Err(rpc_err(
                            -32602,
                            "invalid_params",
                            &format!(
                                "unknown key {key:?} (try enter, esc, up, ctrl+c, alt+x, f5, a)"
                            ),
                            "see `hc schema`",
                        ));
                    }
                }
                Ok(json!({"status": t["status"]["state"], "turn": t["turn"]}))
            }
            "events.subscribe" => Ok(json!({"seq": 41, "terminals": self.terminals})),
            other => Err(rpc_err(
                -32601,
                "unknown_method",
                &format!("unknown method {other:?}"),
                "see `hc schema`",
            )),
        }
    }
}

/// A herdr-core that listens on a socket in a temp dir.
struct Fake {
    _dir: tempfile::TempDir,
    socket: String,
    state: Arc<Mutex<FakeState>>,
    push: mpsc::UnboundedSender<Push>,
}

impl Fake {
    fn start() -> Fake {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("default.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let state = Arc::new(Mutex::new(FakeState {
            terminals: terminals(),
            screen: "> fix the build\n● Working…".into(),
            requests: Vec::new(),
        }));
        let (push, pushed) = mpsc::unbounded_channel();
        let pushed = Arc::new(tokio::sync::Mutex::new(pushed));
        let shared = state.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve(stream, shared.clone(), pushed.clone()));
            }
        });
        Fake {
            _dir: dir,
            socket: socket.to_string_lossy().into_owned(),
            state,
            push,
        }
    }

    fn backend(&self) -> HerdrCore {
        HerdrCore::new(
            "devbox".into(),
            Arc::new(LocalTransport),
            self.socket.clone(),
            "/home/u/.local/bin/herdr-core".into(),
        )
    }

    /// Requests seen so far, as `(method, params)`.
    fn requests(&self) -> Vec<(String, Value)> {
        let state = self.state.lock().unwrap();
        state
            .requests
            .iter()
            .map(|r| {
                (
                    r["method"].as_str().unwrap_or("").to_string(),
                    r["params"].clone(),
                )
            })
            .collect()
    }

    fn last_request(&self) -> (String, Value) {
        self.requests().pop().expect("a request")
    }

    fn event(&self, seq: u64, kind: &str, terminal: &str, data: Value) {
        let params = json!({"type": kind, "terminal": terminal, "data": data, "seq": seq});
        self.notify("event", params);
    }

    fn notify(&self, method: &str, params: Value) {
        let line = json!({"jsonrpc": "2.0", "method": method, "params": params});
        self.push.send(Push::Line(line)).unwrap();
    }

    fn close_events(&self) {
        self.push.send(Push::Close).unwrap();
    }
}

async fn serve(
    stream: UnixStream,
    state: Arc<Mutex<FakeState>>,
    pushed: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<Push>>>,
) {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let request: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["jsonrpc"], "2.0", "{request}");
        let method = request["method"].as_str().unwrap().to_string();
        let result = {
            let mut state = state.lock().unwrap();
            state.requests.push(request.clone());
            state.answer(&method, &request["params"])
        };
        let subscribed = result.is_ok() && method == "events.subscribe";
        let reply = match result {
            Ok(result) => json!({"jsonrpc": "2.0", "id": request["id"], "result": result}),
            Err(error) => json!({"jsonrpc": "2.0", "id": request["id"], "error": error}),
        };
        if write
            .write_all(format!("{reply}\n").as_bytes())
            .await
            .is_err()
        {
            return;
        }
        if subscribed {
            // Plain mode: from here on, notifications only.
            let mut pushed = pushed.lock().await;
            while let Some(Push::Line(line)) = pushed.recv().await {
                if write
                    .write_all(format!("{line}\n").as_bytes())
                    .await
                    .is_err()
                {
                    return;
                }
            }
            return;
        }
    }
}

#[tokio::test]
async fn hello_reads_version_and_sends_only_the_generation() {
    let fake = Fake::start();
    let info = fake.backend().hello().await.unwrap();
    assert_eq!(info.version, "0.1.21");
    assert_eq!(info.protocol, Some(1));
    let (method, params) = fake.last_request();
    assert_eq!(method, "hello");
    // No `core` (it would register koolie as a peer core) and no `framed`.
    assert_eq!(params, json!({"generation": 1}));
}

#[test]
fn hello_refuses_a_core_plain_mode_cannot_use() {
    let ok = json!({"generation": 1, "version": "0.1.21", "core": {"id": "x", "name": "h"},
        "framed": false, "methods": METHODS, "features": FEATURES, "brand_new": 1});
    assert_eq!(
        parse_hello(&ok).unwrap(),
        ServerInfo {
            version: "0.1.21".into(),
            protocol: Some(1)
        }
    );
    // Framed and attach are not needed: attach goes through the CLI.
    let plain = with(ok.clone(), json!({"features": ["events", "status"]}));
    assert!(parse_hello(&plain).is_ok());
    for bad in [
        with(ok.clone(), json!({"generation": 2})),
        with(
            ok.clone(),
            json!({"features": ["framed", "attach", "status"]}),
        ),
        with(ok.clone(), json!({"methods": ["hello", "terminal.list"]})),
    ] {
        let err = parse_hello(&bad).unwrap_err();
        assert!(matches!(err, BackendError::Unsupported(_)), "{err:?}");
    }
    let err = rpc_error(
        "hello",
        &rpc_err(
            -32000,
            "unsupported_generation",
            "protocol generation 2 not supported",
            "",
        ),
    );
    assert!(matches!(err, BackendError::Unsupported(_)), "{err:?}");
}

#[tokio::test]
async fn snapshot_lists_agents_and_leaves_shells_out() {
    let fake = Fake::start();
    let backend = fake.backend();
    // The version comes from hello; before it, none is known.
    assert_eq!(backend.snapshot().await.unwrap().server_version, None);
    backend.hello().await.unwrap();
    let inv = backend.snapshot().await.unwrap();
    assert_eq!(inv.server_version.as_deref(), Some("0.1.21"));
    assert_eq!(fake.last_request(), ("terminal.list".into(), json!({})));

    let ids: Vec<&str> = inv.agents.iter().map(|a| a.key.id.as_str()).collect();
    // Not the shell (t1), the wrapper shell with a stale status (t7), the
    // program that replaced a known agent (t8), the lost terminal (t9), the
    // finished one (t10) or another machine's (rohan/t2).
    assert_eq!(ids, ["t2", "t3", "t4", "t5", "t6"]);
    let agent = |id: &str| inv.agents.iter().find(|a| a.key.id == id).unwrap();

    let claude = agent("t2");
    assert_eq!(claude.key.host, "devbox");
    assert_eq!(claude.agent.as_deref(), Some("claude"));
    assert_eq!(claude.status, AgentStatus::Working);
    assert_eq!(claude.title.as_deref(), Some("Fix the build"));
    assert_eq!(claude.cwd.as_deref(), Some("/home/u/dev/app/crates"));
    assert_eq!(claude.workspace_id, "space:s1");
    assert_eq!(claude.workspace_label.as_deref(), Some("app"));
    assert_eq!(claude.tab_id.as_deref(), Some("tab1"));
    assert_eq!(claude.revision, 4);
    assert_eq!(claude.state_change_seq, 1_794_000_090_000);
    assert_eq!(claude.session, None);
    assert!(!claude.focused);

    let codex = agent("t3");
    assert_eq!(codex.agent.as_deref(), Some("codex"));
    assert_eq!(codex.status, AgentStatus::Blocked);
    assert_eq!(codex.title.as_deref(), Some("reviewer"));
    // Same space, named after its first terminal's folder; its own tab.
    assert_eq!(codex.workspace_id, "space:s1");
    assert_eq!(codex.workspace_label.as_deref(), Some("app"));
    assert_eq!(codex.tab_id.as_deref(), Some("t3"));

    // The subagent joins the workspace and tab of the terminal that started it.
    let sub = agent("t4");
    assert_eq!(sub.agent.as_deref(), Some("claude"));
    assert_eq!(sub.status, AgentStatus::Blocked, "error needs the user");
    assert_eq!(sub.workspace_id, "dir:/home/u/dev/lib");
    assert_eq!(sub.workspace_label.as_deref(), Some("lib"));
    assert_eq!(sub.tab_id.as_deref(), Some("t5"));
    assert_eq!(sub.cwd.as_deref(), Some("/tmp/scratch"));

    // No sample and no report yet: the launch argv names a known agent.
    let opencode = agent("t5");
    assert_eq!(opencode.agent.as_deref(), Some("opencode"));
    assert_eq!(opencode.status, AgentStatus::Unknown);
    assert_eq!(opencode.title, None);
    assert_eq!(opencode.workspace_id, "dir:/home/u/dev/lib");
    assert_eq!(opencode.tab_id.as_deref(), Some("t5"));
    assert_eq!(opencode.state_change_seq, 1_794_000_003_000);

    // An unknown program is an agent because it reports a status.
    let unknown = agent("t6");
    assert_eq!(unknown.agent.as_deref(), Some("myagent"));
    assert_eq!(unknown.status, AgentStatus::Done);
    assert_eq!(unknown.title.as_deref(), Some("nightly run"));
    assert_eq!(unknown.workspace_id, "dir:/srv/work");
    assert_eq!(unknown.workspace_label.as_deref(), Some("work"));
}

#[tokio::test]
async fn read_maps_sources_lines_and_revision() {
    let fake = Fake::start();
    let backend = fake.backend();

    let screen = backend
        .read("t2", ReadSource::Visible, None, true)
        .await
        .unwrap();
    assert_eq!(
        fake.last_request(),
        (
            "terminal.read".into(),
            json!({"terminal": "t2", "lines": 0})
        )
    );
    assert_eq!(screen.text, "> fix the build\n● Working…");
    assert!(!screen.ansi, "herdr-core returns plain text only");
    assert!(!screen.truncated);
    assert_eq!(screen.revision, 1);

    // The same text again keeps its revision; new text moves it on.
    let again = backend
        .read("t2", ReadSource::Visible, None, false)
        .await
        .unwrap();
    assert_eq!(again.revision, 1);
    fake.state.lock().unwrap().screen = "> fix the build\n● Done".into();
    let changed = backend
        .read("t2", ReadSource::Visible, None, false)
        .await
        .unwrap();
    assert_eq!(changed.text, "> fix the build\n● Done");
    assert_eq!(changed.revision, 2);

    // History is asked for above the screen, then cut to the lines wanted.
    let recent = backend
        .read("t2", ReadSource::Recent, Some(3), false)
        .await
        .unwrap();
    assert_eq!(fake.last_request().1, json!({"terminal": "t2", "lines": 3}));
    assert_eq!(recent.text, "history 1\n> fix the build\n● Done");
    let default = backend
        .read("t2", ReadSource::RecentUnwrapped, None, false)
        .await
        .unwrap();
    assert_eq!(
        fake.last_request().1,
        json!({"terminal": "t2", "lines": 200})
    );
    assert_eq!(default.text.lines().count(), 202);

    // `complete: false` reads as truncated.
    let long = backend
        .read("t2", ReadSource::Recent, Some(600), false)
        .await
        .unwrap();
    assert!(long.truncated);

    let err = backend
        .read("t99", ReadSource::Visible, None, false)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some("not_found"));
    assert_eq!(err.to_string(), "not_found: no terminal t99");
}

#[tokio::test]
async fn prompt_sends_text_and_a_blocked_agent_refuses() {
    let fake = Fake::start();
    let backend = fake.backend();
    backend
        .prompt("t2", "run the tests\nthen commit")
        .await
        .unwrap();
    // `terminal.send` presses Enter itself; nothing is forced.
    assert_eq!(
        fake.last_request(),
        (
            "terminal.send".into(),
            json!({"terminal": "t2", "text": "run the tests\nthen commit"})
        )
    );
    let err = backend.prompt("t3", "hello").await.unwrap_err();
    assert_eq!(
        err,
        BackendError::Server {
            code: "blocked".into(),
            message: "t3 is blocked (permission)".into()
        }
    );
    assert!(!err.is_retryable());
}

#[tokio::test]
async fn send_keys_translates_names() {
    let fake = Fake::start();
    let backend = fake.backend();
    let keys: Vec<String> = ["Enter", "escape", "Up", "ctrl+c", "C-c", "1", "Y", "pgup"]
        .iter()
        .map(|k| k.to_string())
        .collect();
    backend.send_keys("t3", &keys).await.unwrap();
    assert_eq!(
        fake.last_request(),
        (
            "terminal.keys".into(),
            json!({"terminal": "t3", "keys":
                ["enter", "esc", "up", "ctrl+c", "ctrl+c", "1", "shift+y", "pageup"]})
        )
    );
    // The core checks names before it presses any.
    let err = backend
        .send_keys("t3", &["hello".to_string()])
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some("invalid_params"));
    // Nothing to press: no request.
    let before = fake.requests().len();
    backend.send_keys("t3", &[]).await.unwrap();
    assert_eq!(fake.requests().len(), before);
}

#[test]
fn key_names_fold_to_herdr_cores() {
    for (given, want) in [
        ("enter", "enter"),
        ("Return", "enter"),
        ("ESC", "esc"),
        ("down", "down"),
        ("7", "7"),
        ("a", "a"),
        ("A", "shift+a"),
        (" ", "space"),
        ("\t", "tab"),
        ("\r", "enter"),
        ("-", "-"),
        ("Ctrl+C", "ctrl+c"),
        ("ctrl-c", "ctrl+c"),
        ("control+shift+tab", "ctrl+shift+tab"),
        ("shift-tab", "shift+tab"),
        ("backtab", "shift+tab"),
        ("^d", "ctrl+d"),
        ("M-x", "alt+x"),
        ("C-M-x", "ctrl+alt+x"),
        ("cmd+k", "super+k"),
        ("page-down", "pagedown"),
        ("PageUp", "pageup"),
        ("f5", "f5"),
        ("delete", "delete"),
    ] {
        assert_eq!(translate_key(given), want, "{given:?}");
    }
}

#[tokio::test]
async fn insert_text_types_keys_and_refuses_what_keys_cannot_type() {
    let fake = Fake::start();
    let backend = fake.backend();
    assert!(!backend.capabilities().insert_text);
    backend.insert_text("t3", "Yes 2").await.unwrap();
    assert_eq!(
        fake.last_request(),
        (
            "terminal.keys".into(),
            json!({"terminal": "t3", "keys": ["shift+y", "e", "s", "space", "2"]})
        )
    );
    // Nothing is typed when one character cannot be: not even the first.
    let before = fake.requests().len();
    for text in ["ok?", "日本語", "two\nlines"] {
        let err = backend.insert_text("t3", text).await.unwrap_err();
        assert!(matches!(err, BackendError::Unsupported(_)), "{err:?}");
    }
    backend.insert_text("t3", "").await.unwrap();
    assert_eq!(fake.requests().len(), before);
}

#[tokio::test]
async fn events_map_and_the_channel_ends_when_the_core_closes() {
    let fake = Fake::start();
    let backend = fake.backend();
    let mut rx = backend.subscribe(&["t2".to_string()]).await.unwrap();
    // Unfiltered: every terminal of the core, not only the known agents.
    assert_eq!(fake.last_request(), ("events.subscribe".into(), json!({})));

    fake.notify("heartbeat", json!({"seq": 41}));
    // Already in the snapshot.
    fake.event(41, "title", "t2", json!({"title": "old"}));
    // An agent's status.
    fake.event(
        42,
        "status",
        "t2",
        json!({"status": {"state": "done", "app": "claude-code"}, "records": {}, "reported": true}),
    );
    assert_eq!(
        rx.recv().await.unwrap(),
        Ok(BackendEvent::StatusChanged {
            id: "t2".into(),
            status: AgentStatus::Done
        })
    );
    // The turn counter beside it, a resize, a document and another machine's
    // terminal change nothing the Console shows.
    fake.event(43, "turn", "t2", json!({"turn": 5, "state": "done"}));
    fake.event(44, "size", "t2", json!({"cols": 100, "rows": 30}));
    fake.notify(
        "event",
        json!({"seq": 45, "type": "doc", "data": {"name": "herdr-app", "version": 2}}),
    );
    fake.event(
        46,
        "status",
        "rohan/t2",
        json!({"status": {"state": "idle"}, "records": {}, "reported": true}),
    );
    // A shell's status is no agent's status, but may make it one.
    fake.event(
        47,
        "status",
        "t1",
        json!({"status": {"state": "idle", "app": "claude-code"}, "records": {}, "reported": true}),
    );
    assert_eq!(rx.recv().await.unwrap(), Ok(BackendEvent::Changed));
    // Claude Code takes the shell: now its status is an agent's.
    fake.event(
        48,
        "foreground",
        "t1",
        json!({"pid": 4200, "name": "claude", "argv": ["claude"], "cwd": "/home/u"}),
    );
    assert_eq!(rx.recv().await.unwrap(), Ok(BackendEvent::Changed));
    fake.event(
        49,
        "status",
        "t1",
        json!({"status": {"state": "error", "app": "claude-code"}, "records": {}, "reported": true}),
    );
    assert_eq!(
        rx.recv().await.unwrap(),
        Ok(BackendEvent::StatusChanged {
            id: "t1".into(),
            status: AgentStatus::Blocked
        })
    );
    // A new terminal, then its first report.
    let created = with(
        terminal("t11", &["codex"], "/home/u/dev/new"),
        json!({"foreground": foreground("codex", &["codex"], "/home/u/dev/new")}),
    );
    fake.event(50, "created", "t11", created);
    assert_eq!(rx.recv().await.unwrap(), Ok(BackendEvent::Changed));
    fake.event(
        51,
        "status",
        "t11",
        json!({"status": {"state": "working", "app": "codex"}, "records": {}, "reported": true}),
    );
    assert_eq!(
        rx.recv().await.unwrap(),
        Ok(BackendEvent::StatusChanged {
            id: "t11".into(),
            status: AgentStatus::Working
        })
    );
    // The program ends: the status is cleared first, then it finishes.
    fake.event(
        52,
        "status",
        "t11",
        json!({"status": null, "records": {}, "reported": true}),
    );
    assert_eq!(
        rx.recv().await.unwrap(),
        Ok(BackendEvent::StatusChanged {
            id: "t11".into(),
            status: AgentStatus::Unknown
        })
    );
    fake.event(
        53,
        "finished",
        "t11",
        json!({"reason": "exited", "exit_code": 0, "signal": null, "finished_ms": 1_794_000_100_000u64}),
    );
    assert_eq!(rx.recv().await.unwrap(), Ok(BackendEvent::Changed));
    fake.event(54, "removed", "t11", json!({}));
    assert_eq!(rx.recv().await.unwrap(), Ok(BackendEvent::Changed));

    // Fell behind: a fresh snapshot replaces what the stream knew, and the
    // stream goes on. Older events are dropped.
    let mut fresh = terminals();
    fresh.retain(|t| t["address"] != "t2");
    fake.notify("missed", json!({"seq": 60, "terminals": fresh}));
    assert_eq!(rx.recv().await.unwrap(), Ok(BackendEvent::Changed));
    fake.event(59, "title", "t3", json!({"title": "stale"}));
    fake.event(
        61,
        "status",
        "t3",
        json!({"status": {"state": "working", "app": "codex"}, "records": {}, "reported": true}),
    );
    assert_eq!(
        rx.recv().await.unwrap(),
        Ok(BackendEvent::StatusChanged {
            id: "t3".into(),
            status: AgentStatus::Working
        })
    );
    // t2 is no longer known: its status is only a change.
    fake.event(
        62,
        "status",
        "t2",
        json!({"status": {"state": "idle"}, "records": {}, "reported": true}),
    );
    assert_eq!(rx.recv().await.unwrap(), Ok(BackendEvent::Changed));

    fake.close_events();
    assert_eq!(
        rx.recv().await,
        None,
        "the channel ends with the connection"
    );
}

#[tokio::test]
async fn an_error_on_the_event_stream_ends_it() {
    let fake = Fake::start();
    let mut rx = fake.backend().subscribe(&[]).await.unwrap();
    fake.push
        .send(Push::Line(json!({"jsonrpc": "2.0", "id": null,
            "error": rpc_err(-32603, "internal", "no connection to deliver events on", "")})))
        .unwrap();
    assert_eq!(
        rx.recv().await.unwrap(),
        Err(BackendError::Server {
            code: "internal".into(),
            message: "no connection to deliver events on".into()
        })
    );
    assert_eq!(rx.recv().await, None);
}

#[tokio::test]
async fn a_silent_event_stream_times_out() {
    let fake = Fake::start();
    let mut backend = fake.backend();
    backend.heartbeat_timeout = Duration::from_millis(150);
    let mut rx = backend.subscribe(&[]).await.unwrap();
    fake.notify("heartbeat", json!({"seq": 41}));
    let end = rx.recv().await.unwrap().unwrap_err();
    assert!(
        matches!(end, BackendError::Transport(TransportError::Timeout(_))),
        "{end:?}"
    );
    assert!(end.is_retryable());
    assert_eq!(rx.recv().await, None);
}

#[tokio::test]
async fn errors_map_by_kind() {
    let fake = Fake::start();
    let backend = fake.backend();
    // An older core without the method.
    let err = backend
        .call("terminal.rename", json!({}), REQUEST_TIMEOUT)
        .await
        .unwrap_err();
    assert_eq!(err, BackendError::Unsupported("terminal.rename".into()));
    // Domain errors share code -32000 and are told apart by data.kind.
    let err = backend.prompt("nobody", "hi").await.unwrap_err();
    assert_eq!(err.code(), Some("not_found"));
    // Without a kind, the code stands in.
    assert_eq!(
        rpc_error("x", &json!({"code": -32000, "message": "boom"})),
        BackendError::Server {
            code: "-32000".into(),
            message: "boom".into()
        }
    );
    assert_eq!(
        parse_reply("x", &json!({"jsonrpc": "2.0", "id": 1})).unwrap_err(),
        BackendError::Protocol("x: reply has neither result nor error".into())
    );
}

#[tokio::test]
async fn no_core_reads_as_not_running() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("default.sock");
    let backend = HerdrCore::new(
        "devbox".into(),
        Arc::new(LocalTransport),
        socket.to_string_lossy().into_owned(),
        "herdr-core".into(),
    );
    // Nothing listens: nothing is started, the host is not running.
    for err in [
        backend.hello().await.unwrap_err(),
        backend.snapshot().await.unwrap_err(),
        backend.subscribe(&[]).await.unwrap_err(),
    ] {
        assert!(
            matches!(err, BackendError::Transport(TransportError::NotRunning(_))),
            "{err:?}"
        );
    }
    assert!(!socket.exists());

    // Accepted, then closed without an answer (a forwarded socket nobody
    // listens on).
    let listener = UnixListener::bind(&socket).unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let mut lines = BufReader::new(stream).lines();
            let _ = lines.next_line().await;
        }
    });
    let err = backend.hello().await.unwrap_err();
    assert!(
        matches!(err, BackendError::Transport(TransportError::NotRunning(_))),
        "{err:?}"
    );
}

#[tokio::test]
async fn attach_runs_the_probed_binary_on_the_socket_only_while_it_exists() {
    use std::os::unix::fs::PermissionsExt;
    let fake = Fake::start();
    let dir = tempfile::tempdir().unwrap();
    // Stands in for herdr-core: prints where it would connect and its args.
    let bin = dir.path().join("herdr-core");
    std::fs::write(&bin, "#!/bin/sh\necho \"$HC_SOCKET|$*\"\n").unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let bin = bin.to_string_lossy().into_owned();

    let backend = HerdrCore::new(
        "devbox".into(),
        Arc::new(LocalTransport),
        fake.socket.clone(),
        bin.clone(),
    );
    let argv = backend.attach_argv("t2");
    assert!(argv.contains(&bin) && !argv.iter().any(|a| a == "hc"));
    let out = tokio::process::Command::new(&argv[0])
        .args(&argv[1..])
        .output()
        .await
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{}|attach t2\n", fake.socket)
    );

    // No socket: `attach` would start a core, so it is not run.
    let gone = dir.path().join("gone.sock").to_string_lossy().into_owned();
    let backend = HerdrCore::new("devbox".into(), Arc::new(LocalTransport), gone, bin);
    let argv = backend.attach_argv("t2");
    let out = tokio::process::Command::new(&argv[0])
        .args(&argv[1..])
        .output()
        .await
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("not running"));
}

#[test]
fn core_lock_names_the_socket() {
    // As the core writes it: the path, no newline.
    assert_eq!(
        socket_from_core_lock("/run/user/1000/herdr-core/default.sock").as_deref(),
        Some("/run/user/1000/herdr-core/default.sock")
    );
    // Read through `cat` or an editor: a newline does no harm.
    assert_eq!(
        socket_from_core_lock("/home/u/.local/state/herdr-core/work/work.sock\n").as_deref(),
        Some("/home/u/.local/state/herdr-core/work/work.sock")
    );
    assert_eq!(
        socket_from_core_lock("  /tmp/my socket/hc.sock \r\n").as_deref(),
        Some("/tmp/my socket/hc.sock")
    );
    // Created by a client but never written, or not a Unix path.
    assert_eq!(socket_from_core_lock(""), None);
    assert_eq!(socket_from_core_lock("\n"), None);
    assert_eq!(socket_from_core_lock("relative/default.sock"), None);
    assert_eq!(
        socket_from_core_lock(r"\\.\pipe\herdr-core-u-default.sock"),
        None
    );
    // Windows read from the second byte (ADR 0031) is still no Unix path.
    assert_eq!(
        socket_from_core_lock(r"\.\pipe\herdr-core-u-default.sock"),
        None
    );
}

#[test]
fn default_paths_follow_the_session() {
    assert_eq!(
        default_core_socket("/home/u", "default"),
        "/home/u/.local/state/herdr-core/default/default.sock"
    );
    assert_eq!(
        default_core_socket("/home/u/", " work "),
        "/home/u/.local/state/herdr-core/work/work.sock"
    );
    // A blank session is `default`, as HC_SESSION reads.
    assert_eq!(
        default_core_socket("/Users/u", ""),
        "/Users/u/.local/state/herdr-core/default/default.sock"
    );
    assert_eq!(
        runtime_core_socket("/run/user/1000", ""),
        "/run/user/1000/herdr-core/default.sock"
    );
    assert_eq!(
        core_lock_path("/home/u/.local/state", "work"),
        "/home/u/.local/state/herdr-core/work/core.lock"
    );
}

#[test]
fn last_lines_cuts_from_the_end() {
    assert_eq!(last_lines("a\nb\nc", 2), "b\nc");
    assert_eq!(last_lines("a\nb\nc\n", 2), "b\nc\n");
    assert_eq!(last_lines("a\nb", 5), "a\nb");
    assert_eq!(last_lines("a\nb", 0), "");
}
