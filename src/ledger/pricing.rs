//! Dated, configurable token pricing.
//!
//! There is no built-in rate table. Prices change, differ per account,
//! and are wrong the moment they are compiled in — and a wrong price is
//! worse than no price, because it looks authoritative. Rates come from
//! `[[pricing]]` blocks in config; a model with no matching entry costs
//! an explicit unknown.
//!
//! ```toml
//! [[pricing]]
//! model               = "anthropic/claude-haiku-4.5"
//! effective           = "2026-01-01"
//! currency            = "USD"
//! input_per_mtok      = 1.0
//! output_per_mtok     = 5.0
//! cache_read_per_mtok = 0.1
//! cache_write_per_mtok = 1.25
//! ```
//!
//! `model` matches by prefix, longest first. Among the entries that
//! match, the latest `effective` date not in the future wins, so last
//! year's rate stays on file next to this year's.

use serde::{Deserialize, Serialize};

use super::BilledTokens;

/// One dated rate card. Rates are per million tokens.
#[derive(Clone, Debug, Deserialize)]
pub struct PriceEntry {
    /// Model-id prefix this card applies to.
    pub model: String,
    /// `YYYY-MM-DD` the rate took effect. Absent means "as far back as
    /// anyone cares" — it loses to any dated entry that also applies.
    #[serde(default)]
    pub effective: Option<String>,
    #[serde(default = "default_currency")]
    pub currency: String,
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    #[serde(default)]
    pub cache_read_per_mtok: Option<f64>,
    #[serde(default)]
    pub cache_write_per_mtok: Option<f64>,
}

fn default_currency() -> String {
    "USD".to_string()
}

/// The card that applied to a run, with the provenance a reader needs to
/// judge whether the number means anything.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResolvedPrice {
    /// The `model` prefix that matched, not the model that ran.
    pub matched: String,
    pub effective: Option<String>,
    pub currency: String,
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub cache_read_per_mtok: Option<f64>,
    pub cache_write_per_mtok: Option<f64>,
}

/// Pick the rate card for `model` as of `on_day` (days since the Unix
/// epoch). Longest matching prefix wins; among equally specific cards,
/// the latest effective date that has already arrived.
pub fn resolve(entries: &[PriceEntry], model: &str, on_day: i64) -> Option<ResolvedPrice> {
    entries
        .iter()
        .filter(|e| model.starts_with(&e.model))
        .filter(|e| match e.effective.as_deref().and_then(days_from_iso) {
            Some(day) => day <= on_day,
            // An undated card always applies; an unparseable date is
            // treated as absent rather than silently dropping the card.
            None => true,
        })
        .max_by_key(|e| {
            (
                e.model.len(),
                e.effective.as_deref().and_then(days_from_iso),
            )
        })
        .map(|e| ResolvedPrice {
            matched: e.model.clone(),
            effective: e.effective.clone(),
            currency: e.currency.clone(),
            input_per_mtok: e.input_per_mtok,
            output_per_mtok: e.output_per_mtok,
            cache_read_per_mtok: e.cache_read_per_mtok,
            cache_write_per_mtok: e.cache_write_per_mtok,
        })
}

/// Money for one call, or the list of reasons there isn't any. Never a
/// zero standing in for "we couldn't tell".
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CostEstimate {
    pub amount: Option<f64>,
    pub currency: Option<String>,
    /// Why `amount` is `None`, in the reader's terms. Empty when known.
    #[serde(default)]
    pub unknown: Vec<String>,
}

/// Price one call. Every billable component must have both a token count
/// and a rate; anything missing leaves the whole call unpriced and says
/// which piece was missing.
pub fn cost_of(billed: &BilledTokens, price: Option<&ResolvedPrice>) -> CostEstimate {
    let Some(p) = price else {
        return CostEstimate {
            amount: None,
            currency: None,
            unknown: vec!["no pricing configured for this model".into()],
        };
    };

    let components = [
        ("input", billed.fresh_input, Some(p.input_per_mtok)),
        ("cache-read", billed.cache_read, p.cache_read_per_mtok),
        ("cache-write", billed.cache_write, p.cache_write_per_mtok),
        ("output", billed.output, Some(p.output_per_mtok)),
    ];
    let mut unknown = Vec::new();
    let mut total = 0.0;
    for (label, tokens, rate) in components {
        match (tokens, rate) {
            // A component nobody used can't need a rate.
            (Some(0), _) => {}
            (Some(n), Some(r)) => total += n as f64 * r / 1_000_000.0,
            (Some(_), None) => unknown.push(format!("no {label} rate configured")),
            (None, _) => unknown.push(format!("provider did not report {label} tokens")),
        }
    }

    if unknown.is_empty() {
        CostEstimate {
            amount: Some(total),
            currency: Some(p.currency.clone()),
            unknown,
        }
    } else {
        CostEstimate {
            amount: None,
            currency: Some(p.currency.clone()),
            unknown,
        }
    }
}

/// Days since 1970-01-01 for a `YYYY-MM-DD` string. `None` for anything
/// that isn't one.
pub fn days_from_iso(s: &str) -> Option<i64> {
    let bytes = s.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let year: i64 = s[0..4].parse().ok()?;
    let month: u32 = s[5..7].parse().ok()?;
    let day: u32 = s[8..10].parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day))
}

/// Howard Hinnant's `days_from_civil`: shift the year to start in March
/// so the leap day lands at the end of the era, then count whole eras.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Today, as days since the Unix epoch. Pre-epoch clocks read as day 0
/// — a machine that far out of sync has bigger problems than pricing.
pub fn today() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.as_secs() / 86_400) as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(model: &str, effective: Option<&str>, input: f64, output: f64) -> PriceEntry {
        PriceEntry {
            model: model.into(),
            effective: effective.map(String::from),
            currency: "USD".into(),
            input_per_mtok: input,
            output_per_mtok: output,
            cache_read_per_mtok: Some(0.1),
            cache_write_per_mtok: Some(1.25),
        }
    }

    fn billed(fresh: u64, read: u64, write: u64, out: u64) -> BilledTokens {
        BilledTokens {
            fresh_input: Some(fresh),
            cache_read: Some(read),
            cache_write: Some(write),
            output: Some(out),
        }
    }

    #[test]
    fn days_from_iso_agrees_with_known_epoch_anchors() {
        assert_eq!(days_from_iso("1970-01-01"), Some(0));
        assert_eq!(days_from_iso("1970-01-02"), Some(1));
        assert_eq!(days_from_iso("2000-03-01"), Some(11017));
        assert_eq!(days_from_iso("2026-08-13"), Some(20678));
    }

    #[test]
    fn a_malformed_date_is_rejected_rather_than_guessed_at() {
        assert_eq!(days_from_iso("2026-8-13"), None);
        assert_eq!(days_from_iso("not-a-date"), None);
        assert_eq!(days_from_iso("2026-13-01"), None);
    }

    #[test]
    fn a_model_with_no_matching_entry_has_no_price() {
        let table = [entry("anthropic/claude", None, 1.0, 5.0)];
        assert!(resolve(&table, "qwen2.5-coder:7b", today()).is_none());
    }

    #[test]
    fn the_latest_effective_card_that_has_already_arrived_wins() {
        let table = [
            entry("m", Some("2026-01-01"), 1.0, 5.0),
            entry("m", Some("2026-06-01"), 2.0, 6.0),
            entry("m", Some("2099-01-01"), 9.0, 9.0),
        ];
        let day = days_from_iso("2026-08-13").unwrap();
        let p = resolve(&table, "m", day).unwrap();
        assert_eq!(p.input_per_mtok, 2.0);
        assert_eq!(p.effective.as_deref(), Some("2026-06-01"));
    }

    #[test]
    fn a_longer_model_prefix_beats_a_shorter_one() {
        let table = [
            entry("anthropic/", None, 1.0, 5.0),
            entry("anthropic/claude-opus", None, 15.0, 75.0),
        ];
        let p = resolve(&table, "anthropic/claude-opus-4-7", today()).unwrap();
        assert_eq!(p.input_per_mtok, 15.0);
        assert_eq!(p.matched, "anthropic/claude-opus");
    }

    #[test]
    fn cost_sums_every_billable_component_at_its_own_rate() {
        let table = [entry("m", None, 3.0, 15.0)];
        let price = resolve(&table, "m", today()).unwrap();
        // 1M fresh @3 + 1M read @0.1 + 1M write @1.25 + 1M out @15.
        let c = cost_of(
            &billed(1_000_000, 1_000_000, 1_000_000, 1_000_000),
            Some(&price),
        );
        assert!((c.amount.unwrap() - 19.35).abs() < 1e-9, "{c:?}");
        assert_eq!(c.currency.as_deref(), Some("USD"));
        assert!(c.unknown.is_empty());
    }

    #[test]
    fn an_unpriced_model_costs_an_explicit_unknown_not_zero() {
        let c = cost_of(&billed(10, 0, 0, 5), None);
        assert_eq!(c.amount, None);
        assert_eq!(c.unknown, vec!["no pricing configured for this model"]);
    }

    #[test]
    fn an_unreported_component_names_itself_instead_of_costing_zero() {
        let table = [entry("m", None, 3.0, 15.0)];
        let price = resolve(&table, "m", today()).unwrap();
        let c = cost_of(
            &BilledTokens {
                fresh_input: Some(10),
                cache_read: None,
                cache_write: Some(0),
                output: Some(5),
            },
            Some(&price),
        );
        assert_eq!(c.amount, None);
        assert_eq!(
            c.unknown,
            vec!["provider did not report cache-read tokens".to_string()]
        );
    }

    #[test]
    fn a_component_the_card_has_no_rate_for_blocks_the_price_only_when_it_is_nonzero() {
        let mut card = entry("m", None, 3.0, 15.0);
        card.cache_read_per_mtok = None;
        let table = [card];
        let price = resolve(&table, "m", today()).unwrap();

        // Nothing was served from cache, so the missing rate can't
        // change the answer.
        let quiet = cost_of(&billed(10, 0, 0, 5), Some(&price));
        let expected = 10.0 * 3.0 / 1e6 + 5.0 * 15.0 / 1e6;
        assert!((quiet.amount.unwrap() - expected).abs() < 1e-12, "{quiet:?}");

        // The moment it's used, the gap matters.
        let loud = cost_of(&billed(10, 900, 0, 5), Some(&price));
        assert_eq!(loud.amount, None);
        assert_eq!(loud.unknown, vec!["no cache-read rate configured"]);
    }
}
