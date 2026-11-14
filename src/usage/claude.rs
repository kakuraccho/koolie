//! Claude Code transcripts: `~/.claude/projects/**/*.jsonl`.
//!
//! Counting rules (after Heeler's `docs/design/agent-usage.md`):
//! - `type: "assistant"` lines with `message.usage`; `<synthetic>` is skipped.
//! - One response is written as several lines (one per content block) that
//!   repeat `message.id`, `requestId` and `usage`; resumed and forked sessions
//!   copy them into other files. The dedupe key is `message.id + requestId`
//!   (without `requestId`: `message.id + sessionId + timestamp`), global
//!   across files. See [`super::dedupe_claude`] for collisions.
//! - Cache writes are `cache_creation.ephemeral_5m/1h_input_tokens`; without
//!   the split, all of `cache_creation_input_tokens` counts as 5-minute writes.

use super::{Count, Tier, UsageRecord, parse_timestamp_ms};
use serde::Deserialize;

#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    #[serde(rename = "isSidechain")]
    is_sidechain: Option<bool>,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    id: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Usage {
    #[serde(default)]
    input_tokens: Count,
    #[serde(default)]
    output_tokens: Count,
    #[serde(default)]
    cache_creation_input_tokens: Count,
    #[serde(default)]
    cache_read_input_tokens: Count,
    cache_creation: Option<CacheCreation>,
    speed: Option<String>,
}

#[derive(Deserialize)]
struct CacheCreation {
    ephemeral_5m_input_tokens: Option<Count>,
    ephemeral_1h_input_tokens: Option<Count>,
}

/// Cheap check before a JSON parse: only assistant lines with usage count.
pub(crate) fn may_count(line: &[u8]) -> bool {
    memchr::memmem::find(line, b"\"usage\"").is_some()
        && memchr::memmem::find(line, b"\"assistant\"").is_some()
}

/// The usage of one transcript line, or `None` when it carries none.
pub(crate) fn parse_line(line: &[u8]) -> Option<UsageRecord> {
    if !may_count(line) {
        return None;
    }
    let line: Line = serde_json::from_slice(line).ok()?;
    if line.kind.as_deref() != Some("assistant") {
        return None;
    }
    let message = line.message?;
    let usage = message.usage?;
    let model = message.model.unwrap_or_else(|| "unknown".into());
    if model == "<synthetic>" {
        return None;
    }
    let id = message.id?;
    let timestamp = line.timestamp?;
    let ts_ms = parse_timestamp_ms(&timestamp)?;
    let (write_5m, write_1h) = match &usage.cache_creation {
        Some(c)
            if c.ephemeral_5m_input_tokens.is_some() || c.ephemeral_1h_input_tokens.is_some() =>
        {
            (
                c.ephemeral_5m_input_tokens.unwrap_or_default().0,
                c.ephemeral_1h_input_tokens.unwrap_or_default().0,
            )
        }
        _ => (usage.cache_creation_input_tokens.0, 0),
    };
    let session = line.session_id.unwrap_or_default();
    let key = match line.request_id.filter(|r| !r.is_empty()) {
        Some(req) => format!("{id}\u{1f}{req}"),
        None => format!("{id}\u{1f}{session}\u{1f}{timestamp}"),
    };
    Some(UsageRecord {
        group: Some(format!("{id}\u{1f}{session}")),
        key,
        ts_ms,
        model,
        input: usage.input_tokens.0,
        output: usage.output_tokens.0,
        cache_read: usage.cache_read_input_tokens.0,
        write_5m,
        write_1h,
        reasoning: 0,
        tier: if usage.speed.as_deref() == Some("fast") {
            Tier::Fast
        } else {
            Tier::Standard
        },
        sidechain: line.is_sidechain == Some(true),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_usage_and_split() {
        let l = br#"{"type":"assistant","timestamp":"2026-11-01T00:00:00.000Z","sessionId":"s","requestId":"r","message":{"id":"m","model":"claude-opus-5-5","usage":{"input_tokens":3,"output_tokens":5,"cache_creation_input_tokens":30,"cache_read_input_tokens":7,"cache_creation":{"ephemeral_5m_input_tokens":10,"ephemeral_1h_input_tokens":20},"speed":"fast","unknown":{"x":1}}},"extra":1}"#;
        let r = parse_line(l).unwrap();
        assert_eq!(
            (r.input, r.output, r.cache_read, r.write_5m, r.write_1h),
            (3, 5, 7, 10, 20)
        );
        assert_eq!(r.tier, Tier::Fast);
        assert_eq!(r.key, "m\u{1f}r");
        assert!(!r.sidechain);
    }

    #[test]
    fn without_split_all_writes_are_5m() {
        let l = br#"{"type":"assistant","timestamp":"2026-11-01T00:00:00Z","sessionId":"s","message":{"id":"m","model":"x","usage":{"input_tokens":1,"cache_creation_input_tokens":30}}}"#;
        let r = parse_line(l).unwrap();
        assert_eq!((r.write_5m, r.write_1h), (30, 0));
        assert_eq!(r.key, "m\u{1f}s\u{1f}2026-11-01T00:00:00Z");
    }

    #[test]
    fn skips_non_usage() {
        let synthetic = br#"{"type":"assistant","timestamp":"2026-11-01T00:00:00Z","message":{"id":"m","model":"<synthetic>","usage":{"input_tokens":1}}}"#;
        assert!(parse_line(synthetic).is_none());
        let user = br#"{"type":"user","timestamp":"2026-11-01T00:00:00Z","message":{"id":"m","usage":{"input_tokens":1}},"x":"assistant"}"#;
        assert!(parse_line(user).is_none());
        assert!(parse_line(b"not json \"usage\" \"assistant\"").is_none());
    }
}
