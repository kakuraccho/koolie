//! Token usage of Claude Code and Codex from their transcripts.
//!
//! Placeholder interface; implemented in M5.

use serde::{Deserialize, Serialize};

/// Tokens for one (day, program, model).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageRow {
    /// Local date, `YYYY-MM-DD`.
    pub date: String,
    /// `claude` or `codex`.
    pub program: String,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    /// Estimated cost at API list prices, USD. `None` when the model has no price.
    pub cost_usd: Option<f64>,
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
}

/// Scan the transcripts under `home` for the last `days` days.
pub fn scan(_home: &std::path::Path, days: u32) -> anyhow::Result<UsageReport> {
    Ok(UsageReport {
        version: 1,
        generated_at: 0,
        days,
        rows: Vec::new(),
    })
}
