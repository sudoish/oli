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
    LinearWithCompact, Memory, PersistedMemory, list_sessions, new_session_id,
};
use crate::config::Config;
use crate::error::{AgentError, Result};
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
/// then a fresh id for interactive mode, then `None` for
/// ephemeral one-shot mode.
pub fn resolve_session_id(
    resume: Option<&str>,
    continue_latest: bool,
    interactive: bool,
) -> Result<Option<String>> {
    if let Some(id) = resume {
        return Ok(Some(id.to_string()));
    }
    if continue_latest {
        let latest = list_sessions()
            .into_iter()
            .next()
            .ok_or_else(|| AgentError::Config("no prior sessions to continue".into()))?;
        return Ok(Some(latest.id));
    }
    if interactive {
        return Ok(Some(new_session_id()));
    }
    Ok(None)
}

/// Build the agent's memory. With a session id we wrap the
/// linear default in `PersistedMemory`, replaying any existing
/// transcript. Without one, the linear memory stands alone.
///
/// Returns the memory plus the read paths replayed from the
/// session transcript and (when persisted) a `ReadLogger` that
/// mirrors future reads into the same file.
pub async fn build_memory(
    session_id: Option<&str>,
) -> Result<(Box<dyn Memory>, Vec<PathBuf>, Option<Arc<dyn ReadLogger>>)> {
    let inner: Box<dyn Memory> = Box::new(LinearWithCompact::new());
    match session_id {
        Some(id) => {
            let mut persisted = PersistedMemory::open(id, inner).await?;
            let reads = persisted.drain_replayed_reads();
            let logger = persisted.read_logger();
            Ok((Box::new(persisted), reads, Some(logger)))
        }
        None => Ok((inner, Vec::new(), None)),
    }
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

        agent.run(prompt).await
    }
}
