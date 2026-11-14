//! Codex rollouts: `~/.codex/sessions/**/rollout-*.jsonl`.
//!
//! A port of Heeler's `plugin/src/usage/codex-scanner.js` (Apache-2.0, see
//! `THIRD_PARTY_NOTICES.md`) and the rules in its `docs/design/agent-usage.md`,
//! rewritten in Rust and changed for koolie:
//! - `token_usage_record` (Codex 0.153+): one per response, counted once per
//!   `response_id` globally; a record of another thread (copied fork) is not
//!   counted from this file.
//! - `event_msg` `token_count`: Codex re-sends the cumulative
//!   `total_token_usage` with every rate-limit update. An event whose total
//!   equals the previous advancing total is a repeat and adds nothing.
//!   Otherwise the delta is `last_token_usage` (else total minus previous
//!   total), added only when no own-thread `token_usage_record` arrived since
//!   the last advance (records are written before the `token_count` that
//!   reports them).
//! - Inherited prefixes: a copied fork (`forked_from_id` without
//!   `history_base`) or a subagent (`session_id != id`) starts with the
//!   parent's events. Codex writes that prefix in one burst (lines no more
//!   than 1000 ms apart). `token_count`s in the burst before an own-thread
//!   `thread_settings_applied` are inherited; a subagent counts after an
//!   `inter_agent_communication(_metadata)` with `trigger_turn: true`; a fork
//!   without a marker treats the whole burst as inherited. (Heeler can also
//!   match a fork against its parent file; koolie does not.)
//! - Model: latest `turn_context.model` or `thread_settings.model`. Tier:
//!   latest `thread_settings.service_tier`.
//! - Codex's `input_tokens` includes cached and cache-write tokens, so
//!   non-cached input is `input - cached - cache_write`.

use super::{Count, Tier, UsageRecord, parse_timestamp_ms};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize)]
struct Usage {
    #[serde(default)]
    input_tokens: Count,
    #[serde(default)]
    cached_input_tokens: Count,
    #[serde(default)]
    cache_write_input_tokens: Count,
    #[serde(default)]
    output_tokens: Count,
    #[serde(default)]
    reasoning_output_tokens: Count,
}

impl Usage {
    fn array(&self) -> [u64; 5] {
        [
            self.input_tokens.0,
            self.cached_input_tokens.0,
            self.cache_write_input_tokens.0,
            self.output_tokens.0,
            self.reasoning_output_tokens.0,
        ]
    }
}

#[derive(Deserialize)]
struct Info {
    total_token_usage: Option<Usage>,
    last_token_usage: Option<Usage>,
}

#[derive(Deserialize)]
struct ThreadSettings {
    model: Option<String>,
    service_tier: Option<String>,
}

#[derive(Default, Deserialize)]
struct Payload {
    #[serde(rename = "type")]
    kind: Option<String>,
    id: Option<String>,
    session_id: Option<String>,
    forked_from_id: Option<String>,
    history_base: Option<serde_json::Value>,
    model: Option<String>,
    thread_settings: Option<ThreadSettings>,
    thread_id: Option<String>,
    trigger_turn: Option<serde_json::Value>,
    response_id: Option<String>,
    usage: Option<Usage>,
    info: Option<Info>,
}

#[derive(Deserialize)]
struct Line {
    timestamp: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    payload: Option<Payload>,
}

/// Only the timing of a line, when its payload has unexpected shapes.
#[derive(Deserialize)]
struct Head {
    timestamp: Option<String>,
    payload: Option<serde::de::IgnoredAny>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Inheritance {
    Subagent,
    Fork,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum Pending {
    Trigger,
    Count(Option<UsageRecord>),
}

/// Per-file parser state; cached between scans so appended lines continue
/// where the last scan stopped.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct CodexState {
    thread_id: Option<String>,
    model: String,
    tier: Tier,
    previous_total: Option<[u64; 5]>,
    record_since_advance: bool,
    inheritance: Option<Inheritance>,
    inherited_ended: bool,
    leading_closed: bool,
    leading_last_ms: Option<i64>,
    pending: Vec<Pending>,
}

impl Default for CodexState {
    fn default() -> Self {
        Self {
            thread_id: None,
            model: "unknown".into(),
            tier: Tier::Standard,
            previous_total: None,
            record_since_advance: false,
            inheritance: None,
            inherited_ended: true,
            leading_closed: false,
            leading_last_ms: None,
            pending: Vec::new(),
        }
    }
}

const BURST_GAP_MS: i64 = 1000;

/// Line types the parser acts on; everything else only matters for burst timing.
const RELEVANT: &[&[u8]] = &[
    b"token_count",
    b"token_usage_record",
    b"turn_context",
    b"thread_settings_applied",
    b"inter_agent_communication",
    b"session_meta",
];

fn relevant(line: &[u8]) -> bool {
    RELEVANT
        .iter()
        .any(|p| memchr::memmem::find(line, p).is_some())
}

fn tier_of(service_tier: Option<&str>) -> Tier {
    match service_tier {
        None | Some("default") | Some("standard") => Tier::Standard,
        Some("priority") | Some("fast") => Tier::Fast,
        Some("flex") => Tier::Flex,
        Some(_) => Tier::Unknown,
    }
}

fn truthy(v: &Option<serde_json::Value>) -> bool {
    match v {
        None | Some(serde_json::Value::Null) | Some(serde_json::Value::Bool(false)) => false,
        Some(serde_json::Value::String(s)) => !s.is_empty(),
        Some(serde_json::Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(_) => true,
    }
}

impl CodexState {
    /// Whether a line must be parsed at all.
    pub(crate) fn wants(&self, line: &[u8]) -> bool {
        !self.leading_closed || relevant(line)
    }

    fn contribution(
        &self,
        usage: [u64; 5],
        ts_ms: Option<i64>,
        key: String,
    ) -> Option<UsageRecord> {
        let ts_ms = ts_ms?;
        let [input, cached, write, output, reasoning] = usage;
        let rec = UsageRecord {
            key,
            ts_ms,
            model: self.model.clone(),
            input: input.saturating_sub(cached.saturating_add(write)),
            output,
            cache_read: cached,
            write_5m: write,
            write_1h: 0,
            reasoning,
            tier: self.tier,
            group: None,
            sidechain: false,
        };
        (rec.total() > 0).then_some(rec)
    }

    fn accept(&self, candidate: Option<UsageRecord>) -> Option<UsageRecord> {
        // Without the parent file, a fork's burst is inherited and dropped.
        if self.inherited_ended {
            candidate
        } else {
            None
        }
    }

    fn end_leading_burst(&mut self, out: &mut Vec<UsageRecord>) {
        self.leading_closed = true;
        for p in std::mem::take(&mut self.pending) {
            match p {
                Pending::Trigger => {
                    if self.inheritance == Some(Inheritance::Subagent) {
                        self.inherited_ended = true;
                    }
                }
                Pending::Count(c) => out.extend(self.accept(c)),
            }
        }
        if self.inheritance == Some(Inheritance::Fork) {
            self.inherited_ended = true;
        }
    }

    /// Close a leading burst whose last line is more than a second old.
    pub(crate) fn finish(&mut self, now_ms: i64) -> Vec<UsageRecord> {
        let mut out = Vec::new();
        if !self.leading_closed
            && let Some(last) = self.leading_last_ms
            && now_ms - last > BURST_GAP_MS
        {
            self.end_leading_burst(&mut out);
        }
        out
    }

    /// Consume one complete line. `event_key` identifies the line (file and
    /// byte offset) for usage that has no response id.
    pub(crate) fn consume(
        &mut self,
        raw: &[u8],
        event_key: impl FnOnce() -> String,
    ) -> Vec<UsageRecord> {
        let mut out = Vec::new();
        let (timestamp, kind, payload) = match serde_json::from_slice::<Line>(raw) {
            Ok(Line {
                timestamp,
                kind,
                payload: Some(payload),
            }) => (timestamp, kind, payload),
            Ok(_) => return out,
            Err(_) => match serde_json::from_slice::<Head>(raw) {
                Ok(Head {
                    timestamp,
                    payload: Some(_),
                }) => (timestamp, None, Payload::default()),
                _ => return out,
            },
        };
        let ts_ms = timestamp.as_deref().and_then(parse_timestamp_ms);
        let kind = kind.as_deref().unwrap_or("");
        let ptype = payload.kind.as_deref().unwrap_or("");

        if kind == "session_meta" && self.thread_id.is_none() {
            let Some(id) = payload.id.clone() else {
                return out;
            };
            if !truthy(&payload.history_base) {
                self.inheritance = match (&payload.session_id, &payload.forked_from_id) {
                    (Some(s), _) if *s != id => Some(Inheritance::Subagent),
                    (_, Some(_)) => Some(Inheritance::Fork),
                    _ => None,
                };
                self.inherited_ended = self.inheritance.is_none();
            }
            self.thread_id = Some(id);
        }
        let Some(thread_id) = self.thread_id.clone() else {
            return out;
        };
        if let Some(ts) = ts_ms
            && !self.leading_closed
        {
            match self.leading_last_ms {
                Some(last) if ts - last > BURST_GAP_MS => self.end_leading_burst(&mut out),
                _ => self.leading_last_ms = Some(ts),
            }
        }
        if kind == "turn_context"
            && let Some(m) = &payload.model
        {
            self.model = m.clone();
        }
        if kind == "event_msg" && ptype == "thread_settings_applied" {
            if let Some(m) = payload
                .thread_settings
                .as_ref()
                .and_then(|s| s.model.clone())
            {
                self.model = m;
            }
            self.tier = tier_of(
                payload
                    .thread_settings
                    .as_ref()
                    .and_then(|s| s.service_tier.as_deref()),
            );
            if !self.leading_closed && payload.thread_id.as_deref() == Some(thread_id.as_str()) {
                self.pending.clear();
                self.inherited_ended = true;
            }
        }
        if (kind == "inter_agent_communication_metadata" || kind == "inter_agent_communication")
            && payload.trigger_turn == Some(serde_json::Value::Bool(true))
        {
            if self.leading_closed && self.inheritance == Some(Inheritance::Subagent) {
                self.inherited_ended = true;
            } else if !self.inherited_ended {
                self.pending.push(Pending::Trigger);
            }
        }
        if kind == "token_usage_record"
            && payload.thread_id.as_deref() == Some(thread_id.as_str())
            && let (Some(rid), Some(usage)) = (&payload.response_id, &payload.usage)
        {
            self.record_since_advance = true;
            out.extend(self.contribution(usage.array(), ts_ms, format!("resp\u{1f}{rid}")));
            return out;
        }
        if kind != "event_msg" || ptype != "token_count" {
            return out;
        }
        let Some(info) = &payload.info else {
            return out;
        };
        let Some(total) = info.total_token_usage.map(|u| u.array()) else {
            return out;
        };
        let repeated = self.previous_total == Some(total);
        let delta = match info.last_token_usage {
            Some(last) => last.array(),
            None => {
                let prev = self.previous_total.unwrap_or_default();
                std::array::from_fn(|i| total[i].saturating_sub(prev[i]))
            }
        };
        let already_counted = self.record_since_advance;
        if !repeated {
            self.previous_total = Some(total);
            self.record_since_advance = false;
        }
        let candidate = if repeated || already_counted {
            None
        } else {
            self.contribution(
                delta,
                ts_ms,
                format!("event\u{1f}{thread_id}\u{1f}{}", event_key()),
            )
        };
        if !self.inherited_ended && !self.leading_closed {
            self.pending.push(Pending::Count(candidate));
        } else {
            out.extend(self.accept(candidate));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(lines: &[String], now_ms: i64) -> Vec<UsageRecord> {
        let mut s = CodexState::default();
        let mut out = Vec::new();
        for (i, l) in lines.iter().enumerate() {
            if s.wants(l.as_bytes()) {
                out.extend(s.consume(l.as_bytes(), || i.to_string()));
            }
        }
        out.extend(s.finish(now_ms));
        out
    }

    fn usage(input: u64, cached: u64, output: u64) -> String {
        format!(
            r#"{{"input_tokens":{input},"cached_input_tokens":{cached},"cache_write_input_tokens":0,"output_tokens":{output},"reasoning_output_tokens":1,"total_tokens":{}}}"#,
            input + output
        )
    }

    fn ts(sec: u32) -> String {
        format!("2026-11-01T00:{:02}:{:02}.000Z", sec / 60, sec % 60)
    }

    fn meta(sec: u32, id: &str, session: &str) -> String {
        format!(
            r#"{{"timestamp":"{}","type":"session_meta","payload":{{"id":"{id}","session_id":"{session}","cwd":"/x"}}}}"#,
            ts(sec)
        )
    }

    fn turn(sec: u32, model: &str) -> String {
        format!(
            r#"{{"timestamp":"{}","type":"turn_context","payload":{{"model":"{model}"}}}}"#,
            ts(sec)
        )
    }

    fn count(sec: u32, total: &str, last: Option<&str>) -> String {
        let last = last
            .map(|l| format!(r#","last_token_usage":{l}"#))
            .unwrap_or_default();
        format!(
            r#"{{"timestamp":"{}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{total}{last}}},"rate_limits":{{}}}}}}"#,
            ts(sec)
        )
    }

    fn record(sec: u32, thread: &str, resp: &str, u: &str) -> String {
        format!(
            r#"{{"timestamp":"{}","type":"token_usage_record","payload":{{"thread_id":"{thread}","response_id":"{resp}","usage":{u}}}}}"#,
            ts(sec)
        )
    }

    const NOW: i64 = 1_900_000_000_000;

    #[test]
    fn token_count_without_double_counting() {
        let lines = vec![
            meta(0, "t", "t"),
            turn(5, "gpt-5.4"),
            count(10, &usage(100, 40, 10), Some(&usage(100, 40, 10))),
            // Repeat (rate-limit update): nothing.
            count(11, &usage(100, 40, 10), Some(&usage(100, 40, 10))),
            // Advance without last_token_usage: delta from totals.
            count(20, &usage(250, 90, 30), None),
        ];
        let r = run(&lines, NOW);
        assert_eq!(r.len(), 2, "{r:?}");
        assert_eq!((r[0].input, r[0].cache_read, r[0].output), (60, 40, 10));
        assert_eq!((r[1].input, r[1].cache_read, r[1].output), (100, 50, 20));
        assert_eq!(r[0].model, "gpt-5.4");
    }

    #[test]
    fn records_suppress_their_token_count() {
        let lines = vec![
            meta(0, "t", "t"),
            turn(5, "gpt-5.6-sol"),
            record(10, "t", "r1", &usage(100, 40, 10)),
            count(10, &usage(100, 40, 10), Some(&usage(100, 40, 10))),
            // A record copied from another thread is not counted.
            record(20, "other", "r0", &usage(999, 0, 9)),
            count(21, &usage(300, 40, 20), Some(&usage(200, 0, 10))),
        ];
        let r = run(&lines, NOW);
        assert_eq!(r.len(), 2, "{r:?}");
        assert_eq!(r[0].key, "resp\u{1f}r1");
        assert_eq!(r[1].input, 200);
    }

    #[test]
    fn subagent_prefix_is_inherited_until_trigger() {
        let trigger = format!(
            r#"{{"timestamp":"{}","type":"inter_agent_communication_metadata","payload":{{"trigger_turn":true}}}}"#,
            ts(30)
        );
        let lines = vec![
            meta(0, "child", "parent"),
            // Copied burst.
            count(0, &usage(500, 0, 50), Some(&usage(500, 0, 50))),
            turn(30, "gpt-5.4"),
            trigger,
            count(40, &usage(600, 0, 60), Some(&usage(100, 0, 10))),
        ];
        let r = run(&lines, NOW);
        assert_eq!(r.len(), 1, "{r:?}");
        assert_eq!(r[0].input, 100);
    }

    #[test]
    fn fork_burst_is_inherited() {
        let fork_meta = format!(
            r#"{{"timestamp":"{}","type":"session_meta","payload":{{"id":"f","forked_from_id":"p"}}}}"#,
            ts(0)
        );
        let lines = vec![
            fork_meta,
            count(0, &usage(500, 0, 50), Some(&usage(500, 0, 50))),
            count(30, &usage(600, 0, 60), Some(&usage(100, 0, 10))),
        ];
        let r = run(&lines, NOW);
        assert_eq!(r.len(), 1, "{r:?}");
        assert_eq!(r[0].input, 100);
    }

    #[test]
    fn service_tier() {
        let tsa = format!(
            r#"{{"timestamp":"{}","type":"event_msg","payload":{{"type":"thread_settings_applied","thread_settings":{{"model":"gpt-5.5","service_tier":"priority"}}}}}}"#,
            ts(1)
        );
        let lines = vec![meta(0, "t", "t"), tsa, count(10, &usage(10, 0, 1), None)];
        let r = run(&lines, NOW);
        assert_eq!(r[0].tier, Tier::Fast);
        assert_eq!(r[0].model, "gpt-5.5");
    }
}
