//! The read-only host probe: one POSIX `sh` script, run through a
//! [`Transport`], whose `key=value` output becomes a [`ProbeResult`].

use crate::transport::{Transport, TransportError};
use std::time::Duration;

/// The probe script. `$1` is the herdr-core session.
pub const SCRIPT: &str = include_str!("probe.sh");

/// The line before the probe's output; anything a chatty rc file printed
/// earlier is skipped.
pub const BEGIN_MARKER: &str = "@@KOOLIE-PROBE-BEGIN@@";
/// The line after the probe's output.
pub const END_MARKER: &str = "@@KOOLIE-PROBE-END@@";

/// How long the whole probe may take on a host.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// A herdr TUI socket found on a host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HerdrSocket {
    /// `default` or the named session.
    pub session: String,
    pub path: String,
}

/// Where [`ProbeResult::core_socket`] came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreSocketSource {
    /// The session's `core.lock`, written by a core that ran (it outlives
    /// the core, so the path may be stale).
    Lock,
    /// `HC_SOCKET` in the probe's environment.
    Env,
    /// `socket` in herdr-core's `config.toml`.
    Config,
    /// The documented default (runtime dir, else state dir).
    Default,
}

/// What a probe found on a host.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProbeResult {
    pub home: Option<String>,
    /// `uname -s`, e.g. `Linux`, `Darwin`.
    pub os: Option<String>,
    /// `uname -m`, e.g. `x86_64`, `arm64`.
    pub arch: Option<String>,
    pub herdr_bin: Option<String>,
    /// First line of `herdr --version`, e.g. `herdr 0.9.5`.
    pub herdr_version: Option<String>,
    /// Live Herdr TUI sockets (only real sockets), `default` first.
    pub herdr_sockets: Vec<HerdrSocket>,
    /// The `herdr-core` executable. Never run by the probe.
    pub core_bin: Option<String>,
    /// The herdr-core session the core fields refer to.
    pub core_session: Option<String>,
    /// herdr-core socket path, from `core.lock` or the default.
    pub core_socket: Option<String>,
    pub core_socket_source: Option<CoreSocketSource>,
    /// Whether `core_socket` is a socket file right now. A stale socket file
    /// can still refuse connections.
    pub core_socket_present: bool,
    pub koolie_bin: Option<String>,
}

impl ProbeResult {
    /// The version number from `herdr --version`, e.g. `0.9.5`.
    pub fn herdr_version_number(&self) -> Option<&str> {
        self.herdr_version.as_deref().and_then(|v| {
            v.split_whitespace()
                .map(|w| w.trim_start_matches('v'))
                .find(|w| w.starts_with(|c: char| c.is_ascii_digit()))
        })
    }

    /// The socket of a Herdr TUI session, if it is running.
    pub fn herdr_socket(&self, session: &str) -> Option<&HerdrSocket> {
        self.herdr_sockets.iter().find(|s| s.session == session)
    }
}

/// Run the read-only probe on a host, for herdr-core's `default` session.
pub async fn probe(transport: &dyn Transport) -> Result<ProbeResult, TransportError> {
    probe_core_session(transport, "default").await
}

/// Run the read-only probe on a host; the herdr-core fields follow
/// `core_session` (hc's `HC_SESSION`; blank means `default`).
pub async fn probe_core_session(
    transport: &dyn Transport,
    core_session: &str,
) -> Result<ProbeResult, TransportError> {
    let session = check_core_session(core_session)?;
    let argv = probe_argv(&session);
    let out = transport.exec(&argv, None, PROBE_TIMEOUT).await?;
    let stdout = out.stdout_str();
    parse_probe_checked(&stdout).ok_or_else(|| {
        let err = out.stderr_str();
        let err = err.trim();
        let tail: String = err
            .chars()
            .rev()
            .take(300)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        TransportError::Io(format!(
            "host probe gave no result (exit {:?}){}{}",
            out.code,
            if tail.is_empty() { "" } else { ": " },
            tail
        ))
    })
}

/// The argv that runs the probe for a (checked) herdr-core session.
pub fn probe_argv(core_session: &str) -> Vec<String> {
    vec![
        "sh".into(),
        "-c".into(),
        SCRIPT.into(),
        "koolie-probe".into(),
        core_session.into(),
    ]
}

/// Trim a herdr-core session name and refuse what hc refuses.
pub fn check_core_session(raw: &str) -> Result<String, TransportError> {
    let name = raw.trim();
    let name = if name.is_empty() { "default" } else { name };
    if name == "."
        || name == ".."
        || name
            .chars()
            .any(|c| c.is_control() || r#"/\:*?"<>|"#.contains(c))
    {
        return Err(TransportError::Io(format!(
            "herdr-core session {name:?} can't be a folder name"
        )));
    }
    Ok(name.to_string())
}

/// Parse the probe's stdout. Output without the begin marker yields an empty
/// result.
pub fn parse_probe(stdout: &str) -> ProbeResult {
    parse_probe_checked(stdout).unwrap_or_default()
}

/// Parse the probe's stdout; `None` unless both markers are there.
pub fn parse_probe_checked(stdout: &str) -> Option<ProbeResult> {
    let mut lines = stdout.lines().map(|l| l.trim_end_matches('\r'));
    lines.by_ref().find(|l| l.trim() == BEGIN_MARKER)?;
    let mut r = ProbeResult::default();
    let mut core_lock: Option<String> = None;
    let mut core_default: Option<(String, CoreSocketSource)> = None;
    let mut present: Vec<String> = Vec::new();
    let mut ended = false;
    for line in lines {
        if line.trim() == END_MARKER {
            ended = true;
            break;
        }
        let Some((key, raw)) = line.split_once('=') else {
            continue;
        };
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
            continue;
        }
        let fields: Vec<String> = raw.split('\t').map(decode).collect();
        let first = fields.first().cloned().unwrap_or_default();
        let one = || non_empty(first.trim_end_matches(['\n', '\r', '\0']));
        match key {
            "home" => r.home = one(),
            "os" => r.os = one().map(|s| s.trim().to_string()),
            "arch" => r.arch = one().map(|s| s.trim().to_string()),
            "herdr_bin" => r.herdr_bin = one(),
            "herdr_version" => {
                r.herdr_version = first
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty())
                    .map(str::to_string)
            }
            "herdr_socket" => {
                if let [session, path] = fields.as_slice()
                    && !path.is_empty()
                    && !r.herdr_sockets.iter().any(|s| &s.path == path)
                {
                    let session = if session.is_empty() {
                        "default".to_string()
                    } else {
                        session.clone()
                    };
                    r.herdr_sockets.push(HerdrSocket {
                        session,
                        path: path.clone(),
                    });
                }
            }
            "core_bin" => r.core_bin = one(),
            "core_session" => r.core_session = one(),
            "core_lock" => core_lock = one(),
            "core_default_socket" => {
                if let Some(path) = one() {
                    let src = match fields.get(1).map(String::as_str) {
                        Some("env") => CoreSocketSource::Env,
                        Some("config") => CoreSocketSource::Config,
                        _ => CoreSocketSource::Default,
                    };
                    core_default = Some((path, src));
                }
            }
            "present" => present.extend(one()),
            "koolie_bin" => r.koolie_bin = one(),
            _ => {}
        }
    }
    if !ended {
        return None;
    }
    // `default` first, then by name.
    r.herdr_sockets.sort_by(|a, b| {
        (a.session != "default", &a.session).cmp(&(b.session != "default", &b.session))
    });
    let (socket, source) = match (core_lock, core_default) {
        (Some(lock), _) => (Some(lock), Some(CoreSocketSource::Lock)),
        (None, Some((path, src))) => (Some(path), Some(src)),
        (None, None) => (None, None),
    };
    r.core_socket_present = socket.as_ref().is_some_and(|s| present.contains(s));
    r.core_socket = socket;
    r.core_socket_source = source;
    Some(r)
}

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

/// Undo the script's percent-encoding (`%XX`); a malformed escape stays as is.
fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push(h << 4 | l);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrap(body: &str) -> String {
        format!("\n{BEGIN_MARKER}\nv=1\n{body}{END_MARKER}\n")
    }

    #[test]
    fn decodes_escapes() {
        assert_eq!(decode("a%25b%09c%0Ad%0De"), "a%b\tc\nd\re");
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("%zz%4"), "%zz%4");
        assert_eq!(decode("%E6%97%A5"), "日");
    }

    #[test]
    fn missing_herdr() {
        let r = parse_probe(&wrap(
            "home=/home/u\nos=Linux\narch=aarch64\ncore_session=default\n\
             core_default_socket=/run/user/1000/herdr-core/default.sock\tdefault\n",
        ));
        assert_eq!(r.home.as_deref(), Some("/home/u"));
        assert_eq!(r.os.as_deref(), Some("Linux"));
        assert_eq!(r.arch.as_deref(), Some("aarch64"));
        assert_eq!(r.herdr_bin, None);
        assert_eq!(r.herdr_version, None);
        assert_eq!(r.herdr_version_number(), None);
        assert!(r.herdr_sockets.is_empty());
        assert_eq!(r.core_bin, None);
        assert_eq!(
            r.core_socket.as_deref(),
            Some("/run/user/1000/herdr-core/default.sock")
        );
        assert_eq!(r.core_socket_source, Some(CoreSocketSource::Default));
        assert!(!r.core_socket_present);
        assert_eq!(r.koolie_bin, None);
    }

    #[test]
    fn several_sessions_sorted_default_first_and_deduped() {
        let r = parse_probe(&wrap(
            "herdr_bin=/home/u/.local/bin/herdr\n\
             herdr_version=herdr 0.9.5%0Aextra line\n\
             herdr_socket=work\t/home/u/.config/herdr/sessions/work/herdr.sock\n\
             herdr_socket=default\t/home/u/.config/herdr/herdr.sock\n\
             herdr_socket=alpha\t/home/u/.config/herdr/sessions/alpha/herdr.sock\n\
             herdr_socket=alpha\t/home/u/.config/herdr/sessions/alpha/herdr.sock\n",
        ));
        assert_eq!(r.herdr_version.as_deref(), Some("herdr 0.9.5"));
        assert_eq!(r.herdr_version_number(), Some("0.9.5"));
        let names: Vec<&str> = r.herdr_sockets.iter().map(|s| s.session.as_str()).collect();
        assert_eq!(names, ["default", "alpha", "work"]);
        assert_eq!(
            r.herdr_socket("work").map(|s| s.path.as_str()),
            Some("/home/u/.config/herdr/sessions/work/herdr.sock")
        );
    }

    #[test]
    fn paths_with_spaces_tabs_and_newlines() {
        let r = parse_probe(&wrap(
            "home=/Users/Jane Doe\n\
             herdr_bin=/Users/Jane Doe/.local/bin/herdr\n\
             herdr_socket=my session\t/Users/Jane Doe/.config/herdr/sessions/my session/herdr.sock\n\
             herdr_socket=odd%09name%0Ax\t/p/odd%09name%0Ax/herdr.sock\n\
             core_lock=/tmp/a b/%25core.sock\n\
             present=/tmp/a b/%25core.sock\n\
             core_default_socket=/x/default.sock\tdefault\n",
        ));
        assert_eq!(r.home.as_deref(), Some("/Users/Jane Doe"));
        assert_eq!(
            r.herdr_bin.as_deref(),
            Some("/Users/Jane Doe/.local/bin/herdr")
        );
        assert_eq!(r.herdr_sockets[0].session, "my session");
        assert_eq!(r.herdr_sockets[1].session, "odd\tname\nx");
        assert_eq!(r.herdr_sockets[1].path, "/p/odd\tname\nx/herdr.sock");
        assert_eq!(r.core_socket.as_deref(), Some("/tmp/a b/%core.sock"));
        assert_eq!(r.core_socket_source, Some(CoreSocketSource::Lock));
        assert!(r.core_socket_present);
    }

    #[test]
    fn junk_before_and_after_markers_is_ignored() {
        let out = format!(
            "Welcome to Ubuntu 24.04 LTS\nhome=/evil\n * Documentation: https://x\n\
             Last login: Mon\r\n\r\n{BEGIN_MARKER}\r\nv=1\r\nhome=/home/u\r\nnot a kv line\r\n\
             =novalue\r\nBad-Key=1\r\nunknown_key=whatever\r\nos=Darwin\r\n{END_MARKER}\r\n\
             logout\nhome=/after\n"
        );
        let r = parse_probe(&out);
        assert_eq!(r.home.as_deref(), Some("/home/u"));
        assert_eq!(r.os.as_deref(), Some("Darwin"));
        assert!(parse_probe_checked(&out).is_some());
    }

    #[test]
    fn missing_markers() {
        assert_eq!(parse_probe("home=/home/u\n"), ProbeResult::default());
        assert!(parse_probe_checked("home=/home/u\n").is_none());
        // Truncated output (no end marker) is not trusted.
        let cut = format!("{BEGIN_MARKER}\nhome=/home/u\n");
        assert!(parse_probe_checked(&cut).is_none());
    }

    #[test]
    fn core_socket_sources() {
        let env = parse_probe(&wrap(
            "core_bin=/home/u/.local/bin/herdr-core\n\
             core_default_socket=/s/hc.sock\tenv\npresent=/s/hc.sock\n",
        ));
        assert_eq!(
            env.core_bin.as_deref(),
            Some("/home/u/.local/bin/herdr-core")
        );
        assert_eq!(env.core_socket_source, Some(CoreSocketSource::Env));
        assert!(env.core_socket_present);
        let cfg = parse_probe(&wrap("core_default_socket=/c.sock\tconfig\n"));
        assert_eq!(cfg.core_socket_source, Some(CoreSocketSource::Config));
        let none = parse_probe(&wrap(""));
        assert_eq!(none.core_socket, None);
        assert_eq!(none.core_socket_source, None);
    }

    #[test]
    fn core_session_names() {
        assert_eq!(check_core_session("").unwrap(), "default");
        assert_eq!(check_core_session(" smoke ").unwrap(), "smoke");
        for bad in ["a/b", "a\\b", "..", "a\tb", "a:b"] {
            assert!(check_core_session(bad).is_err(), "{bad:?}");
        }
    }
}
