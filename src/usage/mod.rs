//! Token usage of Claude Code and Codex from their transcripts, with an
//! estimated cost at API list prices. This is an estimate, not a bill.
//!
//! Counting follows Heeler's design (`docs/design/agent-usage.md`, ADR 0028);
//! the per-program rules are in [`claude`] and [`codex`], prices in
//! [`prices`], and the incremental cache in [`cache`].

mod cache;
mod claude;
mod codex;
mod prices;

pub use prices::pricing_version;

use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::io::{BufRead, Read, Seek};
use std::path::{Path, PathBuf};
use time::{Date, OffsetDateTime, UtcOffset};

/// Format version of [`UsageReport`].
pub const REPORT_VERSION: u32 = 1;

const DAY_MS: i64 = 86_400_000;

/// Tokens for one (day, program, model).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageRow {
    /// Local date, `YYYY-MM-DD`.
    pub date: String,
    /// `claude` or `codex`.
    pub program: String,
    pub model: String,
    /// Input that was neither read from nor written to the cache. For Codex
    /// this is `input_tokens - cached_input_tokens - cache_write_input_tokens`.
    pub input_tokens: u64,
    /// Output, including reasoning.
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    /// 5-minute and 1-hour cache writes.
    pub cache_write_tokens: u64,
    /// Estimated cost at API list prices, USD. `None` when the model has no price.
    pub cost_usd: Option<f64>,
    /// Codex reasoning tokens; informative, already inside `output_tokens`.
    #[serde(default)]
    pub reasoning_tokens: u64,
    /// API requests (Claude responses, Codex responses or turns).
    #[serde(default)]
    pub requests: u64,
    /// Tokens left out of `cost_usd` because their model or service tier has
    /// no price (all of them when `cost_usd` is `None`).
    #[serde(default)]
    pub unpriced_tokens: u64,
}

impl UsageRow {
    /// Input + output + cache read + cache write.
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_read_tokens + self.cache_write_tokens
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheStatus {
    /// Scanned without a cache.
    #[default]
    Off,
    /// A valid cache was used.
    Loaded,
    /// No usable cache; everything was read.
    Rebuilt,
}

/// What a scan had to do.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScanStats {
    /// Transcripts modified inside the window.
    pub files: u64,
    /// Transcripts that had new bytes to read.
    pub files_read: u64,
    pub bytes_read: u64,
    pub cache: CacheStatus,
}

/// What `koolie usage --json` prints and the Usage screen shows.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageReport {
    /// Format version of this document.
    pub version: u32,
    /// Unix seconds.
    pub generated_at: i64,
    pub days: u32,
    pub rows: Vec<UsageRow>,
    /// The UTC offset the dates are in, seconds east of UTC.
    #[serde(default)]
    pub utc_offset_seconds: i32,
    /// `pricingVersion` of the price table used for `cost_usd`.
    #[serde(default)]
    pub pricing_version: String,
    #[serde(default)]
    pub stats: ScanStats,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Program {
    Claude,
    Codex,
}

impl Program {
    fn as_str(self) -> &'static str {
        match self {
            Program::Claude => "claude",
            Program::Codex => "codex",
        }
    }
}

/// Service tier a request was priced at.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Tier {
    #[default]
    Standard,
    Fast,
    Flex,
    /// A tier the price table cannot price.
    Unknown,
}

fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

/// One API request's usage, normalized across programs. Short field names
/// keep the cache small.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct UsageRecord {
    /// Dedupe key, unique per request.
    #[serde(rename = "k")]
    pub key: String,
    /// Unix milliseconds.
    #[serde(rename = "t")]
    pub ts_ms: i64,
    #[serde(rename = "m")]
    pub model: String,
    #[serde(rename = "i", default, skip_serializing_if = "is_default")]
    pub input: u64,
    #[serde(rename = "o", default, skip_serializing_if = "is_default")]
    pub output: u64,
    #[serde(rename = "r", default, skip_serializing_if = "is_default")]
    pub cache_read: u64,
    #[serde(rename = "w", default, skip_serializing_if = "is_default")]
    pub write_5m: u64,
    #[serde(rename = "h", default, skip_serializing_if = "is_default")]
    pub write_1h: u64,
    #[serde(rename = "x", default, skip_serializing_if = "is_default")]
    pub reasoning: u64,
    #[serde(rename = "p", default, skip_serializing_if = "is_default")]
    pub tier: Tier,
    /// Claude: `message.id + sessionId`, for sidechain replays.
    #[serde(rename = "g", default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Claude: `isSidechain`.
    #[serde(rename = "s", default, skip_serializing_if = "is_default")]
    pub sidechain: bool,
}

impl UsageRecord {
    pub(crate) fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.write_5m + self.write_1h
    }

    /// Collision order: non-sidechain first, then the larger total (streamed
    /// snapshots can undercount output), then a stable tie-break.
    fn rank(&self) -> (bool, u64, u64, i64, &str) {
        (
            !self.sidechain,
            self.total(),
            self.output,
            self.ts_ms,
            &self.model,
        )
    }
}

/// A token count that tolerates negative, fractional or non-numeric values (as 0).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Count(pub u64);

impl<'de> Deserialize<'de> for Count {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Count;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a token count")
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Count, E> {
                Ok(Count(v))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Count, E> {
                Ok(Count(u64::try_from(v).unwrap_or(0)))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Count, E> {
                Ok(Count(if v.is_finite() && v > 0.0 {
                    v as u64
                } else {
                    0
                }))
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Count, E> {
                Ok(Count(0))
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Count, E> {
                Ok(Count(0))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Count, E> {
                Ok(Count(0))
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut a: A) -> Result<Count, A::Error> {
                while a.next_element::<de::IgnoredAny>()?.is_some() {}
                Ok(Count(0))
            }
            fn visit_map<A: de::MapAccess<'de>>(self, mut a: A) -> Result<Count, A::Error> {
                while a.next_entry::<de::IgnoredAny, de::IgnoredAny>()?.is_some() {}
                Ok(Count(0))
            }
        }
        d.deserialize_any(V)
    }
}

/// RFC 3339 timestamp to unix milliseconds.
pub(crate) fn parse_timestamp_ms(s: &str) -> Option<i64> {
    let t = OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()?;
    i64::try_from(t.unix_timestamp_nanos() / 1_000_000).ok()
}

fn local_date(ms: i64, offset: UtcOffset) -> Option<Date> {
    let t = OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000).ok()?;
    Some(t.to_offset(offset).date())
}

fn format_date(d: Date) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())
}

fn now_ms() -> i64 {
    i64::try_from(OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000).unwrap_or(0)
}

/// Scan the transcripts under `home` for the last `days` days. Days are
/// local dates at `utc_offset` (compute it once at startup, before threads
/// start: `time`'s local-offset lookup is unreliable in threaded programs).
///
/// Synchronous and blocking; uses the cache in `~/.cache/koolie/usage`.
pub fn scan(home: &Path, days: u32, utc_offset: UtcOffset) -> anyhow::Result<UsageReport> {
    let dir = crate::config::cache_home().join("usage");
    scan_with_cache(home, days, utc_offset, Some(&dir))
}

/// [`scan`] with an explicit cache directory; `None` scans without a cache.
pub fn scan_with_cache(
    home: &Path,
    days: u32,
    utc_offset: UtcOffset,
    cache_dir: Option<&Path>,
) -> anyhow::Result<UsageReport> {
    Ok(scan_at(home, days, utc_offset, cache_dir, now_ms()))
}

/// Transcript files: Claude Code projects and Codex rollouts.
fn transcripts(home: &Path) -> Vec<(Program, PathBuf)> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut files = Vec::new();
    walk(
        &home.join(".claude").join("projects"),
        &mut seen,
        &mut files,
        &|n| n.ends_with(".jsonl"),
    );
    out.extend(files.drain(..).map(|p| (Program::Claude, p)));
    for dir in ["sessions", "archived_sessions"] {
        walk(
            &home.join(".codex").join(dir),
            &mut seen,
            &mut files,
            &|n| n.starts_with("rollout-") && n.ends_with(".jsonl"),
        );
    }
    out.extend(files.drain(..).map(|p| (Program::Codex, p)));
    out
}

fn walk(
    dir: &Path,
    seen: &mut HashSet<PathBuf>,
    out: &mut Vec<PathBuf>,
    keep: &dyn Fn(&str) -> bool,
) {
    // Follow symlinked directories once.
    let Ok(real) = std::fs::canonicalize(dir) else {
        return;
    };
    if !seen.insert(real) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(mut ft) = entry.file_type() else {
            continue;
        };
        if ft.is_symlink() {
            match std::fs::metadata(&path) {
                Ok(m) => ft = m.file_type(),
                Err(_) => continue,
            }
        }
        if ft.is_dir() {
            walk(&path, seen, out, keep);
        } else if ft.is_file() && entry.file_name().to_str().is_some_and(keep) {
            out.push(path);
        }
    }
}

struct Identity {
    len: u64,
    mtime_ns: i64,
    dev: u64,
    ino: u64,
}

fn identity(meta: &std::fs::Metadata) -> Identity {
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_nanos()).ok())
        .unwrap_or(0);
    #[cfg(unix)]
    let (dev, ino) = {
        use std::os::unix::fs::MetadataExt;
        (meta.dev(), meta.ino())
    };
    #[cfg(not(unix))]
    let (dev, ino) = (0, 0);
    Identity {
        len: meta.len(),
        mtime_ns,
        dev,
        ino,
    }
}

/// Keep the better of two copies of one Claude request within a file.
fn merge_claude(
    records: &mut Vec<UsageRecord>,
    index: &mut Option<HashMap<String, usize>>,
    rec: UsageRecord,
) {
    let index = index.get_or_insert_with(|| {
        records
            .iter()
            .enumerate()
            .map(|(i, r)| (r.key.clone(), i))
            .collect()
    });
    match index.get(&rec.key) {
        Some(&i) => {
            if rec.rank() > records[i].rank() {
                records[i] = rec;
            }
        }
        None => {
            index.insert(rec.key.clone(), records.len());
            records.push(rec);
        }
    }
}

/// Read complete lines from `entry.offset` up to `len`. Returns bytes consumed.
fn read_tail(entry: &mut cache::FileEntry, path: &Path, len: u64) -> std::io::Result<u64> {
    let mut f = std::fs::File::open(path)?;
    f.seek(std::io::SeekFrom::Start(entry.offset))?;
    let mut reader =
        std::io::BufReader::with_capacity(256 * 1024, f.take(len.saturating_sub(entry.offset)));
    let path_str = path.to_string_lossy();
    let mut buf = Vec::new();
    let mut read = 0u64;
    let mut index = None;
    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 || buf.last() != Some(&b'\n') {
            // End, or a line still being written: leave it for the next scan.
            break;
        }
        let line_offset = entry.offset;
        entry.offset += n as u64;
        read += n as u64;
        let line = &buf[..n - 1];
        match entry.program {
            Program::Claude => {
                if let Some(rec) = claude::parse_line(line) {
                    merge_claude(&mut entry.records, &mut index, rec);
                }
            }
            Program::Codex => {
                let state = entry.codex.get_or_insert_with(Default::default);
                if state.wants(line) {
                    let recs = state.consume(line, || format!("{path_str}\u{1f}{line_offset}"));
                    entry.records.extend(recs);
                }
            }
        }
    }
    Ok(read)
}

fn scan_at(
    home: &Path,
    days: u32,
    offset: UtcOffset,
    cache_dir: Option<&Path>,
    now_ms: i64,
) -> UsageReport {
    let days = days.max(1);
    let today = local_date(now_ms, offset).unwrap_or(Date::MIN);
    let first = today
        .checked_sub(time::Duration::days(i64::from(days) - 1))
        .unwrap_or(Date::MIN);
    let cutoff_ms = first.midnight().assume_offset(offset).unix_timestamp() * 1000;
    // Keep at least 31 days of records so the default window survives any offset.
    let keep_from = cutoff_ms.min(now_ms - 31 * DAY_MS);

    let mut stats = ScanStats::default();
    let mut warnings = Vec::new();
    let cache_file = cache_dir.map(|d| cache::path(d, home));
    let (mut old, loaded) = match &cache_file {
        Some(f) => cache::load(f, home),
        None => (cache::load_none(home), false),
    };
    stats.cache = match (&cache_file, loaded) {
        (None, _) => CacheStatus::Off,
        (Some(_), true) if old.horizon_ms <= keep_from => CacheStatus::Loaded,
        (Some(_), _) => CacheStatus::Rebuilt,
    };
    if old.horizon_ms > keep_from {
        // The cache pruned records this window needs.
        old.files.clear();
    }
    let mut dirty = !loaded || old.horizon_ms > keep_from;
    let mut files = BTreeMap::new();

    for (program, path) in transcripts(home) {
        let key = path.to_string_lossy().into_owned();
        let prev = old.files.remove(&key);
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(e) => {
                warnings.push(format!("{}: {e}", path.display()));
                if let Some(p) = prev {
                    files.insert(key, p);
                }
                continue;
            }
        };
        let id = identity(&meta);
        if id.mtime_ns / 1_000_000 < cutoff_ms {
            // Nothing in this window. Keep what is cached unless it is
            // older than anything kept.
            if let Some(p) = prev {
                if id.mtime_ns / 1_000_000 >= keep_from {
                    files.insert(key, p);
                } else {
                    dirty = true;
                }
            }
            continue;
        }
        stats.files += 1;
        let fresh = || cache::FileEntry {
            program,
            len: 0,
            mtime_ns: 0,
            dev: id.dev,
            ino: id.ino,
            offset: 0,
            codex: None,
            records: Vec::new(),
        };
        let mut entry = match prev {
            Some(p)
                if p.program == program
                    && p.dev == id.dev
                    && p.ino == id.ino
                    && ((id.len == p.len && id.mtime_ns == p.mtime_ns)
                        || (id.len > p.len && id.mtime_ns >= p.mtime_ns)) =>
            {
                p
            }
            _ => fresh(),
        };
        if entry.len != id.len || entry.mtime_ns != id.mtime_ns {
            dirty = true;
            match read_tail(&mut entry, &path, id.len) {
                Ok(n) => {
                    stats.files_read += 1;
                    stats.bytes_read += n;
                    entry.len = id.len;
                    entry.mtime_ns = id.mtime_ns;
                }
                Err(e) => {
                    // Keep what was parsed; the rest is retried next scan.
                    warnings.push(format!("{}: {e}", path.display()));
                    entry.len = entry.offset;
                    entry.mtime_ns = id.mtime_ns;
                }
            }
        }
        if let Some(state) = entry.codex.as_mut() {
            let late = state.finish(now_ms);
            if !late.is_empty() {
                dirty = true;
                entry.records.extend(late);
            }
        }
        let before = entry.records.len();
        entry.records.retain(|r| r.ts_ms >= keep_from);
        dirty |= entry.records.len() != before;
        files.insert(key, entry);
    }
    // Deleted transcripts.
    dirty |= !old.files.is_empty();

    let rows = aggregate(&files, cutoff_ms, offset);
    if let Some(file) = &cache_file
        && dirty
    {
        let cache = cache::Cache {
            format: cache::FORMAT,
            home: home.to_string_lossy().into_owned(),
            horizon_ms: keep_from,
            files,
        };
        if let Err(e) = cache::save(file, &cache) {
            warnings.push(format!("usage cache {}: {e}", file.display()));
        }
    }
    UsageReport {
        version: REPORT_VERSION,
        generated_at: now_ms.div_euclid(1000),
        days,
        rows,
        utc_offset_seconds: offset.whole_seconds(),
        pricing_version: pricing_version().to_string(),
        stats,
        warnings,
    }
}

/// Claude Code copies responses between transcripts; keep one per request.
///
/// 1. Per dedupe key: the non-sidechain copy, then the larger total.
/// 2. Per `message.id + sessionId`: when a non-sidechain request exists,
///    sidechain requests with another `requestId` are replays (subagent logs
///    such as `/btw` re-copy the parent) and are dropped; when all are
///    sidechain, only the best one counts.
///
/// The result does not depend on the order files are read in.
fn dedupe_claude<'a>(records: impl Iterator<Item = &'a UsageRecord>) -> Vec<&'a UsageRecord> {
    let mut best: HashMap<&str, &UsageRecord> = HashMap::new();
    let mut groups: HashMap<&str, Vec<&str>> = HashMap::new();
    for r in records {
        best.entry(&r.key)
            .and_modify(|b| {
                if r.rank() > b.rank() {
                    *b = r;
                }
            })
            .or_insert(r);
        if let Some(g) = &r.group {
            groups.entry(g).or_default().push(&r.key);
        }
    }
    let mut dropped: HashSet<&str> = HashSet::new();
    for keys in groups.values_mut() {
        keys.sort_unstable();
        keys.dedup();
        if keys.len() < 2 {
            continue;
        }
        let winners: Vec<&UsageRecord> = keys.iter().filter_map(|k| best.get(k).copied()).collect();
        if winners.iter().any(|w| !w.sidechain) {
            dropped.extend(
                winners
                    .iter()
                    .filter(|w| w.sidechain)
                    .map(|w| w.key.as_str()),
            );
        } else if let Some(top) = winners
            .iter()
            .max_by(|a, b| a.rank().cmp(&b.rank()).then(b.key.cmp(&a.key)))
        {
            dropped.extend(
                winners
                    .iter()
                    .filter(|w| w.key != top.key)
                    .map(|w| w.key.as_str()),
            );
        }
    }
    best.into_iter()
        .filter(|(k, _)| !dropped.contains(k))
        .map(|(_, r)| r)
        .collect()
}

fn aggregate(
    files: &BTreeMap<String, cache::FileEntry>,
    cutoff_ms: i64,
    offset: UtcOffset,
) -> Vec<UsageRow> {
    let of = |p: Program| {
        files
            .values()
            .filter(move |e| e.program == p)
            .flat_map(|e| e.records.iter())
    };
    let claude = dedupe_claude(of(Program::Claude));
    let mut codex: HashMap<&str, &UsageRecord> = HashMap::new();
    for r in of(Program::Codex) {
        codex
            .entry(&r.key)
            .and_modify(|b| {
                if r.rank() > b.rank() {
                    *b = r;
                }
            })
            .or_insert(r);
    }
    let all = claude
        .into_iter()
        .map(|r| (Program::Claude, r))
        .chain(codex.into_values().map(|r| (Program::Codex, r)));

    let mut rows: BTreeMap<(String, Program, String), (UsageRow, bool, f64)> = BTreeMap::new();
    for (program, r) in all {
        if r.ts_ms < cutoff_ms {
            continue;
        }
        let Some(date) = local_date(r.ts_ms, offset) else {
            continue;
        };
        let date = format_date(date);
        let (row, priced, cost) = rows
            .entry((date.clone(), program, r.model.clone()))
            .or_insert_with(|| {
                (
                    UsageRow {
                        date,
                        program: program.as_str().into(),
                        model: r.model.clone(),
                        ..UsageRow::default()
                    },
                    false,
                    0.0,
                )
            });
        row.input_tokens += r.input;
        row.output_tokens += r.output;
        row.cache_read_tokens += r.cache_read;
        row.cache_write_tokens += r.write_5m + r.write_1h;
        row.reasoning_tokens += r.reasoning;
        row.requests += 1;
        match prices::cost(program, r) {
            Some(c) => {
                *priced = true;
                *cost += c;
            }
            None => row.unpriced_tokens += r.total(),
        }
    }
    rows.into_values()
        .map(|(mut row, priced, cost)| {
            row.cost_usd = priced.then_some(cost);
            row
        })
        .collect()
}

/// Tokens and estimated cost over one period.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Totals {
    pub tokens: u64,
    /// Cost of the priced tokens.
    pub cost_usd: f64,
    /// Tokens of unpriced models, not in `cost_usd`.
    pub unpriced_tokens: u64,
}

impl Totals {
    fn add(&mut self, row: &UsageRow) {
        self.tokens += row.total_tokens();
        self.cost_usd += row.cost_usd.unwrap_or(0.0);
        self.unpriced_tokens += row.unpriced_tokens;
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PeriodTotals {
    pub today: Totals,
    /// The last 7 days, today included.
    pub week: Totals,
    /// The report's whole window.
    pub window: Totals,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProgramSummary {
    pub program: String,
    #[serde(flatten)]
    pub totals: PeriodTotals,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    /// Local date the report was generated, `YYYY-MM-DD`.
    pub today: String,
    pub days: u32,
    /// `claude` and `codex` always, then any other program in the report.
    pub programs: Vec<ProgramSummary>,
    pub overall: PeriodTotals,
}

/// Totals per program and overall for today, 7 days and the window.
pub fn summarize(report: &UsageReport) -> Summary {
    let offset = UtcOffset::from_whole_seconds(report.utc_offset_seconds).unwrap_or(UtcOffset::UTC);
    let today = local_date(report.generated_at.saturating_mul(1000), offset).unwrap_or(Date::MIN);
    let week_start = format_date(
        today
            .checked_sub(time::Duration::days(6))
            .unwrap_or(Date::MIN),
    );
    let today = format_date(today);
    let mut programs: Vec<ProgramSummary> = ["claude", "codex"]
        .into_iter()
        .map(|p| ProgramSummary {
            program: p.into(),
            totals: PeriodTotals::default(),
        })
        .collect();
    let mut overall = PeriodTotals::default();
    for row in &report.rows {
        let i = match programs.iter().position(|p| p.program == row.program) {
            Some(i) => i,
            None => {
                programs.push(ProgramSummary {
                    program: row.program.clone(),
                    totals: PeriodTotals::default(),
                });
                programs.len() - 1
            }
        };
        for t in [&mut programs[i].totals, &mut overall] {
            t.window.add(row);
            if row.date >= week_start {
                t.week.add(row);
            }
            if row.date == today {
                t.today.add(row);
            }
        }
    }
    Summary {
        today,
        days: report.days,
        programs,
        overall,
    }
}

fn fmt_tokens(n: u64) -> String {
    let f = n as f64;
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}K", f / 1e3),
        1_000_000..=999_999_999 => format!("{:.1}M", f / 1e6),
        _ => format!("{:.2}B", f / 1e9),
    }
}

fn fmt_cost(cost: Option<f64>, unpriced: u64) -> String {
    let star = if unpriced > 0 { "*" } else { "" };
    match cost {
        None => "n/a".into(),
        Some(c) if c > 0.0 && c < 0.01 => format!("<$0.01{star}"),
        Some(c) => format!("${c:.2}{star}"),
    }
}

fn fmt_totals(t: &Totals) -> String {
    let cost = if t.tokens > 0 && t.unpriced_tokens >= t.tokens {
        None
    } else {
        Some(t.cost_usd)
    };
    format!(
        "{:>8} {:>10}",
        fmt_tokens(t.tokens),
        fmt_cost(cost, t.unpriced_tokens)
    )
}

/// A compact table for `koolie usage`.
pub fn render_text(report: &UsageReport) -> String {
    let s = summarize(report);
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Token usage, last {} days to {} (estimated at API list prices {}; not a bill)",
        s.days, s.today, report.pricing_version
    );
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "{:<8} {:>19} {:>19} {:>19}",
        "",
        "today",
        "7 days",
        format!("{} days", s.days)
    );
    let line = |out: &mut String, name: &str, t: &PeriodTotals| {
        let _ = writeln!(
            out,
            "{:<8} {} {} {}",
            name,
            fmt_totals(&t.today),
            fmt_totals(&t.week),
            fmt_totals(&t.window)
        );
    };
    for p in &s.programs {
        line(&mut out, &p.program, &p.totals);
    }
    line(&mut out, "total", &s.overall);

    // Per model over the window.
    let mut models: BTreeMap<(&str, &str), UsageRow> = BTreeMap::new();
    for r in &report.rows {
        let m = models.entry((&r.program, &r.model)).or_default();
        m.input_tokens += r.input_tokens;
        m.output_tokens += r.output_tokens;
        m.cache_read_tokens += r.cache_read_tokens;
        m.cache_write_tokens += r.cache_write_tokens;
        m.requests += r.requests;
        m.unpriced_tokens += r.unpriced_tokens;
        if let Some(c) = r.cost_usd {
            m.cost_usd = Some(m.cost_usd.unwrap_or(0.0) + c);
        }
    }
    if !models.is_empty() {
        let width = models
            .keys()
            .map(|(_, m)| m.len())
            .max()
            .unwrap_or(5)
            .max(5);
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "{:<8} {:<width$} {:>8} {:>8} {:>8} {:>8} {:>8} {:>10}",
            "program", "model", "requests", "input", "output", "cache rd", "cache wr", "cost"
        );
        for ((program, model), m) in &models {
            let _ = writeln!(
                out,
                "{:<8} {:<width$} {:>8} {:>8} {:>8} {:>8} {:>8} {:>10}",
                program,
                model,
                m.requests,
                fmt_tokens(m.input_tokens),
                fmt_tokens(m.output_tokens),
                fmt_tokens(m.cache_read_tokens),
                fmt_tokens(m.cache_write_tokens),
                fmt_cost(m.cost_usd, m.unpriced_tokens)
            );
        }
    }

    // Per day, newest first.
    let mut daily: BTreeMap<&str, Totals> = BTreeMap::new();
    for r in &report.rows {
        daily.entry(&r.date).or_default().add(r);
    }
    if !daily.is_empty() {
        let _ = writeln!(out);
        for (date, t) in daily.iter().rev() {
            let _ = writeln!(out, "{date} {}", fmt_totals(t));
        }
    }
    if report.rows.iter().any(|r| r.unpriced_tokens > 0) {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "* excludes tokens of models or tiers without a price (n/a: none priced)"
        );
    }
    for w in &report.warnings {
        let _ = writeln!(out, "warning: {w}");
    }
    out
}

#[cfg(test)]
mod tests;
