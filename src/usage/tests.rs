use super::*;
use std::fs;
use std::io::Write as _;
use time::macros::offset;

fn ms(ts: &str) -> i64 {
    parse_timestamp_ms(ts).unwrap()
}

fn now() -> i64 {
    ms("2026-11-14T12:00:00Z")
}

/// Write `content` and set the mtime.
fn put(path: &Path, content: &str, mtime_ms: i64) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
    touch(path, mtime_ms);
}

fn append(path: &Path, content: &str, mtime_ms: i64) {
    let mut f = fs::OpenOptions::new().append(true).open(path).unwrap();
    f.write_all(content.as_bytes()).unwrap();
    drop(f);
    touch(path, mtime_ms);
}

fn touch(path: &Path, mtime_ms: i64) {
    let t = std::time::UNIX_EPOCH + std::time::Duration::from_millis(mtime_ms as u64);
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

struct Claude<'a> {
    ts: &'a str,
    session: &'a str,
    msg: &'a str,
    req: Option<&'a str>,
    model: &'a str,
    input: u64,
    output: u64,
    sidechain: bool,
}

impl Default for Claude<'_> {
    fn default() -> Self {
        Self {
            ts: "2026-11-14T10:00:00.000Z",
            session: "s1",
            msg: "msg_1",
            req: Some("req_1"),
            model: "claude-opus-5-5",
            input: 10,
            output: 100,
            sidechain: false,
        }
    }
}

impl Claude<'_> {
    fn line(&self) -> String {
        let req = self
            .req
            .map(|r| format!(r#""requestId":"{r}","#))
            .unwrap_or_default();
        format!(
            r#"{{"parentUuid":null,"isSidechain":{},"type":"assistant","timestamp":"{}","sessionId":"{}",{req}"message":{{"id":"{}","type":"message","role":"assistant","model":"{}","content":[{{"type":"text","text":"hi"}}],"usage":{{"input_tokens":{},"cache_creation_input_tokens":1000,"cache_read_input_tokens":2000,"output_tokens":{},"cache_creation":{{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":1000}}}}}}}}"#,
            self.sidechain, self.ts, self.session, self.msg, self.model, self.input, self.output
        ) + "\n"
    }
}

fn claude_path(home: &Path, rel: &str) -> PathBuf {
    home.join(".claude/projects/-home-u-proj").join(rel)
}

fn scan(home: &Path, days: u32, off: UtcOffset, cache: Option<&Path>) -> UsageReport {
    scan_at(home, days, off, cache, now())
}

fn totals(r: &UsageReport, program: &str) -> (u64, u64, u64, u64, u64) {
    r.rows
        .iter()
        .filter(|x| x.program == program)
        .fold((0, 0, 0, 0, 0), |a, x| {
            (
                a.0 + x.input_tokens,
                a.1 + x.output_tokens,
                a.2 + x.cache_read_tokens,
                a.3 + x.cache_write_tokens,
                a.4 + x.requests,
            )
        })
}

#[test]
fn claude_multi_line_dedupe_keeps_largest() {
    let home = tempfile::tempdir().unwrap();
    let base = Claude::default();
    let content = Claude {
        output: 1,
        ..Claude::default()
    }
    .line()
        + &base.line()
        + &Claude {
            output: 50,
            ..Claude::default()
        }
        .line()
        + r#"{"type":"user","message":{"role":"user","content":"x"}}"#
        + "\n";
    put(
        &claude_path(home.path(), "s1.jsonl"),
        &content,
        now() - 1000,
    );
    let r = scan(home.path(), 30, UtcOffset::UTC, None);
    assert_eq!(totals(&r, "claude"), (10, 100, 2000, 1000, 1));
}

#[test]
fn claude_cross_file_dedupe() {
    let home = tempfile::tempdir().unwrap();
    let a = Claude::default();
    let b = Claude {
        msg: "msg_2",
        req: Some("req_2"),
        ..Claude::default()
    };
    // A resumed session copies the history into a new file with a new sessionId.
    let copied = Claude {
        session: "s2",
        ..Claude::default()
    };
    let c = Claude {
        session: "s2",
        msg: "msg_3",
        req: Some("req_3"),
        ..Claude::default()
    };
    put(
        &claude_path(home.path(), "s1.jsonl"),
        &(a.line() + &b.line()),
        now() - 1000,
    );
    put(
        &claude_path(home.path(), "s2.jsonl"),
        &(copied.line() + &c.line()),
        now() - 1000,
    );
    let r = scan(home.path(), 30, UtcOffset::UTC, None);
    assert_eq!(totals(&r, "claude").4, 3);
}

#[test]
fn subagent_files_counted_and_replays_dropped() {
    let home = tempfile::tempdir().unwrap();
    let parent = Claude::default();
    put(
        &claude_path(home.path(), "s1.jsonl"),
        &parent.line(),
        now() - 1000,
    );
    let own = Claude {
        msg: "msg_sub",
        req: Some("req_sub"),
        sidechain: true,
        model: "claude-fable-5-2",
        ..Claude::default()
    };
    // A sidechain replay of the parent message under another requestId.
    let replay = Claude {
        req: Some("req_replay"),
        sidechain: true,
        ..Claude::default()
    };
    // A sidechain copy with the parent's own key loses to the parent.
    let copy = Claude {
        sidechain: true,
        output: 999,
        ..Claude::default()
    };
    put(
        &claude_path(home.path(), "s1/subagents/agent-a1.jsonl"),
        &(own.line() + &replay.line() + &copy.line()),
        now() - 1000,
    );
    let r = scan(home.path(), 30, UtcOffset::UTC, None);
    let models: Vec<_> = r
        .rows
        .iter()
        .map(|x| (x.model.as_str(), x.requests, x.output_tokens))
        .collect();
    assert_eq!(
        models,
        vec![("claude-fable-5-2", 1, 100), ("claude-opus-5-5", 1, 100)]
    );
}

#[test]
fn synthetic_model_skipped() {
    let home = tempfile::tempdir().unwrap();
    let s = Claude {
        model: "<synthetic>",
        ..Claude::default()
    };
    put(
        &claude_path(home.path(), "s1.jsonl"),
        &s.line(),
        now() - 1000,
    );
    let r = scan(home.path(), 30, UtcOffset::UTC, None);
    assert!(r.rows.is_empty());
}

fn codex_usage(input: u64, cached: u64, output: u64, reasoning: u64) -> String {
    format!(
        r#"{{"input_tokens":{input},"cached_input_tokens":{cached},"cache_write_input_tokens":0,"output_tokens":{output},"reasoning_output_tokens":{reasoning},"total_tokens":{}}}"#,
        input + output
    )
}

fn codex_rollout() -> String {
    let count = |ts: &str, total: String, last: String| {
        format!(
            r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{total},"last_token_usage":{last},"model_context_window":258400}},"rate_limits":{{"primary":{{"used_percent":1.0}}}}}}}}"#
        ) + "\n"
    };
    [
        r#"{"timestamp":"2026-11-14T09:00:00.000Z","type":"session_meta","payload":{"id":"t1","session_id":"t1","cwd":"/w","cli_version":"0.149.0","base_instructions":{"text":"..."}}}"#.to_string() + "\n",
        r#"{"timestamp":"2026-11-14T09:00:05.000Z","type":"turn_context","payload":{"cwd":"/w","model":"gpt-5.4"}}"#.to_string() + "\n",
        r#"{"timestamp":"2026-11-14T09:00:06.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"token_count"}]}}"#.to_string() + "\n",
        count("2026-11-14T09:00:10.000Z", codex_usage(1000, 400, 100, 30), codex_usage(1000, 400, 100, 30)),
        // Rate-limit update: same total again.
        count("2026-11-14T09:00:11.000Z", codex_usage(1000, 400, 100, 30), codex_usage(1000, 400, 100, 30)),
        count("2026-11-14T09:00:20.000Z", codex_usage(3000, 1400, 300, 50), codex_usage(2000, 1000, 200, 20)),
        count("2026-11-14T09:00:21.000Z", codex_usage(3000, 1400, 300, 50), codex_usage(2000, 1000, 200, 20)),
    ]
    .concat()
}

fn codex_path(home: &Path) -> PathBuf {
    home.join(".codex/sessions/2026/11/14/rollout-2026-11-14T09-00-00-t1.jsonl")
}

#[test]
fn codex_token_count_without_double_counting() {
    let home = tempfile::tempdir().unwrap();
    put(&codex_path(home.path()), &codex_rollout(), now() - 1000);
    let r = scan(home.path(), 30, UtcOffset::UTC, None);
    // input 3000 of which 1400 cached → 1600 uncached.
    assert_eq!(totals(&r, "codex"), (1600, 300, 1400, 0, 2));
    let row = &r.rows[0];
    assert_eq!((row.model.as_str(), row.reasoning_tokens), ("gpt-5.4", 50));
    // gpt-5.4: $2.5 in, $0.25 cached, $15 out per MTok.
    let expected = (1600.0 * 2.5 + 1400.0 * 0.25 + 300.0 * 15.0) / 1e6;
    assert!((row.cost_usd.unwrap() - expected).abs() < 1e-12);
}

#[test]
fn local_date_bucketing_across_midnight() {
    let home = tempfile::tempdir().unwrap();
    let before = Claude {
        ts: "2026-11-13T14:59:59.000Z",
        ..Claude::default()
    };
    let after = Claude {
        ts: "2026-11-13T15:00:00.000Z",
        msg: "msg_2",
        req: Some("req_2"),
        ..Claude::default()
    };
    put(
        &claude_path(home.path(), "s1.jsonl"),
        &(before.line() + &after.line()),
        now() - 1000,
    );
    let dates = |off| {
        scan(home.path(), 30, off, None)
            .rows
            .iter()
            .map(|r| (r.date.clone(), r.requests))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        dates(offset!(+9)),
        vec![("2026-11-13".into(), 1), ("2026-11-14".into(), 1)]
    );
    assert_eq!(dates(UtcOffset::UTC), vec![("2026-11-13".into(), 2)]);
    assert_eq!(dates(offset!(-5)), vec![("2026-11-13".into(), 2)]);
    let r = scan(home.path(), 30, offset!(+9), None);
    assert_eq!(r.utc_offset_seconds, 9 * 3600);
}

#[test]
fn window_cutoff() {
    let home = tempfile::tempdir().unwrap();
    // At +09:00, now is 2026-11-14 21:00; a 2-day window starts 2026-11-13 00:00 local
    // = 2026-11-12T15:00Z.
    let inside = Claude {
        ts: "2026-11-12T15:00:00.000Z",
        ..Claude::default()
    };
    let outside = Claude {
        ts: "2026-11-12T14:59:59.999Z",
        msg: "msg_2",
        req: Some("req_2"),
        ..Claude::default()
    };
    put(
        &claude_path(home.path(), "s1.jsonl"),
        &(inside.line() + &outside.line()),
        now() - 1000,
    );
    // A file not modified inside the window is not read at all.
    let old = Claude {
        ts: "2026-11-01T00:00:00.000Z",
        msg: "msg_3",
        req: Some("req_3"),
        ..Claude::default()
    };
    put(
        &claude_path(home.path(), "old.jsonl"),
        &old.line(),
        ms("2026-11-01T00:00:01Z"),
    );
    let r = scan(home.path(), 2, offset!(+9), None);
    assert_eq!(r.rows.len(), 1);
    assert_eq!(
        (r.rows[0].date.as_str(), r.rows[0].requests),
        ("2026-11-13", 1)
    );
    assert_eq!(r.stats.files, 1);
    let r = scan(home.path(), 30, offset!(+9), None);
    assert_eq!(r.rows.iter().map(|r| r.requests).sum::<u64>(), 3);
}

#[test]
fn unknown_model_has_no_cost() {
    let home = tempfile::tempdir().unwrap();
    let a = Claude {
        model: "claude-imaginary-9",
        ..Claude::default()
    };
    let b = Claude {
        msg: "msg_2",
        req: Some("req_2"),
        model: "claude-sonnet-5-20260101",
        ..Claude::default()
    };
    put(
        &claude_path(home.path(), "s1.jsonl"),
        &(a.line() + &b.line()),
        now() - 1000,
    );
    let r = scan(home.path(), 30, UtcOffset::UTC, None);
    let unknown = r
        .rows
        .iter()
        .find(|r| r.model == "claude-imaginary-9")
        .unwrap();
    assert_eq!(unknown.cost_usd, None);
    assert_eq!(unknown.unpriced_tokens, unknown.total_tokens());
    let known = r
        .rows
        .iter()
        .find(|r| r.model == "claude-sonnet-5-20260101")
        .unwrap();
    // sonnet-5: $2 in, $0.2 read, 1h write = 2 × $2, $10 out per MTok.
    let expected = (10.0 * 2.0 + 2000.0 * 0.2 + 1000.0 * 4.0 + 100.0 * 10.0) / 1e6;
    assert!((known.cost_usd.unwrap() - expected).abs() < 1e-12);
    assert_eq!(known.unpriced_tokens, 0);
    let s = summarize(&r);
    assert_eq!(s.overall.today.unpriced_tokens, unknown.total_tokens());
}

#[test]
fn cache_second_scan_reads_nothing() {
    let home = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    put(
        &claude_path(home.path(), "s1.jsonl"),
        &Claude::default().line(),
        now() - 1000,
    );
    put(&codex_path(home.path()), &codex_rollout(), now() - 1000);
    let first = scan(home.path(), 30, UtcOffset::UTC, Some(cache.path()));
    assert_eq!(first.stats.cache, CacheStatus::Rebuilt);
    assert_eq!(first.stats.files_read, 2);
    let second = scan(home.path(), 30, UtcOffset::UTC, Some(cache.path()));
    assert_eq!(second.stats.cache, CacheStatus::Loaded);
    assert_eq!(
        (
            second.stats.files,
            second.stats.files_read,
            second.stats.bytes_read
        ),
        (2, 0, 0)
    );
    assert_eq!(first.rows, second.rows);
}

#[test]
fn cache_appended_file_reads_only_tail() {
    let home = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let path = claude_path(home.path(), "s1.jsonl");
    let first_line = Claude::default().line();
    put(&path, &first_line, now() - 5000);
    scan(home.path(), 30, UtcOffset::UTC, Some(cache.path()));

    // A copy of the first message (another content block) and a new one,
    // the latter still being written (no newline yet).
    let second = Claude {
        msg: "msg_2",
        req: Some("req_2"),
        ..Claude::default()
    }
    .line();
    let (head, tail) = second.split_at(40);
    let more = first_line.clone() + head;
    append(&path, &more, now() - 4000);
    let r = scan(home.path(), 30, UtcOffset::UTC, Some(cache.path()));
    assert_eq!(r.stats.bytes_read, first_line.len() as u64);
    assert_eq!(totals(&r, "claude").4, 1);

    append(&path, tail, now() - 3000);
    let r = scan(home.path(), 30, UtcOffset::UTC, Some(cache.path()));
    assert_eq!(r.stats.bytes_read, second.len() as u64);
    assert_eq!(totals(&r, "claude").4, 2);
    // Same result as a scan without a cache.
    assert_eq!(r.rows, scan(home.path(), 30, UtcOffset::UTC, None).rows);
}

#[test]
fn cache_codex_append_continues_state() {
    let home = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let path = codex_path(home.path());
    let all = codex_rollout();
    let lines: Vec<&str> = all.split_inclusive('\n').collect();
    put(&path, &lines[..5].concat(), now() - 5000);
    scan(home.path(), 30, UtcOffset::UTC, Some(cache.path()));
    append(&path, &lines[5..].concat(), now() - 4000);
    let r = scan(home.path(), 30, UtcOffset::UTC, Some(cache.path()));
    assert_eq!(r.stats.bytes_read, lines[5..].concat().len() as u64);
    assert_eq!(totals(&r, "codex"), (1600, 300, 1400, 0, 2));
}

#[test]
fn cache_rewritten_file_is_reread() {
    let home = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let path = claude_path(home.path(), "s1.jsonl");
    put(
        &path,
        &(Claude::default().line()
            + &Claude {
                msg: "m2",
                ..Claude::default()
            }
            .line()),
        now() - 5000,
    );
    scan(home.path(), 30, UtcOffset::UTC, Some(cache.path()));
    put(
        &path,
        &Claude {
            model: "claude-sonnet-5",
            ..Claude::default()
        }
        .line(),
        now() - 4000,
    );
    let r = scan(home.path(), 30, UtcOffset::UTC, Some(cache.path()));
    assert_eq!(r.rows.len(), 1);
    assert_eq!(r.rows[0].model, "claude-sonnet-5");
    // Deleted files leave the totals.
    fs::remove_file(&path).unwrap();
    let r = scan(home.path(), 30, UtcOffset::UTC, Some(cache.path()));
    assert!(r.rows.is_empty());
}

#[test]
fn corrupted_cache_is_rebuilt() {
    let home = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    put(
        &claude_path(home.path(), "s1.jsonl"),
        &Claude::default().line(),
        now() - 1000,
    );
    let first = scan(home.path(), 30, UtcOffset::UTC, Some(cache.path()));
    let file = cache::path(cache.path(), home.path());
    assert!(file.exists());
    fs::write(&file, b"{\"format\":1,\"files\":[garbage").unwrap();
    let r = scan(home.path(), 30, UtcOffset::UTC, Some(cache.path()));
    assert_eq!(r.stats.cache, CacheStatus::Rebuilt);
    assert_eq!(r.stats.files_read, 1);
    assert_eq!(r.rows, first.rows);
    let r = scan(home.path(), 30, UtcOffset::UTC, Some(cache.path()));
    assert_eq!(r.stats.cache, CacheStatus::Loaded);
}

#[test]
fn wider_window_than_cache_rebuilds() {
    let home = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let old = Claude {
        ts: "2026-09-01T00:00:00.000Z",
        ..Claude::default()
    };
    let new = Claude {
        msg: "m2",
        ..Claude::default()
    };
    put(
        &claude_path(home.path(), "s1.jsonl"),
        &(old.line() + &new.line()),
        now() - 1000,
    );
    let r = scan(home.path(), 30, UtcOffset::UTC, Some(cache.path()));
    assert_eq!(totals(&r, "claude").4, 1);
    let r = scan(home.path(), 90, UtcOffset::UTC, Some(cache.path()));
    assert_eq!(r.stats.cache, CacheStatus::Rebuilt);
    assert_eq!(totals(&r, "claude").4, 2);
}

#[test]
fn report_json_round_trip_and_text() {
    let home = tempfile::tempdir().unwrap();
    put(
        &claude_path(home.path(), "s1.jsonl"),
        &Claude::default().line(),
        now() - 1000,
    );
    put(&codex_path(home.path()), &codex_rollout(), now() - 1000);
    let old = Claude {
        ts: "2026-11-10T00:00:00.000Z",
        msg: "m2",
        model: "mystery",
        ..Claude::default()
    };
    put(
        &claude_path(home.path(), "s2.jsonl"),
        &old.line(),
        now() - 1000,
    );
    let r = scan(home.path(), 30, UtcOffset::UTC, None);
    let json = serde_json::to_string(&r).unwrap();
    let back: UsageReport = serde_json::from_str(&json).unwrap();
    assert_eq!(back, r);
    // Older documents without the added fields still parse.
    let minimal: UsageReport = serde_json::from_str(
        r#"{"version":1,"generated_at":0,"days":30,"rows":[{"date":"2026-11-14","program":"claude","model":"m","input_tokens":1,"output_tokens":2,"cache_read_tokens":3,"cache_write_tokens":4,"cost_usd":null}]}"#,
    )
    .unwrap();
    assert_eq!(minimal.rows[0].total_tokens(), 10);

    let s = summarize(&r);
    assert_eq!(s.today, "2026-11-14");
    assert_eq!(s.programs.len(), 2);
    let claude = &s.programs[0].totals;
    assert_eq!(claude.today.tokens, 3110);
    assert_eq!(claude.week.tokens, 6220);
    assert_eq!(s.overall.window.tokens, 6220 + 1600 + 300 + 1400);
    let text = render_text(&r);
    assert!(text.contains("claude-opus-5-5"), "{text}");
    assert!(text.contains("gpt-5.4"), "{text}");
    assert!(text.contains("n/a"), "{text}");
    assert!(text.contains("2026-11-10"), "{text}");
}

/// `cargo test -- --ignored --nocapture real_home_summary`
#[test]
#[ignore]
fn real_home_summary() {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    let days: u32 = std::env::var("KOOLIE_USAGE_DAYS")
        .ok()
        .and_then(|d| d.parse().ok())
        .unwrap_or(30);
    let off = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    let cache = tempfile::tempdir().unwrap();
    let t = std::time::Instant::now();
    let r = scan_with_cache(&home, days, off, Some(cache.path())).unwrap();
    let first = t.elapsed();
    let t = std::time::Instant::now();
    let again = scan_with_cache(&home, days, off, Some(cache.path())).unwrap();
    let second = t.elapsed();
    println!("{}", render_text(&r));
    println!(
        "stats: {:?} in {first:?}; cached: {:?} in {second:?}",
        r.stats, again.stats
    );
    for row in &r.rows {
        println!(
            "{} {} {} in={} out={} cr={} cw={} req={} cost={:?}",
            row.date,
            row.program,
            row.model,
            row.input_tokens,
            row.output_tokens,
            row.cache_read_tokens,
            row.cache_write_tokens,
            row.requests,
            row.cost_usd
        );
    }
}
