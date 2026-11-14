//! herdr-core terminals projected into koolie's model.
//!
//! herdr-core has flat terminals with labels, a parent and a self-reported
//! Program Status (OSC 7501); it has no workspaces, tabs or agent detection.
//! The rules here are a simplified form of Heeler's ADR 0032 ("herdr-core
//! Hosts fill the Console's Herdr TUI-shaped models").
//!
//! ## Which terminals are agents
//!
//! Only live terminals of the host's own core are considered (`machine/t7`
//! addresses of machines that core reaches are left out). The *program* of a
//! terminal is its foreground process after unwrapping (`env`, `sh -c <line>`,
//! the desktop app's `$SHELL -ic 'exec "$@"' $SHELL <agent>` wrapper,
//! interpreters running a script, `npx`/`npm exec`); with no foreground sample
//! the launch argv stands in. Then:
//!
//! 1. a known agent program in the foreground is an agent;
//! 2. a shell in the foreground is never an agent, whatever was reported last
//!    (the core keeps the last Program Status after the agent exits);
//! 3. any other program is an agent only while the terminal has a Program
//!    Status whose `app` is missing or unknown. A status whose `app` names a
//!    known agent that is *not* the foreground program is stale, so it makes
//!    no agent;
//! 4. a kept terminal waiting for its new shell (no pid, a `reason`) is no
//!    agent.
//!
//! ## Status
//!
//! `working`, `idle`, `done` and `blocked` map to the same [`AgentStatus`].
//! `error` maps to [`AgentStatus::Blocked`]: the agent stopped and needs the
//! user, and Heeler sorts Error with Blocked. No status, or a word this code
//! does not know, is [`AgentStatus::Unknown`].
//!
//! ## Grouping
//!
//! * A terminal labeled `herdr.space` (the Herdr desktop app's label) is in
//!   workspace `space:<value>`, tab `herdr.tab` when present. koolie does not
//!   read the desktop app's private `herdr-app` document, so the workspace is
//!   named like a space nobody named: after the folder its first terminal was
//!   launched in.
//! * A terminal without `herdr.*` labels that another local terminal started
//!   (`parent`) joins that terminal's workspace and tab.
//! * Every other terminal is in the derived workspace `dir:<launch cwd>`,
//!   labeled with the directory's last component, as its own tab.
//!
//! ## Ordering fields
//!
//! herdr-core has no output revision and no state change sequence.
//! `revision` carries the terminal's `turn` counter and `state_change_seq`
//! carries `status_ms` (when the status word last changed, ms since the
//! epoch), else `created_ms`.

use crate::backend::BackendEvent;
use crate::model::{Agent, AgentKey, AgentStatus, HostId};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashMap};

/// The desktop app's labels (its adoption contract).
const LABEL_SPACE: &str = "herdr.space";
const LABEL_TAB: &str = "herdr.tab";

/// Agent programs by the names they run under or report as `app`, with the
/// name koolie (and Herdr TUI) uses for them.
const KNOWN_AGENTS: &[(&str, &str)] = &[
    ("claude", "claude"),
    ("claude-code", "claude"),
    ("codex", "codex"),
    ("codex-cli", "codex"),
    ("opencode", "opencode"),
    ("opencode-ai", "opencode"),
    ("pi", "pi"),
    ("pi-coding-agent", "pi"),
    ("omp", "omp"),
    ("grok", "grok"),
    ("grok-build", "grok"),
    ("gemini", "gemini"),
    ("gemini-cli", "gemini"),
    ("amp", "amp"),
    ("cursor-agent", "cursor-agent"),
    ("copilot", "copilot"),
    ("aider", "aider"),
    ("goose", "goose"),
    ("droid", "droid"),
    ("crush", "crush"),
    ("qwen", "qwen"),
    ("qwen-code", "qwen"),
];

const SHELLS: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "fish",
    "dash",
    "ash",
    "ksh",
    "mksh",
    "csh",
    "tcsh",
    "nu",
    "elvish",
    "xonsh",
    "pwsh",
    "powershell",
    "cmd",
    "login",
];

const INTERPRETERS: &[&str] = &[
    "node", "nodejs", "bun", "deno", "tsx", "ts-node", "python", "python3", "ruby",
];

/// How deep wrappers are followed.
const MAX_UNWRAP: usize = 6;

#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct RawStatus {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    app: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct RawForeground {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    argv: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
}

/// The fields of herdr-core's `Terminal` record koolie reads.
#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct RawTerminal {
    address: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    labels: BTreeMap<String, String>,
    #[serde(default)]
    argv: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    pid: Option<u64>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    created_ms: u64,
    #[serde(default)]
    status: Option<RawStatus>,
    #[serde(default)]
    status_ms: Option<u64>,
    #[serde(default)]
    turn: u64,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    foreground: Option<RawForeground>,
}

fn non_empty(s: Option<&str>) -> Option<String> {
    s.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// A terminal of the core itself, not of a machine it reaches (`rohan/t7`).
fn is_local(address: &str) -> bool {
    !address.is_empty() && !address.contains('/')
}

/// Last path component, lowercased, without a login shell's leading `-` or a
/// Windows `.exe`.
fn base_name(program: &str) -> String {
    let name = program
        .trim_matches(|c| c == '"' || c == '\'')
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("");
    let name = name.strip_prefix('-').unwrap_or(name).to_ascii_lowercase();
    name.strip_suffix(".exe").unwrap_or(&name).to_string()
}

fn is_shell(program: &str) -> bool {
    SHELLS.contains(&program)
}

/// koolie's name for a known agent program or reported `app`.
pub(crate) fn known_agent(name: &str) -> Option<&'static str> {
    let name = name.trim().to_ascii_lowercase();
    KNOWN_AGENTS
        .iter()
        .find(|(alias, _)| *alias == name)
        .map(|(_, agent)| *agent)
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(k, _)| {
        !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// A script or package path names an agent by its file name
/// (`~/.local/bin/claude`, `codex.js`) or by its package folder
/// (`.../node_modules/@anthropic-ai/claude-code/cli.js`). Other folders on
/// the way say nothing: `/home/pi/app/server.js` is not Pi.
fn agent_in_path(path: &str) -> Option<&'static str> {
    let parts: Vec<String> = path
        .split(['/', '\\'])
        .filter(|p| !p.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    let file = parts.last()?;
    let stem = file.split_once('.').map_or(file.as_str(), |(stem, _)| stem);
    if let Some(agent) = known_agent(file).or_else(|| known_agent(stem)) {
        return Some(agent);
    }
    let modules = parts.iter().rposition(|p| p == "node_modules")?;
    parts[modules + 1..].iter().find_map(|p| known_agent(p))
}

/// The program an argv runs, after unwrapping. `None` when argv is empty.
pub(crate) fn program_of(argv: &[String]) -> Option<String> {
    unwrap_program(argv, 0)
}

fn unwrap_program(argv: &[String], depth: usize) -> Option<String> {
    let (first, rest) = argv.split_first()?;
    let prog = base_name(first);
    if prog.is_empty() {
        return None;
    }
    if depth >= MAX_UNWRAP {
        return Some(prog);
    }
    if prog == "env" {
        // Options and assignments, then the command.
        let mut i = 0;
        while i < rest.len() {
            let a = &rest[i];
            if a == "-u" || a == "--unset" || a == "-C" || a == "--chdir" {
                i += 2;
            } else if a.starts_with('-') || is_assignment(a) {
                i += 1;
            } else {
                break;
            }
        }
        return unwrap_program(rest.get(i..).unwrap_or(&[]), depth + 1).or(Some(prog));
    }
    if is_shell(&prog) {
        return Some(shell_program(&prog, rest, depth));
    }
    if INTERPRETERS.contains(&prog.as_str()) {
        let mut args = rest.iter().filter(|a| !a.starts_with('-'));
        let mut script = args.next();
        if prog == "deno" && script.is_some_and(|s| s == "run") {
            script = args.next();
        }
        return Some(match script {
            Some(script) => match agent_in_path(script) {
                Some(agent) => agent.to_string(),
                None => {
                    let name = base_name(script);
                    name.split_once('.')
                        .map_or(name.clone(), |(stem, _)| stem.to_string())
                }
            },
            None => prog,
        });
    }
    let runner = matches!(prog.as_str(), "npx" | "bunx" | "pnpx")
        || (matches!(prog.as_str(), "npm" | "pnpm" | "yarn" | "bun")
            && rest
                .first()
                .is_some_and(|a| matches!(a.as_str(), "exec" | "x" | "dlx")));
    if runner {
        let skip = usize::from(!matches!(prog.as_str(), "npx" | "bunx" | "pnpx"));
        let package = rest.iter().skip(skip).find(|a| !a.starts_with('-'));
        return Some(match package {
            Some(package) => {
                // `@scope/name@version` -> `@scope/name`.
                let name = match package.rfind('@') {
                    Some(at) if at > 0 => &package[..at],
                    _ => package.as_str(),
                };
                let name = base_name(name);
                known_agent(&name).map_or(name.clone(), str::to_string)
            }
            None => prog,
        });
    }
    Some(prog)
}

/// What a shell invocation runs: the command of `-c <line>`, a script, or the
/// shell itself.
fn shell_program(shell: &str, rest: &[String], depth: usize) -> String {
    // Leading options; one holding `c` takes the command line.
    let mut i = 0;
    let mut line = None;
    while let Some(arg) = rest.get(i) {
        if !arg.starts_with('-') || arg == "-" || arg == "--" {
            break;
        }
        i += 1;
        if !arg.starts_with("--") && arg.contains('c') {
            line = rest.get(i);
            i += 1;
            break;
        }
    }
    let Some(line) = line else {
        // A script the shell runs is its own program; no script is the shell.
        return match rest.get(i).filter(|a| a.as_str() != "--") {
            Some(script) => {
                let name = base_name(script);
                if name.is_empty() {
                    shell.to_string()
                } else {
                    name
                }
            }
            None => shell.to_string(),
        };
    };
    // The desktop app's wrappers: `exec "$@"` (POSIX) and `exec $argv[2..]`
    // (fish) replace the shell with the arguments after `$0`.
    if line.contains("$@") || line.contains("$argv") {
        let positional = rest.get(i + 1..).unwrap_or(&[]);
        return unwrap_program(positional, depth + 1).unwrap_or_else(|| shell.to_string());
    }
    let segments: Vec<Vec<String>> = line
        .split([';', '|', '&', '\n'])
        .map(command_words)
        .filter(|words| !words.is_empty())
        .collect();
    match segments.as_slice() {
        [] => shell.to_string(),
        [only] => unwrap_program(only, depth + 1).unwrap_or_else(|| shell.to_string()),
        // A compound line is the shell's own script, unless it runs an agent
        // (`cd app && claude`): a non-interactive shell stays the foreground
        // for the agent's whole life.
        many => many
            .iter()
            .filter_map(|words| unwrap_program(words, depth + 1))
            .find(|p| known_agent(p).is_some())
            .unwrap_or_else(|| shell.to_string()),
    }
}

/// The words of one simple command, without `exec` and leading assignments.
fn command_words(segment: &str) -> Vec<String> {
    segment
        .split_whitespace()
        .skip_while(|w| matches!(*w, "exec" | "command" | "nohup" | "time") || is_assignment(w))
        .map(|w| w.trim_matches(|c| c == '"' || c == '\'').to_string())
        .collect()
}

pub(crate) fn map_status(word: Option<&str>) -> AgentStatus {
    match word.map(|w| w.trim().to_ascii_lowercase()).as_deref() {
        Some("working") => AgentStatus::Working,
        Some("idle") => AgentStatus::Idle,
        Some("done") => AgentStatus::Done,
        // Error: the agent stopped and needs the user (see the module docs).
        Some("blocked") | Some("error") => AgentStatus::Blocked,
        _ => AgentStatus::Unknown,
    }
}

/// A terminal title without the spinner or status glyph programs put first.
fn strip_title(title: &str) -> String {
    title
        .trim_start_matches(|c: char| c.is_whitespace() || (!c.is_ascii() && !c.is_alphanumeric()))
        .trim()
        .to_string()
}

fn folder_name(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return (!path.is_empty()).then(|| "/".to_string());
    }
    trimmed.rsplit('/').next().map(str::to_string)
}

impl RawTerminal {
    fn is_live(&self) -> bool {
        self.state.as_deref() == Some("live")
    }

    fn status_word(&self) -> Option<&str> {
        self.status.as_ref().and_then(|s| s.state.as_deref())
    }

    fn has_desktop_labels(&self) -> bool {
        self.labels.keys().any(|k| k.starts_with("herdr."))
    }

    fn space(&self) -> Option<&str> {
        self.labels
            .get(LABEL_SPACE)
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
    }

    fn launch_cwd(&self) -> &str {
        self.cwd.as_deref().unwrap_or("")
    }

    /// `Some(program name)` when this terminal is an agent (the name may be
    /// unknown), per the rules in the module docs.
    pub(crate) fn agent_program(&self) -> Option<Option<String>> {
        if !self.is_live() {
            return None;
        }
        // Lost, waiting for its new shell (or its first spawn).
        if self.pid.is_none() && self.reason.is_some() {
            return None;
        }
        let sampled = self
            .foreground
            .as_ref()
            .filter(|f| !f.argv.is_empty() || non_empty(f.name.as_deref()).is_some());
        let os_name = sampled
            .and_then(|f| non_empty(f.name.as_deref()))
            .map(|n| base_name(&n));
        let program = match sampled {
            Some(f) => program_of(&f.argv).or_else(|| os_name.clone()),
            None => program_of(&self.argv),
        };
        let known = program
            .as_deref()
            .and_then(known_agent)
            .or_else(|| os_name.as_deref().and_then(known_agent));
        if let Some(agent) = known {
            return Some(Some(agent.to_string()));
        }
        if program.as_deref().is_some_and(is_shell) {
            return None;
        }
        let status = self.status.as_ref()?;
        non_empty(status.state.as_deref())?;
        match non_empty(status.app.as_deref()) {
            // A known agent reported, but something else holds the terminal now.
            Some(app) if known_agent(&app).is_some() => None,
            Some(app) => Some(Some(app)),
            None => Some(program),
        }
    }

    fn title(&self) -> Option<String> {
        non_empty(self.name.as_deref())
            .or_else(|| non_empty(self.status.as_ref().and_then(|s| s.title.as_deref())))
            .or_else(|| non_empty(self.title.as_deref().map(strip_title).as_deref()))
    }

    fn current_cwd(&self) -> Option<String> {
        non_empty(self.foreground.as_ref().and_then(|f| f.cwd.as_deref()))
            .or_else(|| non_empty(self.cwd.as_deref()))
    }
}

/// Where a terminal is shown.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Place {
    workspace_id: String,
    workspace_label: Option<String>,
    tab_id: Option<String>,
}

fn place_of(t: &RawTerminal, all: &HashMap<&str, &RawTerminal>, hops: usize) -> Place {
    if let Some(space) = t.space() {
        // Named like a space nobody named: after its first terminal's folder.
        let first = all
            .values()
            .filter(|o| o.space() == Some(space))
            .min_by(|a, b| {
                (a.created_ms, a.address.len(), &a.address).cmp(&(
                    b.created_ms,
                    b.address.len(),
                    &b.address,
                ))
            })
            .copied()
            .unwrap_or(t);
        return Place {
            workspace_id: format!("space:{space}"),
            workspace_label: folder_name(first.launch_cwd()),
            tab_id: non_empty(t.labels.get(LABEL_TAB).map(String::as_str))
                .or_else(|| Some(t.address.clone())),
        };
    }
    if !t.has_desktop_labels()
        && hops < 16
        && let Some(parent) = t.parent.as_deref().and_then(|p| all.get(p))
        && parent.address != t.address
    {
        return place_of(parent, all, hops + 1);
    }
    let cwd = t.launch_cwd();
    Place {
        workspace_id: format!("dir:{cwd}"),
        workspace_label: folder_name(cwd),
        tab_id: Some(t.address.clone()),
    }
}

fn parse_terminal(record: &Value) -> Option<RawTerminal> {
    RawTerminal::deserialize(record).ok()
}

/// The agents among a core's terminal records, in the core's order. Records
/// that do not parse are skipped.
pub(crate) fn project<'a>(
    host: &HostId,
    records: impl IntoIterator<Item = &'a Value>,
) -> Vec<Agent> {
    let terminals: Vec<RawTerminal> = records
        .into_iter()
        .filter_map(parse_terminal)
        .filter(|t| is_local(&t.address) && t.is_live())
        .collect();
    let by_address: HashMap<&str, &RawTerminal> =
        terminals.iter().map(|t| (t.address.as_str(), t)).collect();
    terminals
        .iter()
        .filter_map(|t| {
            let program = t.agent_program()?;
            let place = place_of(t, &by_address, 0);
            Some(Agent {
                key: AgentKey {
                    host: host.clone(),
                    id: t.address.clone(),
                },
                agent: program,
                display_agent: None,
                title: t.title(),
                status: map_status(t.status_word()),
                workspace_id: place.workspace_id,
                workspace_label: place.workspace_label,
                tab_id: place.tab_id,
                cwd: t.current_cwd(),
                revision: t.turn,
                state_change_seq: t.status_ms.unwrap_or(t.created_ms),
                session: None,
                focused: false,
            })
        })
        .collect()
}

/// The terminal records an event subscription keeps, by address, so that a
/// `status` event can be told from an agent's or a shell's.
#[derive(Debug, Default)]
pub(crate) struct Terminals {
    records: HashMap<String, Map<String, Value>>,
    /// Events at or below this sequence are already in `records`.
    seq: u64,
}

impl Terminals {
    /// From an `events.subscribe` result or a `missed` notification.
    pub(crate) fn from_snapshot(snapshot: &Value) -> Self {
        let mut me = Terminals::default();
        me.reset(snapshot);
        me
    }

    fn reset(&mut self, snapshot: &Value) {
        self.seq = snapshot.get("seq").and_then(Value::as_u64).unwrap_or(0);
        self.records.clear();
        let terminals = snapshot.get("terminals").and_then(Value::as_array);
        for record in terminals.into_iter().flatten() {
            self.put(record);
        }
    }

    fn put(&mut self, record: &Value) {
        if let Some(fields) = record.as_object()
            && let Some(address) = fields.get("address").and_then(Value::as_str)
            && is_local(address)
        {
            self.records.insert(address.to_string(), fields.clone());
        }
    }

    fn status_of(&self, address: &str) -> Option<AgentStatus> {
        let record = Value::Object(self.records.get(address)?.clone());
        let terminal = parse_terminal(&record)?;
        terminal.agent_program()?;
        Some(map_status(terminal.status_word()))
    }

    /// A `missed` notification: the subscriber fell behind and gets a fresh
    /// snapshot instead of the backlog.
    pub(crate) fn missed(&mut self, params: &Value) -> BackendEvent {
        self.reset(params);
        BackendEvent::Changed
    }

    /// Apply one `event` notification as herdr-core's `schema/README.md` says
    /// to; `None` when it changes nothing the inventory shows.
    pub(crate) fn apply(&mut self, params: &Value) -> Option<BackendEvent> {
        if let Some(seq) = params.get("seq").and_then(Value::as_u64) {
            if seq <= self.seq {
                return None;
            }
            self.seq = seq;
        }
        let kind = params.get("type").and_then(Value::as_str).unwrap_or("");
        // Documents have no terminal; other machines' terminals are not listed.
        let address = params
            .get("terminal")
            .and_then(Value::as_str)
            .filter(|a| is_local(a))?;
        let data = params.get("data").cloned().unwrap_or(Value::Null);
        match kind {
            "doc" | "size" | "keep" => return None,
            "created" | "restarted" => self.put(&data),
            "removed" => {
                self.records.remove(address);
            }
            _ => {}
        }
        let record = self.records.get_mut(address);
        let fields = data.as_object();
        let copy = |record: &mut Map<String, Value>, keys: &[&str]| {
            for key in keys {
                if let Some(v) = fields.and_then(|f| f.get(*key)) {
                    record.insert((*key).to_string(), v.clone());
                }
            }
        };
        match (kind, record) {
            ("status", Some(record)) => {
                copy(record, &["status", "records", "reported"]);
                if let Some(status) = self.status_of(address) {
                    return Some(BackendEvent::StatusChanged {
                        id: address.to_string(),
                        status,
                    });
                }
            }
            ("turn", Some(record)) => {
                copy(record, &["turn"]);
                // The status event beside it already asks for a refetch.
                return None;
            }
            ("title", Some(record)) => copy(record, &["title"]),
            ("labels", Some(record)) => copy(record, &["labels"]),
            ("foreground", Some(record)) => {
                if data.is_null() {
                    record.remove("foreground");
                } else {
                    record.insert("foreground".into(), data.clone());
                }
            }
            ("finished", Some(record)) => {
                copy(record, &["reason", "exit_code", "signal", "finished_ms"]);
                record.insert("state".into(), Value::String("finished".into()));
                record.remove("foreground");
            }
            ("lost", Some(record)) => {
                copy(record, &["reason", "exit_code", "signal", "finished_ms"]);
                record.insert("pid".into(), Value::Null);
                record.remove("foreground");
            }
            _ => {}
        }
        Some(BackendEvent::Changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn programs_are_unwrapped() {
        let p = |words: &[&str]| program_of(&argv(words));
        assert_eq!(p(&[]), None);
        assert_eq!(p(&["/usr/bin/zsh"]).as_deref(), Some("zsh"));
        assert_eq!(p(&["-zsh"]).as_deref(), Some("zsh"));
        assert_eq!(p(&["claude", "--resume"]).as_deref(), Some("claude"));
        assert_eq!(
            p(&["env", "-u", "TMUX", "FOO=1", "codex"]).as_deref(),
            Some("codex")
        );
        // The desktop app's wrappers.
        assert_eq!(
            p(&[
                "/bin/zsh",
                "-ic",
                "exec \"$@\"",
                "/bin/zsh",
                "claude",
                "--model",
                "opus"
            ])
            .as_deref(),
            Some("claude")
        );
        assert_eq!(
            p(&["fish", "-ic", "exec $argv[2..]", "fish", "codex"]).as_deref(),
            Some("codex")
        );
        // A template's command under a non-interactive sh.
        assert_eq!(
            p(&["sh", "-c", "claude --model opus"]).as_deref(),
            Some("claude")
        );
        assert_eq!(
            p(&["sh", "-c", "cd app && exec claude"]).as_deref(),
            Some("claude")
        );
        assert_eq!(
            p(&["sh", "-c", "cd app && npm test"]).as_deref(),
            Some("sh")
        );
        // A script is its own program; a bare shell is a shell.
        assert_eq!(p(&["bash", "./deploy.sh"]).as_deref(), Some("deploy.sh"));
        assert_eq!(p(&["bash", "-l"]).as_deref(), Some("bash"));
        // Interpreters and package runners.
        assert_eq!(
            p(&[
                "node",
                "/usr/lib/node_modules/@anthropic-ai/claude-code/cli.js"
            ])
            .as_deref(),
            Some("claude")
        );
        assert_eq!(
            p(&["node", "--no-warnings", "/opt/tools/server.js"]).as_deref(),
            Some("server")
        );
        assert_eq!(
            p(&["npx", "-y", "@openai/codex@latest"]).as_deref(),
            Some("codex")
        );
        assert_eq!(p(&["npm", "exec", "claude"]).as_deref(), Some("claude"));
        assert_eq!(p(&["vim", "notes.md"]).as_deref(), Some("vim"));
    }

    #[test]
    fn status_words_map() {
        assert_eq!(map_status(Some("working")), AgentStatus::Working);
        assert_eq!(map_status(Some("idle")), AgentStatus::Idle);
        assert_eq!(map_status(Some("done")), AgentStatus::Done);
        assert_eq!(map_status(Some("blocked")), AgentStatus::Blocked);
        assert_eq!(map_status(Some("error")), AgentStatus::Blocked);
        assert_eq!(map_status(Some("compacting")), AgentStatus::Unknown);
        assert_eq!(map_status(None), AgentStatus::Unknown);
    }

    #[test]
    fn titles_lose_their_glyph() {
        assert_eq!(strip_title("✳ Fix the build"), "Fix the build");
        assert_eq!(strip_title("⠂ 日本語のタイトル "), "日本語のタイトル");
        assert_eq!(strip_title("~/dev/app"), "~/dev/app");
    }

    #[test]
    fn folder_names() {
        assert_eq!(folder_name("/home/u/dev/app").as_deref(), Some("app"));
        assert_eq!(folder_name("/home/u/dev/app/").as_deref(), Some("app"));
        assert_eq!(folder_name("/").as_deref(), Some("/"));
        assert_eq!(folder_name(""), None);
    }
}
