//! `~/.ssh/config` `Host` entries, read-only.

use std::path::{Path, PathBuf};

/// One concrete `Host` name from an ssh config.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SshHost {
    /// The name after `Host` (what `ssh <alias>` takes).
    pub alias: String,
    /// `HostName`, with `%h` and `%%` expanded.
    pub host_name: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
}

impl SshHost {
    /// The host ssh would connect to.
    pub fn resolved_host(&self) -> &str {
        self.host_name.as_deref().unwrap_or(&self.alias)
    }
}

/// OpenSSH's limit on nested `Include`s.
const MAX_INCLUDE_DEPTH: usize = 16;

/// Hosts that are code forges, not machines that could run herdr.
const FORGES: &[&str] = &[
    "github.com",
    "ssh.github.com",
    "gitlab.com",
    "altssh.gitlab.com",
    "bitbucket.org",
    "altssh.bitbucket.org",
    "ssh.dev.azure.com",
    "vs-ssh.visualstudio.com",
    "codeberg.org",
    "git.sr.ht",
];

/// Parse ssh config text. `load` resolves one `Include` argument (as written,
/// e.g. `config.d/*`) to the contents of the files it names, in order.
///
/// Only concrete `Host` names are returned (patterns with `*`, `?` or `!` are
/// skipped), in first-seen order; a name seen twice keeps the first value of
/// each keyword, as ssh does. `Match` blocks are ignored. Hosts that are code
/// forges (github.com, ...) are dropped.
pub fn parse_ssh_config(text: &str, load: &mut dyn FnMut(&str) -> Vec<String>) -> Vec<SshHost> {
    let mut hosts = Vec::new();
    parse_into(text, load, &mut hosts, 0);
    hosts.retain(|h| {
        let lc = h.resolved_host().to_ascii_lowercase();
        !FORGES.contains(&lc.as_str()) && !FORGES.contains(&h.alias.to_ascii_lowercase().as_str())
    });
    hosts
}

/// Which block keywords apply to.
enum Block {
    /// Before any Host/Match: applies to every host; ignored here.
    Global,
    /// Indexes into `hosts` of the names this `Host` line declared.
    Host(Vec<usize>),
    Match,
}

fn parse_into(
    text: &str,
    load: &mut dyn FnMut(&str) -> Vec<String>,
    hosts: &mut Vec<SshHost>,
    depth: usize,
) {
    let mut block = Block::Global;
    for line in text.lines() {
        let Some((keyword, args)) = split_line(line) else {
            continue;
        };
        match keyword.as_str() {
            "host" => {
                let mut idx = Vec::new();
                for name in &args {
                    if name.is_empty() || name.contains(['*', '?', '!']) {
                        continue;
                    }
                    let i = match hosts.iter().position(|h| h.alias == *name) {
                        Some(i) => i,
                        None => {
                            hosts.push(SshHost {
                                alias: name.clone(),
                                ..SshHost::default()
                            });
                            hosts.len() - 1
                        }
                    };
                    if !idx.contains(&i) {
                        idx.push(i);
                    }
                }
                block = Block::Host(idx);
            }
            "match" => block = Block::Match,
            "include" => {
                if depth >= MAX_INCLUDE_DEPTH {
                    continue;
                }
                for arg in &args {
                    for content in load(arg) {
                        parse_into(&content, load, hosts, depth + 1);
                    }
                }
            }
            "hostname" | "user" | "port" => {
                let Block::Host(idx) = &block else {
                    continue;
                };
                let Some(value) = args.first() else {
                    continue;
                };
                for &i in idx {
                    let h = &mut hosts[i];
                    match keyword.as_str() {
                        "hostname" if h.host_name.is_none() => {
                            h.host_name = Some(expand_host_name(value, &h.alias));
                        }
                        "user" if h.user.is_none() => h.user = Some(value.clone()),
                        "port" if h.port.is_none() => h.port = value.parse().ok(),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

/// `HostName` tokens: `%h` is the alias, `%%` a percent sign.
fn expand_host_name(value: &str, alias: &str) -> String {
    let mut out = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('h') => out.push_str(alias),
            Some('%') => out.push('%'),
            Some(o) => {
                out.push('%');
                out.push(o);
            }
            None => out.push('%'),
        }
    }
    out
}

/// Split one config line into a lowercased keyword and its arguments.
/// `Keyword value`, `Keyword=value` and `Keyword = value` are all accepted;
/// double quotes group words.
fn split_line(line: &str) -> Option<(String, Vec<String>)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let end = line
        .find(|c: char| c.is_whitespace() || c == '=')
        .unwrap_or(line.len());
    let keyword = line[..end].to_ascii_lowercase();
    let mut rest = line[end..].trim_start();
    if let Some(r) = rest.strip_prefix('=') {
        rest = r.trim_start();
    }
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut has_word = false;
    for c in rest.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                has_word = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if has_word {
                    args.push(std::mem::take(&mut cur));
                    has_word = false;
                }
            }
            '#' if !in_quotes && !has_word => break,
            c => {
                cur.push(c);
                has_word = true;
            }
        }
    }
    if has_word {
        args.push(cur);
    }
    Some((keyword, args))
}

/// The loader `ssh` itself uses for the user config: `~` is `home`, relative
/// paths are under `ssh_dir`, and `*`/`?` glob in any component. Unreadable
/// files are skipped.
pub fn fs_include_loader(ssh_dir: PathBuf, home: PathBuf) -> impl FnMut(&str) -> Vec<String> {
    move |arg: &str| {
        let path = if arg == "~" {
            home.clone()
        } else if let Some(rest) = arg.strip_prefix("~/") {
            home.join(rest)
        } else if Path::new(arg).is_absolute() {
            PathBuf::from(arg)
        } else {
            ssh_dir.join(arg)
        };
        glob(&path)
            .into_iter()
            .filter(|p| p.is_file())
            .filter_map(|p| std::fs::read_to_string(p).ok())
            .collect()
    }
}

/// Expand `*` and `?` in each path component, sorted, without matching a
/// leading `.` unless the pattern has one (as glob(3) does).
fn glob(pattern: &Path) -> Vec<PathBuf> {
    let mut found = vec![PathBuf::new()];
    for comp in pattern.components() {
        let comp_s = comp.as_os_str().to_string_lossy();
        if !comp_s.contains(['*', '?']) {
            for p in &mut found {
                p.push(comp.as_os_str());
            }
            continue;
        }
        let mut next = Vec::new();
        for dir in &found {
            let Ok(entries) = std::fs::read_dir(if dir.as_os_str().is_empty() {
                Path::new(".")
            } else {
                dir.as_path()
            }) else {
                continue;
            };
            let mut names: Vec<String> = entries
                .filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| wildcard_match(&comp_s, n))
                .filter(|n| !n.starts_with('.') || comp_s.starts_with('.'))
                .collect();
            names.sort();
            next.extend(names.into_iter().map(|n| dir.join(n)));
        }
        found = next;
    }
    found
}

fn wildcard_match(pattern: &str, name: &str) -> bool {
    fn go(p: &[char], n: &[char]) -> bool {
        match p.split_first() {
            None => n.is_empty(),
            Some(('*', rest)) => (0..=n.len()).any(|i| go(rest, &n[i..])),
            Some(('?', rest)) => !n.is_empty() && go(rest, &n[1..]),
            Some((c, rest)) => n.first() == Some(c) && go(rest, &n[1..]),
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    go(&p, &n)
}

/// Read `~/.ssh/config` and its includes. Missing or unreadable files yield
/// nothing.
pub fn read_user_ssh_config(home: &Path) -> Vec<SshHost> {
    let ssh_dir = home.join(".ssh");
    let Ok(text) = std::fs::read_to_string(ssh_dir.join("config")) else {
        return Vec::new();
    };
    let mut loader = fs_include_loader(ssh_dir, home.to_path_buf());
    parse_ssh_config(&text, &mut loader)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn parse_with(text: &str, files: &[(&str, &str)]) -> Vec<SshHost> {
        let files: HashMap<String, String> = files
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let mut load = |arg: &str| files.get(arg).cloned().into_iter().collect::<Vec<_>>();
        parse_ssh_config(text, &mut load)
    }

    fn aliases(hosts: &[SshHost]) -> Vec<&str> {
        hosts.iter().map(|h| h.alias.as_str()).collect()
    }

    #[test]
    fn hosts_patterns_and_keywords() {
        let hosts = parse_with(
            "# comment\n\
             ServerAliveInterval 30\n\
             HOST box box-alt  \"quoted name\"\n\
             \tHostName=box.example.com\n\
             \tuser koba\n\
             \tPORT = 2222\n\
             Host *.internal !bad web?\n\
             \tHostName nope\n\
             Host pi\n\
             \thostname 192.168.1.5 # trailing comment\n\
             Host *\n\
             \tUser everyone\n",
            &[],
        );
        assert_eq!(aliases(&hosts), ["box", "box-alt", "quoted name", "pi"]);
        assert_eq!(hosts[0].host_name.as_deref(), Some("box.example.com"));
        assert_eq!(hosts[1].host_name.as_deref(), Some("box.example.com"));
        assert_eq!(hosts[0].user.as_deref(), Some("koba"));
        assert_eq!(hosts[0].port, Some(2222));
        assert_eq!(hosts[3].host_name.as_deref(), Some("192.168.1.5"));
        assert_eq!(hosts[3].user, None, "Host * is a pattern, not applied");
    }

    #[test]
    fn match_blocks_are_ignored() {
        let hosts = parse_with(
            "Host a\n  HostName a.lan\nMatch host a exec \"true\"\n  HostName evil\n  User x\nHost b\n",
            &[],
        );
        assert_eq!(aliases(&hosts), ["a", "b"]);
        assert_eq!(hosts[0].host_name.as_deref(), Some("a.lan"));
        assert_eq!(hosts[0].user, None);
    }

    #[test]
    fn first_value_wins_and_tokens_expand() {
        let hosts = parse_with(
            "Host dev\n HostName %h.corp\nHost dev\n HostName other\n User u\nHost pct\n HostName a%%b%x\n",
            &[],
        );
        assert_eq!(hosts.len(), 2);
        assert_eq!(hosts[0].host_name.as_deref(), Some("dev.corp"));
        assert_eq!(hosts[0].user.as_deref(), Some("u"));
        assert_eq!(hosts[1].host_name.as_deref(), Some("a%b%x"));
    }

    #[test]
    fn includes_nested_and_inline() {
        let hosts = parse_with(
            "Include conf.d/*\nHost top\n Include inner\n HostName top.lan\n",
            &[
                (
                    "conf.d/*",
                    "Host inc1\n HostName inc1.lan\nInclude deeper\n",
                ),
                ("deeper", "Host inc2\n"),
                ("inner", "Host fromInner\n"),
            ],
        );
        assert_eq!(aliases(&hosts), ["inc1", "inc2", "top", "fromInner"]);
        assert_eq!(hosts[0].host_name.as_deref(), Some("inc1.lan"));
        // After an Include, keywords apply to the block around it.
        assert_eq!(hosts[2].host_name.as_deref(), Some("top.lan"));
        assert_eq!(hosts[3].host_name, None);
    }

    #[test]
    fn include_loops_stop() {
        let hosts = parse_with(
            "Include self\nHost x\n",
            &[("self", "Include self\nHost y\n")],
        );
        assert_eq!(aliases(&hosts), ["y", "x"]);
    }

    #[test]
    fn forges_are_dropped() {
        let hosts = parse_with(
            "Host github.com\n User git\nHost gh\n HostName GitHub.com\nHost real\n",
            &[],
        );
        assert_eq!(aliases(&hosts), ["real"]);
    }

    #[test]
    fn wildcards() {
        assert!(wildcard_match("*", "abc"));
        assert!(wildcard_match("*.conf", "a.conf"));
        assert!(!wildcard_match("*.conf", "a.confx"));
        assert!(wildcard_match("a?c", "abc"));
        assert!(!wildcard_match("a?c", "ac"));
    }

    #[test]
    fn fs_loader_globs_relative_and_tilde() {
        let home = tempfile::tempdir().unwrap();
        let ssh = home.path().join(".ssh");
        std::fs::create_dir_all(ssh.join("config.d")).unwrap();
        std::fs::write(ssh.join("config.d/b.conf"), "Host b\n").unwrap();
        std::fs::write(ssh.join("config.d/a.conf"), "Host a\n").unwrap();
        std::fs::write(ssh.join("config.d/.hidden.conf"), "Host hidden\n").unwrap();
        std::fs::write(ssh.join("config.d/skip.txt"), "Host txt\n").unwrap();
        std::fs::write(home.path().join("extra"), "Host extra\n").unwrap();
        std::fs::write(
            ssh.join("config"),
            "Include config.d/*.conf ~/extra missing/*\nHost main\n",
        )
        .unwrap();
        let hosts = read_user_ssh_config(home.path());
        assert_eq!(aliases(&hosts), ["a", "b", "extra", "main"]);
    }

    #[test]
    fn missing_config_is_empty() {
        let home = tempfile::tempdir().unwrap();
        assert!(read_user_ssh_config(home.path()).is_empty());
    }
}
