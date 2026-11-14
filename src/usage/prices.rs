//! API list prices, USD per token.
//!
//! `pricing.json` is Heeler's bundled table, copied verbatim from
//! <https://github.com/ZingerLittleBee/Heeler/blob/2ca0abc112a37aa4a3f241785b5e90c43411d44f/plugin/src/usage/pricing.json>
//! (`pricingVersion` 2026-10-30). Heeler generates it from LiteLLM's
//! `model_prices_and_context_window.json` (MIT, see `pricing.LICENSE`).
//! [`SUPPLEMENT`] adds models released after that table, from Anthropic's
//! published prices (2026-11-03). Prices are never fetched at runtime.

use super::{Program, Tier, UsageRecord};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::OnceLock;

const TABLE: &str = include_str!("pricing.json");

/// Models missing from `pricing.json`: (model, input, output, cache read,
/// 5-minute cache write, fast multiplier). USD per million tokens.
const SUPPLEMENT: &[(&str, f64, f64, f64, f64, f64)] = &[
    ("claude-fable-5-2", 8.0, 40.0, 0.2, 10.0, 2.0),
    ("claude-mythos-5-2", 8.0, 40.0, 0.2, 10.0, 2.0),
];

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Rates {
    pub input: f64,
    pub output: f64,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct TierRates {
    pub base: Option<Rates>,
    pub fast: Option<Rates>,
    pub flex: Option<Rates>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct LongContext {
    pub threshold: u64,
    #[serde(flatten)]
    pub rates: TierRates,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPrice {
    #[serde(flatten)]
    pub rates: TierRates,
    pub long_context: Option<LongContext>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Table {
    pricing_version: String,
    models: HashMap<String, ModelPrice>,
}

struct Prices {
    version: String,
    models: HashMap<String, ModelPrice>,
}

fn prices() -> &'static Prices {
    static PRICES: OnceLock<Prices> = OnceLock::new();
    PRICES.get_or_init(|| {
        // The table is compiled in and checked by a test, so a parse failure
        // is a build defect; fall back to "everything unpriced" rather than panic.
        let (version, mut models) = match serde_json::from_str::<Table>(TABLE) {
            Ok(t) => (t.pricing_version, t.models),
            Err(_) => (String::from("invalid"), HashMap::new()),
        };
        for &(name, input, output, read, write, fast) in SUPPLEMENT {
            let m = 1e-6;
            let base = Rates {
                input: input * m,
                output: output * m,
                cache_read: Some(read * m),
                cache_write: Some(write * m),
            };
            let fast = Rates {
                input: base.input * fast,
                output: base.output * fast,
                cache_read: base.cache_read.map(|r| r * fast),
                cache_write: base.cache_write.map(|r| r * fast),
            };
            models.entry(name.to_string()).or_insert(ModelPrice {
                rates: TierRates {
                    base: Some(base),
                    fast: Some(fast),
                    flex: None,
                },
                long_context: None,
            });
        }
        Prices { version, models }
    })
}

/// `pricingVersion` of the bundled table.
pub fn pricing_version() -> &'static str {
    &prices().version
}

/// Remove one provider prefix or version suffix, or `None` when nothing applies.
fn strip_once(name: &str) -> Option<&str> {
    // `[1m]` (Claude Code's long-context marker) and similar.
    if let Some(i) = name.find('[')
        && name.ends_with(']')
    {
        return Some(&name[..i]);
    }
    // Vertex: `claude-opus-4-5@20251101`.
    if let Some(i) = name.rfind('@') {
        return Some(&name[..i]);
    }
    // `openai/gpt-5`, `anthropic/claude-…`.
    if let Some(i) = name.rfind('/') {
        return Some(&name[i + 1..]);
    }
    // Bedrock: `us.anthropic.claude-…`, `anthropic.claude-…`.
    if let Some(i) = name.find("claude-")
        && i > 0
        && name[..i].ends_with('.')
    {
        return Some(&name[i..]);
    }
    let (head, tail) = name.rsplit_once('-')?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    // Bedrock `-v1:0`, `-v2`.
    if let Some(v) = tail.strip_prefix('v') {
        let (major, minor) = v.split_once(':').unwrap_or((v, "0"));
        if digits(major) && digits(minor) {
            return Some(head);
        }
    }
    // `-20251001`.
    if tail.len() == 8 && digits(tail) {
        return Some(head);
    }
    // `-2025-10-01`.
    if tail.len() == 2 && digits(tail) {
        let b = name.as_bytes();
        let n = b.len();
        if n >= 11
            && b[n - 3] == b'-'
            && b[n - 6] == b'-'
            && b[n - 11] == b'-'
            && digits(&name[n - 10..n - 6])
            && digits(&name[n - 5..n - 3])
        {
            return Some(&name[..n - 11]);
        }
    }
    None
}

/// The table entry for `model`, tolerating provider prefixes and date or
/// version suffixes. Never guesses across model versions: an unknown
/// `claude-opus-9` does not fall back to `claude-opus`.
pub fn lookup(model: &str) -> Option<(&'static str, &'static ModelPrice)> {
    let models = &prices().models;
    let lower = model.trim().to_ascii_lowercase();
    let mut name = lower.as_str();
    loop {
        if let Some((k, v)) = models.get_key_value(name) {
            return Some((k.as_str(), v));
        }
        name = strip_once(name)?;
    }
}

/// Estimated cost of one request, or `None` when the model or its service
/// tier has no price.
pub fn cost(program: Program, rec: &UsageRecord) -> Option<f64> {
    let (_, price) = lookup(&rec.model)?;
    let context = rec.input + rec.cache_read + rec.write_5m + rec.write_1h;
    // A request above the long-context threshold is priced entirely at that tier.
    let tier = match &price.long_context {
        Some(lc) if context > lc.threshold => &lc.rates,
        _ => &price.rates,
    };
    let rates = match rec.tier {
        Tier::Standard => tier.base,
        Tier::Fast => tier.fast,
        Tier::Flex => tier.flex,
        Tier::Unknown => None,
    }?;
    let read = rates.cache_read.unwrap_or(rates.input);
    let write = rates.cache_write.unwrap_or(rates.input);
    let f = |n: u64| n as f64;
    let writes = match program {
        // 1-hour writes cost twice the input rate.
        Program::Claude => f(rec.write_5m) * write + f(rec.write_1h) * rates.input * 2.0,
        Program::Codex => f(rec.write_5m + rec.write_1h) * write,
    };
    Some(
        f(rec.input) * rates.input
            + f(rec.cache_read) * read
            + writes
            + f(rec.output) * rates.output,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_parses() {
        assert_eq!(pricing_version(), "2026-10-30");
        assert!(prices().models.len() > 100);
    }

    #[test]
    fn suffixes_are_tolerated() {
        let key = |m: &str| lookup(m).map(|(k, _)| k);
        assert_eq!(key("claude-opus-5-5"), Some("claude-opus-5-5"));
        assert_eq!(key("claude-opus-5-5-20261101"), Some("claude-opus-5-5"));
        assert_eq!(key("claude-opus-5-5[1m]"), Some("claude-opus-5-5"));
        assert_eq!(key("claude-sonnet-5@20260301"), Some("claude-sonnet-5"));
        assert_eq!(
            key("us.anthropic.claude-sonnet-5-v1:0"),
            Some("claude-sonnet-5")
        );
        assert_eq!(key("anthropic/claude-haiku-4-5"), Some("claude-haiku-4-5"));
        assert_eq!(key("openai/gpt-5.4-2026-03-05"), Some("gpt-5.4-2026-03-05"));
        assert_eq!(key("gpt-5.4-2027-01-02"), Some("gpt-5.4"));
        assert_eq!(key("GPT-5.6-SOL"), Some("gpt-5.6-sol"));
        // An exact dated entry wins over the stripped name.
        assert_eq!(
            key("claude-haiku-4-5-20251001"),
            Some("claude-haiku-4-5-20251001")
        );
        // Supplement.
        assert_eq!(key("claude-fable-5-2"), Some("claude-fable-5-2"));
        // No guessing across versions.
        assert_eq!(key("claude-opus-9"), None);
        assert_eq!(key("claude-opus-5-6"), None);
        assert_eq!(key("gpt-5.4-ultra"), None);
        assert_eq!(key("<synthetic>"), None);
    }

    fn rec(model: &str) -> UsageRecord {
        UsageRecord {
            model: model.into(),
            ..Default::default()
        }
    }

    #[test]
    fn claude_cost() {
        let r = UsageRecord {
            input: 1_000_000,
            output: 1_000_000,
            cache_read: 1_000_000,
            write_5m: 1_000_000,
            write_1h: 1_000_000,
            ..rec("claude-opus-5-5")
        };
        // 4 + 20 + 0.2 + 5 + 8.
        let c = cost(Program::Claude, &r).unwrap();
        assert!((c - 37.2).abs() < 1e-9, "{c}");
        let fast = UsageRecord {
            tier: Tier::Fast,
            ..r.clone()
        };
        assert!((cost(Program::Claude, &fast).unwrap() - 74.4).abs() < 1e-9);
        assert_eq!(cost(Program::Claude, &rec("claude-unknown-1")), None);
    }

    #[test]
    fn codex_cost_and_long_context() {
        // gpt-5.4: input 2.5, cached 0.25, output 15 per MTok; >272K uses 5 / 0.5 / 22.5.
        let r = UsageRecord {
            input: 100_000,
            cache_read: 100_000,
            output: 10_000,
            ..rec("gpt-5.4")
        };
        let c = cost(Program::Codex, &r).unwrap();
        assert!((c - (0.25 + 0.025 + 0.15)).abs() < 1e-9, "{c}");
        let long = UsageRecord {
            input: 200_000,
            cache_read: 100_000,
            output: 10_000,
            ..rec("gpt-5.4")
        };
        let c = cost(Program::Codex, &long).unwrap();
        assert!((c - (1.0 + 0.05 + 0.225)).abs() < 1e-9, "{c}");
        // A tier the model has no rates for is unpriced.
        let fast_nano = UsageRecord {
            tier: Tier::Fast,
            ..rec("gpt-5-nano")
        };
        assert_eq!(cost(Program::Codex, &fast_nano), None);
        let unknown = UsageRecord {
            tier: Tier::Unknown,
            ..rec("gpt-5.4")
        };
        assert_eq!(cost(Program::Codex, &unknown), None);
    }
}
