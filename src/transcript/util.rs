//! Helpers shared by the transcript formats: text limits, one-line
//! summaries, timestamps and tag extraction.

use serde_json::{Map, Value};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

pub(super) type Obj = Map<String, Value>;

/// Longest prompt, answer or intermediate text kept, in characters.
pub(super) const MAX_TEXT_CHARS: usize = 20_000;
/// Longest tool summary kept, in characters.
pub(super) const MAX_SUMMARY_CHARS: usize = 200;

/// Trim `text` and cut it at [`MAX_TEXT_CHARS`], saying how much was dropped.
pub(super) fn clip_text(text: &str) -> String {
    let text = text.trim();
    match text.char_indices().nth(MAX_TEXT_CHARS) {
        None => text.to_string(),
        Some((idx, _)) => {
            let rest = text[idx..].chars().count();
            format!(
                "{}\n… [truncated, {rest} more characters]",
                text[..idx].trim_end()
            )
        }
    }
}

/// Collapse `text` to one line of at most `max` characters; whitespace runs
/// become one space, control characters are dropped and a cut ends in `…`.
/// Stops reading `text` once the limit is reached.
pub(super) fn one_line(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut n = 0usize;
    for word in text.split_whitespace() {
        let mut first = true;
        for c in word.chars().filter(|c| !c.is_control()) {
            if first && n > 0 {
                out.push(' ');
                n += 1;
            }
            first = false;
            if n >= max {
                out.truncate(out.trim_end().len());
                out.push('…');
                return out;
            }
            out.push(c);
            n += 1;
        }
    }
    out
}

/// A transcript timestamp as unix seconds: an RFC 3339 string, or a number
/// of seconds or milliseconds.
pub(super) fn parse_ts(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::String(s) => OffsetDateTime::parse(s, &Rfc3339)
            .ok()
            .map(OffsetDateTime::unix_timestamp),
        Value::Number(n) => {
            let x = n.as_i64().or_else(|| n.as_f64().map(|f| f as i64))?;
            // Anything this large is milliseconds (seconds reach 1e11 in the year 5138).
            Some(if x.abs() >= 100_000_000_000 {
                x / 1000
            } else {
                x
            })
        }
        _ => None,
    }
}

/// The text between the first `<tag>` and its `</tag>` (or the end of `text`
/// when the closing tag is missing).
pub(super) fn tag_content<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let start = text.find(&open)? + open.len();
    let rest = &text[start..];
    let close = format!("</{tag}>");
    Some(match rest.find(&close) {
        Some(end) => &rest[..end],
        None => rest,
    })
}

/// Remove ANSI escape sequences (CSI and OSC) from `s`.
pub(super) fn strip_ansi(s: &str) -> String {
    const ESC: char = '\u{1b}';
    if !s.contains(ESC) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != ESC {
            out.push(c);
            continue;
        }
        match chars.next() {
            // CSI: parameters, then one final byte in @..~
            Some('[') => {
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            // OSC: ends with BEL or ESC \
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == ESC {
                        chars.next();
                        break;
                    }
                }
            }
            // Two-character escape, or a lone ESC at the end.
            _ => {}
        }
    }
    out
}

pub(super) fn flag(obj: &Obj, key: &str) -> bool {
    obj.get(key).and_then(Value::as_bool).unwrap_or(false)
}

pub(super) fn str_of<'a>(obj: &'a Obj, key: &str) -> Option<&'a str> {
    obj.get(key).and_then(Value::as_str)
}

/// A non-blank string field.
pub(super) fn text_of<'a>(obj: &'a Obj, key: &str) -> Option<&'a str> {
    str_of(obj, key).filter(|s| !s.trim().is_empty())
}

/// A command as the user would type it: a string as is, and an argv joined
/// with spaces, except that `[sh|bash|zsh, -c|-lc, script]` gives `script`.
pub(super) fn command_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        Value::Array(items) => {
            let argv: Vec<&str> = items.iter().filter_map(Value::as_str).collect();
            if argv.is_empty() {
                return None;
            }
            if let [shell, opt, script] = argv.as_slice() {
                let shell = shell.rsplit('/').next().unwrap_or(shell);
                if matches!(shell, "sh" | "bash" | "zsh") && matches!(*opt, "-lc" | "-c") {
                    return Some((*script).to_string());
                }
            }
            Some(argv.join(" "))
        }
        _ => None,
    }
}

/// Input fields that say best what a tool call is about, most telling first.
const SUMMARY_KEYS: &[&str] = &[
    "command",
    "cmd",
    "file_path",
    "notebook_path",
    "path",
    "pattern",
    "query",
    "url",
    "description",
    "prompt",
    "skill",
    "subject",
    "title",
    "task_name",
    "name",
    "message",
    "text",
];

/// One line saying what a tool input is about, for tools without a rule of
/// their own.
pub(super) fn summarize_value(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => one_line(s, MAX_SUMMARY_CHARS),
        Value::Object(map) => {
            for key in SUMMARY_KEYS {
                if let Some(text) = map.get(*key).and_then(command_text) {
                    return one_line(&text, MAX_SUMMARY_CHARS);
                }
            }
            for value in map.values() {
                if let Value::String(s) = value
                    && !s.trim().is_empty()
                {
                    return one_line(s, MAX_SUMMARY_CHARS);
                }
            }
            if map.is_empty() {
                String::new()
            } else {
                one_line(&v.to_string(), MAX_SUMMARY_CHARS)
            }
        }
        other => one_line(&other.to_string(), MAX_SUMMARY_CHARS),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn clip_text_marks_what_it_dropped() {
        assert_eq!(clip_text("  short \n"), "short");
        let long = "あ".repeat(MAX_TEXT_CHARS + 5);
        let clipped = clip_text(&long);
        assert!(clipped.starts_with("あああ"));
        assert!(clipped.ends_with("… [truncated, 5 more characters]"));
        assert!(clipped.chars().count() < MAX_TEXT_CHARS + 60);
    }

    #[test]
    fn one_line_collapses_and_cuts() {
        assert_eq!(one_line("  a\n\tb   c ", 50), "a b c");
        assert_eq!(one_line("abcdef", 3), "abc…");
        assert_eq!(one_line("ab cdef", 3), "ab…");
        assert_eq!(one_line("a\u{1b}b", 9), "ab");
        assert_eq!(one_line("", 9), "");
    }

    #[test]
    fn timestamps() {
        assert_eq!(
            parse_ts(Some(&json!("2026-01-02T03:04:05.678Z"))),
            Some(1_767_323_045)
        );
        assert_eq!(
            parse_ts(Some(&json!("2026-01-02T12:04:05+09:00"))),
            Some(1_767_323_045)
        );
        assert_eq!(parse_ts(Some(&json!(1_767_323_045))), Some(1_767_323_045));
        assert_eq!(
            parse_ts(Some(&json!(1_767_323_045_678_i64))),
            Some(1_767_323_045)
        );
        assert_eq!(parse_ts(Some(&json!("yesterday"))), None);
        assert_eq!(parse_ts(Some(&json!({}))), None);
        assert_eq!(parse_ts(None), None);
    }

    #[test]
    fn tags_and_ansi() {
        assert_eq!(tag_content("<a>x</a><b>y</b>", "b"), Some("y"));
        assert_eq!(tag_content("<a>open", "a"), Some("open"));
        assert_eq!(tag_content("none", "a"), None);
        assert_eq!(
            strip_ansi("\u{1b}[1;32mok\u{1b}[0m \u{1b}]0;t\u{7}done\u{1b}"),
            "ok done"
        );
    }

    #[test]
    fn commands_and_summaries() {
        assert_eq!(
            command_text(&json!(["/bin/bash", "-lc", "ls -la"])).as_deref(),
            Some("ls -la")
        );
        assert_eq!(
            command_text(&json!(["git", "status"])).as_deref(),
            Some("git status")
        );
        assert_eq!(command_text(&json!(7)), None);
        assert_eq!(
            summarize_value(&json!({"zzz": 1, "url": "https://example.com"})),
            "https://example.com"
        );
        assert_eq!(summarize_value(&json!({"b": 2, "a": "first"})), "first");
        assert_eq!(summarize_value(&json!({"n": 3})), "{\"n\":3}");
        assert_eq!(summarize_value(&json!({})), "");
    }
}
