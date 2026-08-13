//! Library-side wiring helpers shared between the binary entry
//! point and any embedder building their own `oli`-flavored agent.
//!
//! Nothing here is on the agent's hot path — these are
//! once-at-startup constructors. The split exists so embedders
//! can reuse the bundled tool set, default subagent spawner, and
//! session-id / memory plumbing without copying the binary's
//! `main.rs`.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use crate::agent::Agent;
use crate::agent::memory::{
    LinearWithCompact, Memory, PersistedMemory, SessionEntry, list_sessions, new_session_id,
};
use crate::config::Config;
use crate::error::{AgentError, Result};
use crate::ledger::{self, Ledger, PromptAccounting, RunIdentity, pricing};
use crate::mcp;
use crate::notes;
use crate::policy::ConfigPolicy;
use crate::providers;
use crate::providers::Provider as ProviderTrait;
use crate::tools::context::ReadLogger;
use crate::tools::task::SubagentSpawner;
use crate::tools::{
    Registry, bash::Bash, edit::Edit, glob::Glob, grep::Grep, read::Read, show_full::ShowFull,
    write::Write,
};

/// Built-in tool set shared between the parent agent and any
/// subagent spawned via `Task`. Excludes `Task` itself so
/// subagents can't recurse (the binary registers `Task`
/// separately on top of this). Includes the notes tools backed
/// by the supplied `NotesStore`.
pub fn build_default_tools(cfg: &Config, notes_store: Arc<dyn notes::NotesStore>) -> Registry {
    let mut tools = Registry::new();
    tools.register(Read);
    tools.register(Write);
    tools.register(Edit);
    tools.register(Bash);
    tools.register(Grep);
    tools.register(Glob);
    tools.register(ShowFull);
    tools.register(crate::tools::notes::WriteNote::new(notes_store.clone()));
    tools.register(crate::tools::notes::SearchNotes::new(notes_store.clone()));
    tools.register(crate::tools::notes::ListNotes::new(notes_store));
    for sub in &cfg.tools.subprocess {
        tools.register(crate::tools::subprocess::SubprocessTool::from_config(sub));
    }
    tools
}

/// Decide which session id to use for this run. Precedence:
/// explicit `resume`, then `continue_latest` (latest by mtime),
/// then a fresh id. Every top-level run is persisted so a later
/// invocation can continue it. Returns (id, is_fresh).
pub fn resolve_session_id(resume: Option<&str>, continue_latest: bool) -> Result<(String, bool)> {
    resolve_session_id_from(resume, continue_latest, &list_sessions(), new_session_id)
}

fn resolve_session_id_from(
    resume: Option<&str>,
    continue_latest: bool,
    sessions: &[SessionEntry],
    fresh_id: impl FnOnce() -> String,
) -> Result<(String, bool)> {
    if let Some(id) = resume {
        let exists = sessions.iter().any(|entry| entry.id == id);
        if !exists {
            return Err(AgentError::Config(format!("unknown conversation: {id}")));
        }
        return Ok((id.to_string(), false));
    }
    if continue_latest {
        let latest = sessions
            .iter()
            .max_by(|a, b| a.mtime.cmp(&b.mtime).then_with(|| a.id.cmp(&b.id)))
            .ok_or_else(|| AgentError::Config("no prior sessions to continue".into()))?;
        return Ok((latest.id.clone(), false));
    }
    Ok((fresh_id(), true))
}

#[cfg(test)]
mod session_tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn entry(id: &str, age: u64) -> SessionEntry {
        SessionEntry {
            id: id.into(),
            path: PathBuf::from(format!("{id}.jsonl")),
            mtime: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(age)),
        }
    }

    #[test]
    fn fresh_runs_allocate_a_new_id() {
        let (id, is_fresh) =
            resolve_session_id_from(None, false, &[], || "fresh-id".into()).unwrap();
        assert_eq!(id, "fresh-id");
        assert!(is_fresh);
    }

    #[test]
    fn explicit_known_conversation_wins() {
        let sessions = [entry("older", 1), entry("wanted", 2)];
        let (id, is_fresh) =
            resolve_session_id_from(Some("wanted"), true, &sessions, || unreachable!()).unwrap();
        assert_eq!(id, "wanted");
        assert!(!is_fresh);
    }

    #[test]
    fn continue_selects_latest_deterministically() {
        let sessions = [entry("newest", 20), entry("older", 10)];
        let (id, is_fresh) =
            resolve_session_id_from(None, true, &sessions, || unreachable!()).unwrap();
        assert_eq!(id, "newest");
        assert!(!is_fresh);
    }

    #[test]
    fn continue_breaks_equal_mtime_ties_by_id() {
        let sessions = [entry("alpha", 20), entry("beta", 20)];
        let (id, is_fresh) =
            resolve_session_id_from(None, true, &sessions, || unreachable!()).unwrap();
        assert_eq!(id, "beta");
        assert!(!is_fresh);
    }

    #[test]
    fn invalid_conversation_fails_without_allocating() {
        let sessions = [entry("known", 1)];
        let error = resolve_session_id_from(Some("missing"), false, &sessions, || {
            panic!("invalid resume must not allocate a transcript id")
        })
        .unwrap_err();
        assert!(error.to_string().contains("unknown conversation: missing"));
    }
}

/// Build the agent's memory. With a session id we wrap the
/// linear default in `PersistedMemory`, replaying any existing
/// transcript. Without one, the linear memory stands alone.
/// `is_fresh` controls whether to allow creation of new session files;
/// when false, an error is returned if the session does not already exist.
///
/// Returns the memory plus the read paths replayed from the
/// session transcript and (when persisted) a `ReadLogger` that
/// mirrors future reads into the same file.
pub async fn build_memory(
    session_id: Option<&str>,
    is_fresh: bool,
    meta: Option<serde_json::Value>,
) -> Result<(Box<dyn Memory>, Vec<PathBuf>, Option<Arc<dyn ReadLogger>>)> {
    let inner: Box<dyn Memory> = Box::new(LinearWithCompact::new());
    match session_id {
        Some(id) => {
            let mut persisted = PersistedMemory::open(id, inner, is_fresh).await?;
            let reads = persisted.drain_replayed_reads();
            let logger = persisted.read_logger();
            if let Some(meta) = meta {
                persisted.write_meta(meta).await?;
            }
            Ok((Box::new(persisted), reads, Some(logger)))
        }
        None => Ok((inner, Vec::new(), None)),
    }
}

/// The memory strategy `build_memory` installs, and the version of what
/// it puts in the context. Recorded on every observation so two runs are
/// only compared when they were built the same way.
pub const MEMORY_STRATEGY: &str = "linear-with-compact";
pub const MEMORY_STRATEGY_VERSION: u32 = 2;

pub struct RunAccountingProfile {
    pub settings: serde_json::Value,
    pub provider_kind: String,
    pub config_hash: String,
    pub accounting: PromptAccounting,
    pub price: Option<crate::ledger::ResolvedPrice>,
}

/// Resolve every runtime input that affects the identity or cost of a
/// request. The REPL reuses this after live provider/model/config swaps,
/// keeping observations comparable to those built at startup.
pub fn run_accounting_profile(
    cfg: &Config,
    provider_name: &str,
    model: &str,
    max_turns: usize,
) -> RunAccountingProfile {
    let caps = crate::agent::caps_for_with_overrides(model, &cfg.caps);
    let ctx_window = caps.ctx_window;
    let provider_kind = cfg
        .provider(provider_name)
        .map(|p| p.kind.clone())
        .unwrap_or_default();
    let price = pricing::resolve(&cfg.pricing, model, pricing::today());
    let settings = serde_json::json!({
        "provider": provider_name,
        "provider_kind": provider_kind,
        "model": model,
        "strategy": MEMORY_STRATEGY,
        "strategy_version": MEMORY_STRATEGY_VERSION,
        "max_turns": max_turns,
        "policy_mode": format!("{:?}", cfg.policy.mode),
        "ctx_window": ctx_window,
        "ctx_window_authoritative": caps.ctx_window_is_authoritative,
        "context_target_tokens": cfg.agent.context_target_tokens,
        "pricing": price,
    });
    let config_hash = ledger::config_hash(&settings);
    RunAccountingProfile {
        settings,
        accounting: PromptAccounting::for_provider_kind(&provider_kind),
        provider_kind,
        config_hash,
        price,
    }
}

#[cfg(test)]
mod accounting_tests {
    use super::*;

    #[test]
    fn resolved_pricing_is_part_of_the_comparability_hash() {
        let mut cfg = Config::env_default();
        cfg.pricing.push(crate::ledger::PriceEntry {
            model: "model".into(),
            effective: None,
            currency: "USD".into(),
            input_per_mtok: 1.0,
            output_per_mtok: 2.0,
            cache_read_per_mtok: Some(0.1),
            cache_write_per_mtok: Some(1.25),
        });
        let before = run_accounting_profile(&cfg, "provider", "model-v1", 40);
        cfg.pricing[0].input_per_mtok = 3.0;
        let after = run_accounting_profile(&cfg, "provider", "model-v1", 40);

        assert_ne!(before.config_hash, after.config_hash);
        assert_ne!(before.settings["pricing"], after.settings["pricing"]);
    }

    #[test]
    fn provider_accounting_convention_is_part_of_the_comparability_hash() {
        let mut cfg = Config::env_default();
        let before = run_accounting_profile(&cfg, "openrouter", "model-v1", 40);
        cfg.providers.get_mut("openrouter").unwrap().kind = "anthropic".into();
        let after = run_accounting_profile(&cfg, "openrouter", "model-v1", 40);

        assert_ne!(before.config_hash, after.config_hash);
        assert_ne!(before.accounting, after.accounting);
    }

    #[test]
    fn active_context_target_is_part_of_the_comparability_hash() {
        let mut cfg = Config::env_default();
        let before = run_accounting_profile(&cfg, "provider", "model-v1", 40);
        cfg.agent.context_target_tokens = Some(2_000);
        let after = run_accounting_profile(&cfg, "provider", "model-v1", 40);

        assert_ne!(before.config_hash, after.config_hash);
        assert_eq!(after.settings["context_target_tokens"], 2_000);
    }
}

/// Keep session identity in transcript metadata and measurements in a sibling ledger.
pub fn build_run_accounting(
    cfg: &Config,
    provider_name: &str,
    model: &str,
    session_id: &str,
    is_fresh: bool,
    max_turns: usize,
) -> (serde_json::Value, Ledger) {
    let profile = run_accounting_profile(cfg, provider_name, model, max_turns);
    let mut meta = profile.settings.clone();
    meta["config_hash"] = serde_json::Value::String(profile.config_hash.clone());

    let identity = RunIdentity {
        session: session_id.to_string(),
        run: crate::agent::memory::new_session_id(),
        provider: provider_name.to_string(),
        model: model.to_string(),
        strategy: MEMORY_STRATEGY.to_string(),
        strategy_version: MEMORY_STRATEGY_VERSION,
        config_hash: profile.config_hash,
    };
    let mut ledger = Ledger::new(identity, profile.accounting)
        .with_price(profile.price)
        .resumed(!is_fresh);
    if let Some(dir) = crate::agent::memory::persisted::sessions_dir() {
        ledger = ledger.with_sink(ledger::ledger_path(&dir, session_id));
    }
    (meta, ledger)
}

/// `SubagentSpawner` impl that builds a fresh agent from config
/// on each call. Each subagent has its own `LinearWithCompact`
/// memory (no persistence — children are ephemeral by design)
/// and inherits the parent's policy + capability registry. The
/// result is the child's final assistant message; intermediate
/// tool steps stay in the child's memory and are discarded when
/// it returns.
pub struct DefaultAgentSpawner {
    pub cfg: Arc<Config>,
    pub provider_name: String,
    pub notes_store: Arc<dyn notes::NotesStore>,
    /// Subagents see the same MCP tools the parent does. We
    /// share the connected handles by `Arc` so spawning a child
    /// doesn't re-dial the servers.
    pub mcp_handles: Arc<Vec<mcp::McpHandle>>,
}

#[async_trait]
impl SubagentSpawner for DefaultAgentSpawner {
    async fn spawn(
        &self,
        prompt: &str,
        max_turns: usize,
        parent_ctx: Option<crate::tools::ToolContext>,
    ) -> Result<String> {
        let model = self.cfg.model_for(&self.provider_name)?;
        let provider: Box<dyn ProviderTrait> = providers::build(&self.cfg, &self.provider_name)?;
        let mut tools = build_default_tools(&self.cfg, self.notes_store.clone());
        for tool in mcp::build_tools(&self.mcp_handles).await {
            tools.register_box(tool);
        }
        let policy = Box::new(ConfigPolicy::from_config(&self.cfg.policy));

        let mut agent = Agent::new(provider, tools, model)
            .with_policy(policy)
            .with_config(self.cfg.clone(), &self.provider_name)
            .with_max_turns(max_turns);

        // Seed the child's read-set + cwd from the parent's
        // snapshot so the child can Edit files the parent has
        // already read without re-reading. `read_logger` is
        // intentionally not propagated — the child's reads are
        // ephemeral and don't belong in the parent's session
        // transcript.
        if let Some(parent) = parent_ctx {
            let child_ctx = agent.tool_context();
            let reads = parent.snapshot_reads().await;
            child_ctx.insert_canonical_reads_with_mtimes(reads).await;
            if let Some(cwd) = parent.cwd().await {
                child_ctx.set_cwd(cwd).await;
            }
        }

        agent.run(prompt).await?.into_completed()
    }
}
