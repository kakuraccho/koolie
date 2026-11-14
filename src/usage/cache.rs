//! Incremental scan cache: `<cache_dir>/scan-v<N>-<hash of home>.json`.
//!
//! Per transcript it keeps the file identity (len, mtime, dev, ino), the
//! byte offset parsed up to the last complete line, the Codex parser state,
//! and the file's own usage records (already deduplicated within the file).
//! Deduplication across files runs over all cached records on every scan, so
//! a file that is rewritten can be re-read on its own without invalidating
//! the others. A cache that fails to load, or was written by another format
//! version or for another home, is discarded.

use super::codex::CodexState;
use super::{Program, UsageRecord};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Bump when the record shape or a counting rule changes.
pub(crate) const FORMAT: u32 = 1;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Cache {
    pub format: u32,
    pub home: String,
    /// Records older than this (unix ms) were pruned; a scan that reaches
    /// further back rebuilds.
    pub horizon_ms: i64,
    pub files: BTreeMap<String, FileEntry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct FileEntry {
    pub program: Program,
    pub len: u64,
    pub mtime_ns: i64,
    pub dev: u64,
    pub ino: u64,
    /// Bytes parsed: the end of the last complete line.
    pub offset: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex: Option<CodexState>,
    pub records: Vec<UsageRecord>,
}

fn fnv1a(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

pub(crate) fn path(cache_dir: &Path, home: &Path) -> PathBuf {
    cache_dir.join(format!(
        "scan-v{FORMAT}-{:016x}.json",
        fnv1a(&home.to_string_lossy())
    ))
}

/// An empty cache for `home`.
pub(crate) fn load_none(home: &Path) -> Cache {
    Cache {
        format: FORMAT,
        home: home.to_string_lossy().into_owned(),
        // Nothing pruned yet.
        horizon_ms: i64::MIN,
        files: BTreeMap::new(),
    }
}

/// The cache for `home` and whether it was usable, or an empty one.
pub(crate) fn load(file: &Path, home: &Path) -> (Cache, bool) {
    let empty = load_none(home);
    let Ok(bytes) = std::fs::read(file) else {
        return (empty, false);
    };
    match serde_json::from_slice::<Cache>(&bytes) {
        Ok(c) if c.format == FORMAT && c.home == empty.home => (c, true),
        _ => (empty, false),
    }
}

/// Write atomically (temporary file + rename).
pub(crate) fn save(file: &Path, cache: &Cache) -> std::io::Result<()> {
    let dir = file.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.tmp.{}",
        file.file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default(),
        std::process::id()
    ));
    let result = (|| {
        let mut f = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
        serde_json::to_writer(&mut f, cache)?;
        f.flush()?;
        f.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        std::fs::rename(&tmp, file)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}
