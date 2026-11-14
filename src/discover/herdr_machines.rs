//! herdr's saved SSH machines from `herdr machine list --json`.
//!
//! herdr 0.9.5 prints a JSON array of
//! `{id, label, target, session, enabled, selected}`; anything else is read
//! defensively and unknown shapes yield nothing.

use serde_json::Value;

/// One saved herdr machine profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HerdrMachine {
    pub id: Option<String>,
    pub label: String,
    /// SSH target as herdr stores it: an ssh config host, `user@host`, or
    /// `ssh://user@host:port`.
    pub target: String,
    /// The remote herdr session the profile uses.
    pub session: Option<String>,
    pub enabled: Option<bool>,
}

/// Parse `herdr machine list --json`.
pub fn parse_machine_list(json: &str) -> Vec<HerdrMachine> {
    let Ok(v) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    let rows = match &v {
        Value::Array(rows) => rows.as_slice(),
        Value::Object(o) => match ["machines", "ssh", "profiles"]
            .iter()
            .find_map(|k| o.get(*k).and_then(Value::as_array))
        {
            Some(rows) => rows.as_slice(),
            None => return Vec::new(),
        },
        _ => return Vec::new(),
    };
    rows.iter().filter_map(row).collect()
}

fn row(v: &Value) -> Option<HerdrMachine> {
    let o = v.as_object()?;
    let s = |k: &str| {
        o.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let target = s("target").or_else(|| s("ssh_target"))?;
    Some(HerdrMachine {
        id: s("id"),
        label: s("label").unwrap_or_else(|| target.clone()),
        target,
        session: s("session"),
        enabled: o.get("enabled").and_then(Value::as_bool),
    })
}

/// Split an SSH target into user, host and port: `host`, `user@host`,
/// `ssh://user@host:port`, `ssh://[v6]:port`.
pub fn split_target(target: &str) -> (Option<&str>, &str, Option<u16>) {
    let (rest, is_url) = match target.strip_prefix("ssh://") {
        Some(r) => (r.trim_end_matches('/'), true),
        None => (target, false),
    };
    let (user, hostport) = match rest.rsplit_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, rest),
    };
    if !is_url {
        return (user, hostport, None);
    }
    if let Some(v6) = hostport.strip_prefix('[')
        && let Some((host, after)) = v6.split_once(']')
    {
        return (
            user,
            host,
            after.strip_prefix(':').and_then(|p| p.parse().ok()),
        );
    }
    match hostport.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => (user, h, p.parse().ok()),
        _ => (user, hostport, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_list_rows() {
        let json = r#"[
          {"id": "0123456789abcdef0123456789abcdef", "label": "Build", "target": "dev@build",
           "session": "agents", "enabled": true, "selected": false, "future": 1},
          {"id": "fedcba9876543210fedcba9876543210", "label": "", "target": "ssh://me@10.0.0.2:2222",
           "session": "default", "enabled": false, "selected": true},
          {"label": "no target"},
          "junk",
          {"target": 5}
        ]"#;
        let m = parse_machine_list(json);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].label, "Build");
        assert_eq!(m[0].target, "dev@build");
        assert_eq!(m[0].session.as_deref(), Some("agents"));
        assert_eq!(m[0].enabled, Some(true));
        assert_eq!(m[1].label, "ssh://me@10.0.0.2:2222");
        assert_eq!(m[1].enabled, Some(false));
    }

    #[test]
    fn unknown_shapes_are_empty() {
        assert!(parse_machine_list("[]").is_empty());
        assert!(parse_machine_list("").is_empty());
        assert!(parse_machine_list("{}").is_empty());
        assert!(parse_machine_list("42").is_empty());
        assert!(parse_machine_list(r#"{"machines": {"a": 1}}"#).is_empty());
        assert_eq!(
            parse_machine_list(r#"{"machines": [{"target": "box"}]}"#)[0].target,
            "box"
        );
    }

    #[test]
    fn targets() {
        assert_eq!(split_target("box"), (None, "box", None));
        assert_eq!(split_target("me@box"), (Some("me"), "box", None));
        assert_eq!(
            split_target("ssh://me@box.lan:2222"),
            (Some("me"), "box.lan", Some(2222))
        );
        assert_eq!(split_target("ssh://box/"), (None, "box", None));
        assert_eq!(
            split_target("ssh://[fd7a::1]:22"),
            (None, "fd7a::1", Some(22))
        );
    }
}
