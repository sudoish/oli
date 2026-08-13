//! Per-request accounting: what a run spent, on what, and how long it
//! took.
//!
//! The session JSONL transcript stays the immutable audit and recovery
//! record; measurements do not belong in it. They go to a sibling file,
//! `<session>.ledger.jsonl`, one JSON object per line:
//!
//! - `{"kind":"request", ...}` — one per provider call, with the
//!   preflight estimate, the context attribution, what the provider
//!   reported, latency, and cost.
//! - `{"kind":"summary", ...}` — one at the end of a run, holding only
//!   bounded aggregates.
//!
//! **Local by default.** The request records stay on disk; nothing here
//! transmits anything. Only the summary is shaped for export, and it
//! carries counts and totals — never prompts, tool arguments, file
//! paths, or model output.

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::providers::Usage;

pub mod estimate;
pub mod pricing;

#[cfg(test)]
mod baselines;

pub use estimate::{ContextEstimate, estimate_context};
pub use pricing::{CostEstimate, PriceEntry, ResolvedPrice};

/// Version of the record shapes below. Bump when a consumer would have
/// to change to keep reading them.
pub const SCHEMA: &str = "oli.ledger/1";

/// How a provider's prompt-token count relates to its cache counters.
/// Without this, summing `prompt_tokens` across two providers produces
/// a number that means nothing: one family counts cache hits inside it,
/// the other alongside it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptAccounting {
    /// The prompt count covers only tokens processed fresh. Cache reads
    /// and cache writes are reported separately and are *not* in it.
    CacheExclusive,
    /// The prompt count already contains the cache-read tokens.
    CacheInclusive,
    #[default]
    Unknown,
}

impl PromptAccounting {
    /// The convention a provider kind follows. This is the one place the
    /// mapping lives; everything downstream reads the neutral property.
    pub fn for_provider_kind(kind: &str) -> Self {
        match kind {
            "anthropic" => Self::CacheExclusive,
            "openai-compat" | "openai-chatgpt" => Self::CacheInclusive,
            _ => Self::Unknown,
        }
    }
}

/// Provider-reported tokens split into the pieces that bill at
/// different rates. `None` means the provider did not report it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BilledTokens {
    /// Input processed fresh: neither served from cache nor stored into
    /// it.
    pub fresh_input: Option<u64>,
    pub cache_read: Option<u64>,
    pub cache_write: Option<u64>,
    pub output: Option<u64>,
}

impl BilledTokens {
    pub fn from_usage(usage: Option<Usage>, accounting: PromptAccounting) -> Self {
        let Some(u) = usage else {
            return Self::default();
        };
        let prompt = u.prompt_tokens.map(u64::from);
        let read = u.cache_read_tokens.map(u64::from);
        let write = u.cache_write_tokens.map(u64::from);
        let fresh = match accounting {
            PromptAccounting::CacheExclusive => prompt,
            // Subtracting requires knowing both parts. Guessing that an
            // unreported cache counter means zero would quietly bill
            // cache hits at the full input rate.
            PromptAccounting::CacheInclusive => match (prompt, read, write) {
                (Some(p), Some(r), Some(w)) => Some(p.saturating_sub(r).saturating_sub(w)),
                _ => None,
            },
            PromptAccounting::Unknown => None,
        };
        Self {
            fresh_input: fresh,
            cache_read: read,
            cache_write: write,
            output: u.completion_tokens.map(u64::from),
        }
    }

    /// Input the provider read for the first time on this call: fresh
    /// input plus whatever it stored into cache. This is the figure that
    /// compares across providers — `prompt_tokens` does not.
    pub fn uncached_input(&self) -> Option<u64> {
        Some(self.fresh_input?.saturating_add(self.cache_write?))
    }

    /// Every input token the provider processed, cached or not.
    pub fn total_input(&self) -> Option<u64> {
        Some(
            self.fresh_input?
                .saturating_add(self.cache_read?)
                .saturating_add(self.cache_write?),
        )
    }
}

/// What the provider said about one call, with its counting convention
/// and the two cross-provider figures derived from it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportedTokens {
    pub prompt: Option<u32>,
    pub completion: Option<u32>,
    /// The provider's own total, verbatim. Not comparable across
    /// providers; see `accounting`.
    pub total: Option<u32>,
    pub cache_read: Option<u32>,
    pub cache_write: Option<u32>,
    pub reasoning: Option<u32>,
    pub accounting: PromptAccounting,
    pub billed: BilledTokens,
    pub uncached_input: Option<u64>,
    pub total_input: Option<u64>,
}

impl ReportedTokens {
    pub fn new(usage: Option<Usage>, accounting: PromptAccounting) -> Self {
        let billed = BilledTokens::from_usage(usage, accounting);
        let u = usage.unwrap_or_default();
        Self {
            prompt: u.prompt_tokens,
            completion: u.completion_tokens,
            total: u.total_tokens,
            cache_read: u.cache_read_tokens,
            cache_write: u.cache_write_tokens,
            reasoning: u.reasoning_tokens,
            accounting,
            billed,
            uncached_input: billed.uncached_input(),
            total_input: billed.total_input(),
        }
    }
}

/// Wall-clock for one request, in milliseconds. Zero here means
/// measured-as-zero; anything not measurable is `None`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Latency {
    /// Assembling the request: compaction, snapshot, tool schemas.
    pub context_build_ms: u64,
    /// The compaction pass alone. Included in `context_build_ms`.
    pub compaction_ms: u64,
    /// Dispatch to first content token. `None` when the provider
    /// streamed no text — a pure tool-call turn exposes nothing to time.
    pub ttft_ms: Option<u64>,
    /// Dispatch to fully assembled response.
    pub model_ms: u64,
    /// Every tool this turn dispatched, summed.
    pub tool_ms: u64,
}

/// One provider call, accounted end to end.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RequestObservation {
    pub kind: String,
    pub schema: String,
    pub session: String,
    pub run: String,
    /// Model turn within the run that issued it, 1-based.
    pub turn: u32,
    /// Provider call within this run, 1-based.
    pub call: u32,
    pub provider: String,
    pub model: String,
    pub strategy: String,
    pub strategy_version: u32,
    pub config_hash: String,
    pub started_at_ms: u64,
    pub estimated: ContextEstimate,
    pub reported: ReportedTokens,
    pub latency: Latency,
    pub cost: CostEstimate,
}

/// Identity carried by run summaries and used as the active template for
/// new observations. Session/run/strategy stay fixed; provider, model,
/// accounting inputs, and config hash can refresh after a live REPL swap.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RunIdentity {
    pub session: String,
    /// Unique invocation within the append-only session ledger. Call and
    /// turn counters restart for each run, so consumers join on this id.
    pub run: String,
    pub provider: String,
    pub model: String,
    /// Memory strategy in force, by name.
    pub strategy: String,
    /// Bumped when a strategy changes what it puts in the context, so
    /// two runs are only comparable when this matches.
    pub strategy_version: u32,
    /// Digest of the settings that shape a run. Two runs with the same
    /// hash saw the same knobs.
    pub config_hash: String,
}

/// Aggregate for one token category across a run. Mirrors the reported/
/// unreported split the provider layer uses: a sum says how much of the
/// run it covers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rollup {
    pub tokens: Option<u64>,
    pub reported_calls: u32,
    pub unreported_calls: u32,
}

impl Rollup {
    fn add(&mut self, v: Option<u64>) {
        match v {
            Some(n) => {
                self.tokens = Some(self.tokens.unwrap_or(0) + n);
                self.reported_calls += 1;
            }
            None => self.unreported_calls += 1,
        }
    }
}

/// Token aggregates for a run. Every figure here is comparable across
/// providers because it is built from `BilledTokens`, not from a
/// provider's own total.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenRollup {
    pub fresh_input: Rollup,
    pub cache_read: Rollup,
    pub cache_write: Rollup,
    pub output: Rollup,
    /// Fresh input plus cache writes — input read for the first time.
    pub uncached_input: Rollup,
    pub total_input: Rollup,
}

/// Preflight estimates summed over the run, per context category.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextRollup {
    pub pinned: u64,
    pub tool_schemas: u64,
    pub summary: u64,
    pub recent: u64,
    pub total: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LatencyRollup {
    pub context_build_ms: u64,
    pub compaction_ms: u64,
    pub model_ms: u64,
    pub tool_ms: u64,
    /// Start of the run to the summary. Covers everything, including
    /// time no other bucket claims.
    pub total_ms: u64,
    /// Fastest first token observed. `None` when nothing streamed text.
    pub best_ttft_ms: Option<u64>,
    pub calls_without_ttft: u32,
}

/// Money for a run. `amount` sums only the calls that could be priced;
/// `unpriced_calls` says how many were left out, so a partial total
/// never reads as a complete one.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CostRollup {
    pub amount: Option<f64>,
    pub currency: Option<String>,
    pub priced_calls: u32,
    pub unpriced_calls: u32,
    /// Deduped reasons a call could not be priced.
    pub unknown: Vec<String>,
    pub price: Option<ResolvedPrice>,
}

/// A single request, reduced to the figures worth exporting.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RequestDigest {
    pub call: u32,
    pub turn: u32,
    pub estimated: ContextEstimate,
    pub uncached_input: Option<u64>,
    pub total_input: Option<u64>,
    pub output: Option<u64>,
    pub latency: Latency,
    pub cost: CostEstimate,
}

/// The machine-readable end-of-run summary. Bounded aggregates only —
/// this is the shape that is safe to hand to anything outside the
/// machine that produced it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunSummary {
    pub kind: String,
    pub schema: String,
    #[serde(flatten)]
    pub identity: RunIdentity,
    pub accounting: PromptAccounting,
    /// Whether this run continued an existing transcript.
    pub resumed: bool,
    pub calls: u32,
    pub turns: u32,
    pub tokens: TokenRollup,
    pub context: ContextRollup,
    pub latency_ms: LatencyRollup,
    pub cost: CostRollup,
    /// The run's first provider call. On a resumed session this is what
    /// picking the conversation back up cost before any new work.
    pub first_request: Option<RequestDigest>,
}

/// Collects observations for one run, writes them to the session's
/// sibling ledger file, and reduces them to a summary on demand.
pub struct Ledger {
    identity: RunIdentity,
    accounting: PromptAccounting,
    price: Option<ResolvedPrice>,
    /// `None` keeps everything in memory — the default for agents with
    /// no persisted session (subagents, tests).
    path: Option<PathBuf>,
    resumed: bool,
    started: Instant,
    observations: Vec<RequestObservation>,
}

impl Default for Ledger {
    fn default() -> Self {
        Self {
            identity: RunIdentity::default(),
            accounting: PromptAccounting::Unknown,
            price: None,
            path: None,
            resumed: false,
            started: Instant::now(),
            observations: Vec::new(),
        }
    }
}

impl Ledger {
    pub fn new(identity: RunIdentity, accounting: PromptAccounting) -> Self {
        Self {
            identity,
            accounting,
            ..Self::default()
        }
    }

    /// Price this run's calls with `price`. Without one, every call
    /// costs an explicit unknown.
    pub fn with_price(mut self, price: Option<ResolvedPrice>) -> Self {
        self.price = price;
        self
    }

    /// Append request records to `path` as well as holding them in
    /// memory.
    pub fn with_sink(mut self, path: PathBuf) -> Self {
        self.path = Some(path);
        self
    }

    /// Mark the run as continuing an existing transcript, which is what
    /// makes the first request's cost worth singling out.
    pub fn resumed(mut self, resumed: bool) -> Self {
        self.resumed = resumed;
        self
    }

    pub fn identity(&self) -> &RunIdentity {
        &self.identity
    }

    pub fn observations(&self) -> &[RequestObservation] {
        &self.observations
    }

    pub fn path(&self) -> Option<&std::path::Path> {
        self.path.as_deref()
    }

    /// Next model turn across repeated `Agent::run` calls in this run.
    pub fn next_turn(&self) -> u32 {
        self.observations
            .iter()
            .map(|observation| observation.turn)
            .max()
            .unwrap_or(0)
            .saturating_add(1)
    }

    /// Refresh the identity and billing inputs used by subsequent
    /// observations after an interactive provider, model, or config
    /// swap. Existing observations retain the values active when they
    /// were sent.
    pub fn reconfigure(
        &mut self,
        provider: String,
        model: String,
        config_hash: String,
        accounting: PromptAccounting,
        price: Option<ResolvedPrice>,
    ) {
        self.identity.provider = provider;
        self.identity.model = model;
        self.identity.config_hash = config_hash;
        self.accounting = accounting;
        self.price = price;
    }

    /// Account for one provider call and append it to the sink. Write
    /// failures land in diagnostics: losing a measurement must never
    /// take down the run it was measuring.
    pub async fn record(
        &mut self,
        turn: u32,
        estimated: ContextEstimate,
        usage: Option<Usage>,
        latency: Latency,
    ) {
        self.record_started(now_ms(), turn, estimated, usage, latency)
            .await;
    }

    /// Account for a provider call whose dispatch timestamp was captured
    /// by the caller immediately before sending the request.
    pub async fn record_started(
        &mut self,
        started_at_ms: u64,
        turn: u32,
        estimated: ContextEstimate,
        usage: Option<Usage>,
        latency: Latency,
    ) {
        let reported = ReportedTokens::new(usage, self.accounting);
        let cost = pricing::cost_of(&reported.billed, self.price.as_ref());
        let obs = RequestObservation {
            kind: "request".into(),
            schema: SCHEMA.into(),
            session: self.identity.session.clone(),
            run: self.identity.run.clone(),
            turn,
            call: self.observations.len() as u32 + 1,
            provider: self.identity.provider.clone(),
            model: self.identity.model.clone(),
            strategy: self.identity.strategy.clone(),
            strategy_version: self.identity.strategy_version,
            config_hash: self.identity.config_hash.clone(),
            started_at_ms,
            estimated,
            reported,
            latency,
            cost,
        };
        self.append(&obs).await;
        self.observations.push(obs);
    }

    /// Reduce the run to bounded aggregates.
    pub fn summary(&self) -> RunSummary {
        let mut tokens = TokenRollup::default();
        let mut context = ContextRollup::default();
        let mut latency = LatencyRollup::default();
        let mut cost = CostRollup {
            currency: self.price.as_ref().map(|p| p.currency.clone()),
            price: self.price.clone(),
            ..CostRollup::default()
        };
        let mut turns = 0;

        for o in &self.observations {
            turns = turns.max(o.turn);

            tokens.fresh_input.add(o.reported.billed.fresh_input);
            tokens.cache_read.add(o.reported.billed.cache_read);
            tokens.cache_write.add(o.reported.billed.cache_write);
            tokens.output.add(o.reported.billed.output);
            tokens.uncached_input.add(o.reported.uncached_input);
            tokens.total_input.add(o.reported.total_input);

            context.pinned += o.estimated.pinned;
            context.tool_schemas += o.estimated.tool_schemas;
            context.summary += o.estimated.summary;
            context.recent += o.estimated.recent;
            context.total += o.estimated.total;

            latency.context_build_ms += o.latency.context_build_ms;
            latency.compaction_ms += o.latency.compaction_ms;
            latency.model_ms += o.latency.model_ms;
            latency.tool_ms += o.latency.tool_ms;
            match o.latency.ttft_ms {
                Some(t) => {
                    latency.best_ttft_ms = Some(latency.best_ttft_ms.map_or(t, |b| b.min(t)))
                }
                None => latency.calls_without_ttft += 1,
            }

            match o.cost.amount {
                Some(a) => {
                    cost.amount = Some(cost.amount.unwrap_or(0.0) + a);
                    cost.priced_calls += 1;
                }
                None => cost.unpriced_calls += 1,
            }
            for reason in &o.cost.unknown {
                if !cost.unknown.contains(reason) {
                    cost.unknown.push(reason.clone());
                }
            }
        }
        latency.total_ms = self.started.elapsed().as_millis() as u64;

        RunSummary {
            kind: "summary".into(),
            schema: SCHEMA.into(),
            identity: self.identity.clone(),
            accounting: self.accounting,
            resumed: self.resumed,
            calls: self.observations.len() as u32,
            turns,
            tokens,
            context,
            latency_ms: latency,
            cost,
            first_request: self.observations.first().map(digest),
        }
    }

    /// Build the summary and append it to the sink.
    pub async fn finish(&self) -> RunSummary {
        let summary = self.summary();
        self.append(&summary).await;
        summary
    }

    async fn append<T: Serialize>(&self, record: &T) {
        let Some(path) = &self.path else {
            return;
        };
        let Ok(mut line) = serde_json::to_string(record) else {
            return;
        };
        line.push('\n');
        if let Err(e) = append_line(path, &line).await {
            crate::diagnostics::push(
                crate::diagnostics::Level::Warn,
                format!("ledger write failed ({}): {e}", path.display()),
            );
        }
    }
}

fn digest(o: &RequestObservation) -> RequestDigest {
    RequestDigest {
        call: o.call,
        turn: o.turn,
        estimated: o.estimated,
        uncached_input: o.reported.uncached_input,
        total_input: o.reported.total_input,
        output: o.reported.billed.output,
        latency: o.latency,
        cost: o.cost.clone(),
    }
}

async fn append_line(path: &std::path::Path, line: &str) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    f.write_all(line.as_bytes()).await?;
    f.flush().await
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn as_ms(d: Duration) -> u64 {
    d.as_millis() as u64
}

/// Ledger file for a session, beside its transcript. A distinct suffix
/// rather than a distinct directory so everything about one session
/// sits together; `list_sessions` skips it.
pub fn ledger_path(sessions_dir: &std::path::Path, session_id: &str) -> PathBuf {
    sessions_dir.join(format!("{session_id}{LEDGER_SUFFIX}"))
}

pub const LEDGER_SUFFIX: &str = ".ledger.jsonl";

/// Digest of the settings that shape a run's cost and shape. Two runs
/// that agree here saw the same knobs, which is what makes their
/// measurements comparable.
pub fn config_hash(settings: &Value) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(canonical(settings).as_bytes());
    format!("{:x}", hasher.finalize())[..16].to_string()
}

/// JSON with object keys in sorted order, so two settings that differ
/// only in field order hash the same.
fn canonical(v: &Value) -> String {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let body: Vec<String> = keys
                .iter()
                .map(|k| format!("{}:{}", Value::String((*k).clone()), canonical(&map[*k])))
                .collect();
            format!("{{{}}}", body.join(","))
        }
        Value::Array(items) => {
            let body: Vec<String> = items.iter().map(canonical).collect();
            format!("[{}]", body.join(","))
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn usage(prompt: u32, completion: u32, read: Option<u32>, write: Option<u32>) -> Usage {
        Usage {
            prompt_tokens: Some(prompt),
            completion_tokens: Some(completion),
            total_tokens: Some(prompt + completion),
            cache_read_tokens: read,
            cache_write_tokens: write,
            reasoning_tokens: None,
        }
    }

    #[test]
    fn a_cache_exclusive_provider_adds_its_cache_counters_to_the_input_it_reports() {
        // The reviewer's case: 100 fresh + 900 cache-read + 40
        // cache-write + 25 output, reported as prompt=100.
        let billed = BilledTokens::from_usage(
            Some(usage(100, 25, Some(900), Some(40))),
            PromptAccounting::CacheExclusive,
        );
        assert_eq!(billed.fresh_input, Some(100));
        assert_eq!(billed.uncached_input(), Some(140));
        assert_eq!(billed.total_input(), Some(1040));
    }

    #[test]
    fn a_cache_inclusive_provider_has_its_cache_counters_taken_back_out() {
        // Same call as above under OpenAI's convention: prompt=1040
        // already contains the 900 read and the 40 written.
        let billed = BilledTokens::from_usage(
            Some(usage(1040, 25, Some(900), Some(40))),
            PromptAccounting::CacheInclusive,
        );
        assert_eq!(billed.fresh_input, Some(100));
        assert_eq!(billed.uncached_input(), Some(140));
        assert_eq!(billed.total_input(), Some(1040));
    }

    #[test]
    fn the_two_conventions_agree_on_the_cross_provider_figures() {
        let exclusive = BilledTokens::from_usage(
            Some(usage(100, 25, Some(900), Some(40))),
            PromptAccounting::CacheExclusive,
        );
        let inclusive = BilledTokens::from_usage(
            Some(usage(1040, 25, Some(900), Some(40))),
            PromptAccounting::CacheInclusive,
        );
        assert_eq!(exclusive.uncached_input(), inclusive.uncached_input());
        assert_eq!(exclusive.total_input(), inclusive.total_input());
    }

    #[test]
    fn an_unknown_convention_refuses_to_split_the_prompt_count() {
        let billed = BilledTokens::from_usage(
            Some(usage(100, 25, Some(900), Some(40))),
            PromptAccounting::Unknown,
        );
        assert_eq!(billed.fresh_input, None);
        assert_eq!(billed.uncached_input(), None);
        // The raw counters still survive for a reader who knows better.
        assert_eq!(billed.cache_read, Some(900));
    }

    #[test]
    fn an_unreported_cache_counter_leaves_the_split_unknown_rather_than_zero() {
        let billed = BilledTokens::from_usage(
            Some(usage(1040, 25, None, None)),
            PromptAccounting::CacheInclusive,
        );
        assert_eq!(billed.fresh_input, None);
        assert_eq!(billed.uncached_input(), None);
    }

    #[test]
    fn a_call_the_provider_said_nothing_about_reports_nothing() {
        let r = ReportedTokens::new(None, PromptAccounting::CacheExclusive);
        assert_eq!(r.prompt, None);
        assert_eq!(r.uncached_input, None);
        assert_eq!(r.total_input, None);
    }

    #[tokio::test]
    async fn a_run_summary_totals_only_what_was_reported_and_counts_the_rest() {
        let mut ledger = Ledger::new(
            RunIdentity {
                session: "s".into(),
                run: "r".into(),
                provider: "p".into(),
                model: "m".into(),
                strategy: "linear-with-compact".into(),
                strategy_version: 1,
                config_hash: "abc".into(),
            },
            PromptAccounting::CacheExclusive,
        );
        ledger
            .record(
                1,
                ContextEstimate {
                    pinned: 10,
                    tool_schemas: 5,
                    summary: 0,
                    recent: 20,
                    total: 35,
                },
                Some(usage(100, 25, Some(0), Some(0))),
                Latency::default(),
            )
            .await;
        ledger
            .record(2, ContextEstimate::default(), None, Latency::default())
            .await;

        let s = ledger.summary();
        assert_eq!(s.calls, 2);
        assert_eq!(s.turns, 2);
        assert_eq!(s.tokens.uncached_input.tokens, Some(100));
        assert_eq!(s.tokens.uncached_input.unreported_calls, 1);
        assert_eq!(s.context.pinned, 10);
        assert_eq!(s.context.total, 35);
    }

    #[tokio::test]
    async fn an_unpriced_run_reports_an_unknown_cost_never_a_zero_one() {
        let mut ledger = Ledger::new(RunIdentity::default(), PromptAccounting::CacheExclusive);
        ledger
            .record(
                1,
                ContextEstimate::default(),
                Some(usage(10, 5, Some(0), Some(0))),
                Latency::default(),
            )
            .await;
        let s = ledger.summary();
        assert_eq!(s.cost.amount, None);
        assert_eq!(s.cost.priced_calls, 0);
        assert_eq!(s.cost.unpriced_calls, 1);
        assert_eq!(s.cost.unknown, vec!["no pricing configured for this model"]);
    }

    #[tokio::test]
    async fn an_unstreamed_turn_leaves_time_to_first_token_unknown_not_zero() {
        let mut ledger = Ledger::new(RunIdentity::default(), PromptAccounting::Unknown);
        ledger
            .record(
                1,
                ContextEstimate::default(),
                None,
                Latency {
                    ttft_ms: None,
                    ..Latency::default()
                },
            )
            .await;
        let s = ledger.summary();
        assert_eq!(s.latency_ms.best_ttft_ms, None);
        assert_eq!(s.latency_ms.calls_without_ttft, 1);
    }

    #[tokio::test]
    async fn a_resumed_run_singles_out_what_its_first_request_cost() {
        let mut ledger = Ledger::new(RunIdentity::default(), PromptAccounting::CacheExclusive)
            .with_price(Some(ResolvedPrice {
                matched: "".into(),
                effective: Some("2026-01-01".into()),
                currency: "USD".into(),
                input_per_mtok: 3.0,
                output_per_mtok: 15.0,
                cache_read_per_mtok: Some(0.3),
                cache_write_per_mtok: Some(3.75),
            }))
            .resumed(true);
        ledger
            .record(
                1,
                ContextEstimate {
                    pinned: 1_000,
                    tool_schemas: 500,
                    summary: 0,
                    recent: 8_000,
                    total: 9_500,
                },
                Some(usage(9_400, 30, Some(0), Some(0))),
                Latency::default(),
            )
            .await;
        ledger
            .record(
                2,
                ContextEstimate::default(),
                Some(usage(20, 5, Some(0), Some(0))),
                Latency::default(),
            )
            .await;

        let s = ledger.summary();
        assert!(s.resumed);
        let first = s.first_request.expect("first request is singled out");
        assert_eq!(first.call, 1);
        assert_eq!(first.estimated.total, 9_500);
        assert_eq!(first.uncached_input, Some(9_400));
        let expected = 9_400.0 * 3.0 / 1e6 + 30.0 * 15.0 / 1e6;
        assert!((first.cost.amount.unwrap() - expected).abs() < 1e-12);
    }

    #[tokio::test]
    async fn the_summary_is_machine_readable_and_round_trips() {
        let mut ledger = Ledger::new(
            RunIdentity {
                session: "s1".into(),
                run: "r1".into(),
                provider: "test".into(),
                model: "test-model".into(),
                strategy: "linear-with-compact".into(),
                strategy_version: 1,
                config_hash: "deadbeef".into(),
            },
            PromptAccounting::CacheInclusive,
        );
        ledger
            .record(
                1,
                ContextEstimate::default(),
                Some(usage(10, 5, Some(0), Some(0))),
                Latency::default(),
            )
            .await;

        let s = ledger.summary();
        let encoded = serde_json::to_string(&s).unwrap();
        let value: Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(value["kind"], "summary");
        assert_eq!(value["schema"], SCHEMA);
        // `identity` is flattened, so consumers read it at the top level.
        assert_eq!(value["session"], "s1");
        assert_eq!(value["model"], "test-model");
        assert_eq!(value["accounting"], "cache_inclusive");
        let back: RunSummary = serde_json::from_str(&encoded).unwrap();
        assert_eq!(back, s);
    }

    #[tokio::test]
    async fn records_land_in_the_sink_one_json_object_per_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = ledger_path(dir.path(), "sess-1");
        let mut ledger =
            Ledger::new(RunIdentity::default(), PromptAccounting::Unknown).with_sink(path.clone());
        ledger
            .record(1, ContextEstimate::default(), None, Latency::default())
            .await;
        ledger.finish().await;

        let body = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2);
        let first: Value = serde_json::from_str(lines[0]).unwrap();
        let second: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(first["kind"], "request");
        assert_eq!(second["kind"], "summary");
        assert!(path.to_string_lossy().ends_with(".ledger.jsonl"));
    }

    #[tokio::test]
    async fn a_caller_supplied_dispatch_timestamp_is_preserved_exactly() {
        let mut ledger = Ledger::new(RunIdentity::default(), PromptAccounting::Unknown);
        ledger
            .record_started(
                123_456,
                1,
                ContextEstimate::default(),
                None,
                Latency::default(),
            )
            .await;
        assert_eq!(ledger.observations()[0].started_at_ms, 123_456);
    }

    #[tokio::test]
    async fn reconfiguration_changes_future_observations_without_rewriting_history() {
        let mut ledger = Ledger::new(
            RunIdentity {
                provider: "old".into(),
                model: "old-model".into(),
                config_hash: "old-hash".into(),
                ..RunIdentity::default()
            },
            PromptAccounting::CacheExclusive,
        );
        ledger
            .record(1, ContextEstimate::default(), None, Latency::default())
            .await;
        ledger.reconfigure(
            "new".into(),
            "new-model".into(),
            "new-hash".into(),
            PromptAccounting::CacheInclusive,
            None,
        );
        ledger
            .record(2, ContextEstimate::default(), None, Latency::default())
            .await;

        let observations = ledger.observations();
        assert_eq!(observations[0].provider, "old");
        assert_eq!(observations[0].config_hash, "old-hash");
        assert_eq!(
            observations[0].reported.accounting,
            PromptAccounting::CacheExclusive
        );
        assert_eq!(observations[1].provider, "new");
        assert_eq!(observations[1].model, "new-model");
        assert_eq!(observations[1].config_hash, "new-hash");
        assert_eq!(
            observations[1].reported.accounting,
            PromptAccounting::CacheInclusive
        );
    }

    #[test]
    fn a_config_hash_ignores_key_order_but_not_values() {
        let a = config_hash(&json!({"model": "m", "max_turns": 40}));
        let b = config_hash(&json!({"max_turns": 40, "model": "m"}));
        let c = config_hash(&json!({"max_turns": 41, "model": "m"}));
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn provider_kinds_map_onto_their_counting_convention() {
        assert_eq!(
            PromptAccounting::for_provider_kind("anthropic"),
            PromptAccounting::CacheExclusive
        );
        assert_eq!(
            PromptAccounting::for_provider_kind("openai-compat"),
            PromptAccounting::CacheInclusive
        );
        assert_eq!(
            PromptAccounting::for_provider_kind("something-new"),
            PromptAccounting::Unknown
        );
    }
}
