//! The agent loop — `Agent` ties together a `Provider`, a `Memory`,
//! a `tools::Registry`, and the `Policy`/`Approver` gate into the
//! think → call → observe loop the model drives.
//!
//! Two entry points:
//! - [`Agent::run`] — one-shot prompt; returns a typed terminal outcome.
//!   Used by the headless CLI and nested agents.
//! - [`Agent::run_streaming`] — same loop but emits incremental
//!   events (content chunks, tool starts/ends, usage updates) to a
//!   user-supplied callback. Drives headless runs and the line REPL.
//!
//! `Agent::with_*` builder methods layer on optional pieces
//! (memory strategy, hook registry, MCP handles, plugin manifest,
//! per-run turn cap). The builder is consumed by `pin_system_prompt`
//! which seals in the system prompt before the first turn.
//!
//! Submodules:
//! - [`memory`] — `Memory` trait and bundled implementations.
//! - [`context`] — system-prompt builder (env, git, AGENTS.md, CLAUDE.md).
//! - [`caps`] — model-capability table (context window, native
//!   tools yes/no, streaming, etc.).

use std::sync::Arc;
use std::time::Instant;

use serde_json::{Value, json};

use crate::config::Config;
use crate::error::Result;
use crate::hooks::{HookRegistry, PreToolDecision};
use crate::ledger::{Latency, Ledger, as_ms, estimate_context, now_ms};
use crate::policy::{AlwaysApprove, Approver, ConfigPolicy, Decision, Policy};
use crate::providers::{ChatRequest, Provider, StreamEvent, StreamSink, Usage, UsageTotals};
use crate::tools::{Registry, ToolContext};

/// Typed terminal state for one agent invocation. Callers that need to
/// distinguish a model completion from a safety-boundary stop can inspect
/// this enum returned by [`Agent::run`] and [`Agent::run_streaming`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    Completed(String),
    MaxTurnsExhausted { limit: usize, message: String },
}

impl RunOutcome {
    /// Require a completed response at a non-interactive composition boundary.
    pub fn into_completed(self) -> Result<String> {
        match self {
            Self::Completed(text) => Ok(text),
            Self::MaxTurnsExhausted { limit, .. } => {
                Err(crate::error::AgentError::MaxTurnsExhausted(limit))
            }
        }
    }
}

pub mod caps;
pub mod context;
pub mod memory;
pub mod tool_parse;

pub use caps::{ModelCaps, caps_for, caps_for_with_overrides};
pub use memory::{CompactContext, CompactionReport, LinearWithCompact, Memory};

pub struct Agent {
    pub provider: Box<dyn Provider>,
    /// Name of the active provider in `cfg.providers`. Empty when the
    /// agent was constructed without a Config (tests). Used by
    /// `/provider` to know which entry is current and by `/model` to
    /// look up the right `base_url` for endpoint queries.
    pub provider_name: String,
    pub tools: Registry,
    pub model: String,
    /// Capabilities resolved from the model id at construction. Drives the
    /// compaction target and (in step 5) whether the tool-call fallback
    /// parser engages.
    pub caps: ModelCaps,
    /// Active-context memory. Default is `LinearWithCompact`. Swap via
    /// `with_memory` to plug in alternative strategies (RAG, graph,
    /// hierarchical) without touching the agent loop.
    pub memory: Box<dyn Memory>,
    /// Most recent per-call token accounting reported by the provider.
    /// Populated after every chat round (streaming and non-streaming).
    /// Drives `Memory::maybe_compact` decisions and powers the
    /// last-call line of the `/cost` slash command.
    pub last_usage: Option<Usage>,
    /// Running total of every per-call usage since session start (or
    /// the last `clear`). `/cost` renders this alongside `last_usage`
    /// so the user can spot a session that's drifting expensive. Calls
    /// a provider didn't report are counted, not summed as zero.
    pub session_usage: UsageTotals,
    /// Per-request accounting for this run: preflight estimate, context
    /// attribution, reported tokens, latency, cost. Detail stays local
    /// (in memory, and on disk when a session sink is attached); only
    /// `Ledger::summary` is shaped for export.
    pub ledger: Ledger,
    /// Gate every tool call. Default is `ConfigPolicy::defaults()` (Read /
    /// Glob / Grep auto-allow, Edit / Write / Bash ask, common dev shell
    /// commands on the bash allowlist).
    pub policy: Box<dyn Policy>,
    /// Resolves `Decision::Ask` outcomes. Default is `AlwaysApprove` so
    /// non-interactive scripted invocations don't deadlock; the REPL
    /// swaps in `ReadlineApprover` at startup.
    pub approver: Box<dyn Approver>,
    /// Optional handle on the parsed configuration. The `/provider` and
    /// `/model` slash commands need it to enumerate alternatives and
    /// build new providers. Tests construct agents without a config.
    pub cfg: Option<Arc<Config>>,
    /// Lifecycle hook dispatcher. Empty by default; populated by the
    /// binary at startup with built-in hooks (Phase 3) and by the
    /// plugin runtime (Phase 3 step 4) with user-authored Lua hooks.
    pub hooks: HookRegistry,
    /// Bound on the number of model turns per `run` invocation.
    /// `None` means unbounded — the default for top-level interactive
    /// runs. Subagents set this to a small number so a runaway child
    /// can't spin forever.
    pub max_turns: Option<usize>,
    /// Plugin manifest captured at startup (or after `/plugins reload`).
    /// `/plugins` introspects this to render its listing.
    pub plugin_manifest: Vec<crate::plugins::PluginManifest>,
    /// MCP server handles. The agent loop drains each one's
    /// `tools_changed` flag per turn and re-syncs the registry so a
    /// server that pushes `notifications/tools/list_changed` mid-
    /// session has its new tools become callable on the next
    /// model turn. Empty by default; the binary populates this at
    /// startup.
    pub mcp_handles: Arc<Vec<crate::mcp::McpHandle>>,
    ctx: ToolContext,
}

impl Agent {
    pub fn new(provider: Box<dyn Provider>, tools: Registry, model: String) -> Self {
        let caps = caps_for(&model);
        Self {
            provider,
            provider_name: String::new(),
            tools,
            model,
            caps,
            memory: Box::new(LinearWithCompact::new()),
            last_usage: None,
            session_usage: UsageTotals::default(),
            ledger: Ledger::default(),
            policy: Box::new(ConfigPolicy::defaults()),
            approver: Box::new(AlwaysApprove),
            cfg: None,
            hooks: HookRegistry::new(),
            max_turns: None,
            plugin_manifest: Vec::new(),
            mcp_handles: Arc::new(Vec::new()),
            ctx: ToolContext::new(),
        }
    }

    /// Bind MCP server handles to this agent. The loop drains each
    /// server's `tools_changed` flag per turn and re-syncs the
    /// registry. Builder; tests typically don't need this.
    pub fn with_mcp_handles(mut self, handles: Arc<Vec<crate::mcp::McpHandle>>) -> Self {
        self.mcp_handles = handles;
        self
    }

    /// Stash plugin metadata for later introspection by `/plugins`.
    pub fn with_plugin_manifest(mut self, manifest: Vec<crate::plugins::PluginManifest>) -> Self {
        self.plugin_manifest = manifest;
        self
    }

    /// Attach a pre-built accounting ledger. Builder; the binary uses it
    /// to bind a session's on-disk sink and the resolved rate card.
    pub fn with_ledger(mut self, ledger: Ledger) -> Self {
        self.ledger = ledger;
        self
    }

    /// Cap turns for this agent. Used by subagent spawning to keep a
    /// child's loop bounded. Builder.
    pub fn with_max_turns(mut self, n: usize) -> Self {
        self.max_turns = Some(n);
        self
    }

    /// Replace the hook registry. Builder; lets the binary or plugin
    /// loader inject pre-populated hooks at construction.
    pub fn with_hooks(mut self, hooks: HookRegistry) -> Self {
        self.hooks = hooks;
        self
    }

    /// Bind the agent to a Config and the name of the currently-active
    /// provider entry in it. Required for `/provider` (to enumerate and
    /// swap) and helpful for `/model` (to look up the active base URL
    /// when listing models).
    ///
    /// Re-resolves `self.caps` against the config's `[[caps]]` overrides
    /// — running `Agent::new(...).with_config(...)` produces the same
    /// caps the user would see after a `/model` swap.
    pub fn with_config(mut self, cfg: Arc<Config>, provider_name: impl Into<String>) -> Self {
        self.caps = caps_for_with_overrides(&self.model, &cfg.caps);
        self.cfg = Some(cfg);
        self.provider_name = provider_name.into();
        self
    }

    /// Resolve capabilities for `model` honoring the bound config's
    /// overrides if any. Used by the `/model` slash command so a swap
    /// picks up `[[caps]]` overrides without re-plumbing config access.
    pub fn resolve_caps(&self, model: &str) -> ModelCaps {
        match &self.cfg {
            Some(cfg) => caps_for_with_overrides(model, &cfg.caps),
            None => caps_for(model),
        }
    }

    /// Re-resolve the ledger inputs that can change in a live REPL.
    /// Call after swapping provider/model or installing a reloaded config.
    pub fn refresh_ledger_accounting(&mut self) {
        let Some(cfg) = self.cfg.clone() else {
            return;
        };
        let profile = crate::bootstrap::run_accounting_profile(
            cfg.as_ref(),
            &self.provider_name,
            &self.model,
            self.max_turns.unwrap_or(0),
        );
        self.ledger.reconfigure(
            self.provider_name.clone(),
            self.model.clone(),
            profile.config_hash,
            profile.accounting,
            profile.price,
        );
    }

    /// Override the default policy. Pairs with `with_approver` for
    /// REPL-vs-script approval ergonomics.
    pub fn with_policy(mut self, policy: Box<dyn Policy>) -> Self {
        self.policy = policy;
        self
    }

    /// Override the default approver. The REPL swaps in `ReadlineApprover`;
    /// scripted `-p` mode keeps `AlwaysApprove`.
    pub fn with_approver(mut self, approver: Box<dyn Approver>) -> Self {
        self.approver = approver;
        self
    }

    /// Replace the default `LinearWithCompact` memory with a custom impl.
    /// Public extension point — tests and (eventually) plugins use it; the
    /// binary itself doesn't, hence the explicit allow.
    #[allow(dead_code)]
    pub fn with_memory(mut self, memory: Box<dyn Memory>) -> Self {
        self.memory = memory;
        self
    }

    /// Pin a system prompt onto memory so it survives compaction. Empty
    /// strings are ignored. Skipped when the memory already has pinned
    /// content — on conversation resume the persisted pin is authoritative; we
    /// don't want to stack a fresh system prompt on top of the loaded one.
    pub async fn pin_system_prompt(mut self, prompt: impl Into<String>) -> Result<Self> {
        let s: String = prompt.into();
        if s.is_empty() {
            return Ok(self);
        }
        if !self.memory.pinned().await.is_empty() {
            return Ok(self);
        }
        self.memory
            .pin(json!({ "role": "system", "content": s }))
            .await?;
        Ok(self)
    }

    /// Reset session state — drops conversation history and the per-tool
    /// context (so `Edit`'s read-first invariant resets too). Pinned
    /// content (system prompt) is preserved and re-injected on the next
    /// turn. Usage counters reset so `/cost` reflects the new turn.
    pub async fn clear(&mut self) -> Result<()> {
        self.memory.clear().await?;
        self.ctx = ToolContext::new();
        self.last_usage = None;
        self.session_usage = UsageTotals::default();
        // The ledger is not reset: it records what the process actually
        // spent, and dropping history doesn't refund it.
        Ok(())
    }

    /// Borrow the per-session tool context. The binary uses this at
    /// startup to wire up a `ReadLogger` (so `Read` calls round-trip
    /// across resumed conversations) and to seed replayed read paths.
    pub fn tool_context(&self) -> &ToolContext {
        &self.ctx
    }

    /// Roll back the most recent user turn — drop the user
    /// message and everything that came after it (assistant
    /// reply, tool round-trips). Returns the body of the popped
    /// user prompt so the caller can show it / re-load it for
    /// editing, or `None` if there was nothing to undo.
    ///
    /// Pinned content (the system prompt) and the rolling
    /// summary are preserved.
    pub async fn undo_last_user_turn(&mut self) -> Option<String> {
        // Only the `recent` part maps onto logical record positions:
        // pinned content and the rolling summary were never `record`ed.
        let parts = self.memory.snapshot_parts().await;
        let (offset, _) = parts
            .recent
            .iter()
            .enumerate()
            .rev()
            .find(|(_, m)| m.get("role").and_then(|v| v.as_str()) == Some("user"))?;
        let body = parts.recent[offset]
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        // `recent` is the tail of the record sequence, so its first
        // element sits at `len() - recent.len()`.
        let base = self.memory.len().checked_sub(parts.recent.len())?;
        // Truncate before the user message — i.e. drop the user
        // message itself and everything after it.
        self.memory.truncate(base + offset).await.ok()?;
        Some(body)
    }

    /// Force a compaction pass regardless of current token usage. Drives
    /// the `/compact` slash command. Returns whatever `maybe_compact`
    /// returns — strategies that decide there's nothing to compact (too
    /// few messages) report success without changing state.
    pub async fn force_compact(&mut self) -> Result<()> {
        let report = self
            .memory
            .maybe_compact(CompactContext {
                provider: self.provider.as_ref(),
                model: &self.model,
                target_tokens: 0,
                current_tokens: usize::MAX,
            })
            .await?;
        if let Some(report) = report {
            let turn = self.ledger.summary().turns.saturating_add(1);
            self.record_compaction(turn, report).await;
        }
        Ok(())
    }

    async fn record_compaction(&mut self, turn: u32, report: CompactionReport) {
        self.session_usage.add(report.usage);
        if let Some(usage) = report.usage {
            self.last_usage = Some(usage);
        }
        self.ledger
            .record_started(
                report.started_at_ms,
                turn,
                report.estimated,
                report.usage,
                report.latency,
            )
            .await;
    }

    /// Run a single tool call through the policy gate, then through the
    /// tool registry. The returned string is what the model sees as the
    /// tool result — including policy denials and user declines, which
    /// are not errors at the agent level.
    async fn dispatch_with_policy(&self, name: &str, args: Value) -> String {
        let decision = self.policy.check(name, &args);
        match decision {
            Decision::Allow => match self.tools.dispatch(name, args, &self.ctx).await {
                Ok(s) => s,
                Err(e) => format!("Error: {}", e),
            },
            Decision::Deny(reason) => format!("policy denied {}: {}", name, reason),
            Decision::Ask(reason) => {
                if self.approver.approve(name, &args, &reason).await {
                    match self.tools.dispatch(name, args, &self.ctx).await {
                        Ok(s) => s,
                        Err(e) => format!("Error: {}", e),
                    }
                } else {
                    format!("user declined {}: {}", name, reason)
                }
            }
        }
    }

    /// Append `prompt` as a user turn, run the loop until the assistant
    /// produces a response without tool calls, and return that final
    /// content. Non-streaming path; uses a no-op sink under the hood.
    pub async fn run(&mut self, prompt: &str) -> Result<RunOutcome> {
        let mut nop = |_: StreamEvent<'_>| {};
        self.run_streaming(prompt, &mut nop).await
    }

    /// Same as `run`, but emits provider stream events through `sink` as
    /// they arrive (assistant text via `StreamEvent::Content`, in-flight
    /// tool-call arguments via `StreamEvent::ToolArgsChunk`). Tool-call
    /// rounds are silent on the content side; only model text reaches
    /// `Content`.
    /// Stop hooks may customize the
    /// human-facing exhaustion message, but cannot turn exhaustion into a
    /// completed outcome.
    pub async fn run_streaming<F>(&mut self, prompt: &str, sink: &mut F) -> Result<RunOutcome>
    where
        F: FnMut(StreamEvent<'_>) + Send,
    {
        self.memory
            .record(json!({ "role": "user", "content": prompt }))
            .await?;

        let first_turn = self.ledger.next_turn();
        let mut invocation_turn = 0usize;
        loop {
            if let Some(cap) = self.max_turns {
                if invocation_turn >= cap {
                    let msg = format!("(max_turns reached: {})", cap);
                    let msg = self.hooks.dispatch_stop(msg).await;
                    return Ok(RunOutcome::MaxTurnsExhausted {
                        limit: cap,
                        message: msg,
                    });
                }
            }
            let turn = first_turn.saturating_add(invocation_turn as u32);
            invocation_turn += 1;

            let build_started = Instant::now();
            // Sync MCP tools that have notified `tools/list_changed`
            // since the last turn. Cost on a quiet turn is one atomic
            // load per server; on a turn where a server pushed an
            // update, we refetch its `tools/list` and swap registry
            // entries so the model can see the deltas on this turn.
            if !self.mcp_handles.is_empty() {
                let deltas = crate::mcp::refresh_changed_tools(self.mcp_handles.as_ref()).await;
                for d in deltas {
                    for name in d.removed {
                        self.tools.remove(&name);
                    }
                    for tool in d.added {
                        self.tools.register_box(tool);
                    }
                }
            }
            let current_tokens = self
                .last_usage
                .and_then(|u| u.prompt_tokens)
                .map(|p| p as usize)
                .unwrap_or(0);
            let compact_started = Instant::now();
            let compaction = self
                .memory
                .maybe_compact(CompactContext {
                    provider: self.provider.as_ref(),
                    model: &self.model,
                    target_tokens: self.caps.compact_target(),
                    current_tokens,
                })
                .await?;
            let compaction_ms = as_ms(compact_started.elapsed());
            if let Some(report) = compaction {
                self.record_compaction(turn, report).await;
            }

            let parts = self.memory.snapshot_parts().await;
            let tool_schemas = self.tools.openai_schemas();
            let estimated = estimate_context(&parts, &tool_schemas);
            let req = ChatRequest {
                model: self.model.clone(),
                messages: parts.flatten(),
                tools: tool_schemas,
            };
            let mut latency = Latency {
                context_build_ms: as_ms(build_started.elapsed()),
                compaction_ms,
                ..Latency::default()
            };

            let request_started_at_ms = now_ms();
            let model_started = Instant::now();
            // Time to first token is only observable from the stream,
            // and only when the turn produces text at all — a pure
            // tool-call round exposes nothing to time.
            let mut ttft = None;
            let resp = {
                let mut timed = |ev: StreamEvent<'_>| {
                    if ttft.is_none() && matches!(ev, StreamEvent::Content(_)) {
                        ttft = Some(as_ms(model_started.elapsed()));
                    }
                    sink(ev);
                };
                let sink_dyn: StreamSink<'_> = &mut timed;
                self.provider.chat_stream(req, sink_dyn).await?
            };
            latency.model_ms = as_ms(model_started.elapsed());
            latency.ttft_ms = ttft;
            self.session_usage.add(resp.usage);
            if let Some(u) = resp.usage {
                self.last_usage = Some(u);
            }

            // Models without native tool-call support sometimes emit calls
            // as raw JSON in `content`. Splice the parsed calls into the
            // assistant message *before* recording so the model's next
            // turn sees a coherent (assistant with tool_calls) → (tool
            // result) sequence.
            let mut message = resp.message.clone();
            let mut tool_calls = message
                .get("tool_calls")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            if tool_calls.is_empty() && !self.caps.supports_native_tool_calls {
                if let Some(content) = message.get("content").and_then(|v| v.as_str()) {
                    if let Some(parsed) = tool_parse::parse_text_tool_calls(content) {
                        tool_calls = parsed.clone();
                        message["tool_calls"] = Value::Array(parsed);
                    }
                }
            }

            self.memory.record(message).await?;

            if tool_calls.is_empty() {
                self.ledger
                    .record_started(request_started_at_ms, turn, estimated, resp.usage, latency)
                    .await;
                let content = resp
                    .message
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let content = self.hooks.dispatch_stop(content).await;
                return Ok(RunOutcome::Completed(content));
            }

            let tools_started = Instant::now();
            for call in &tool_calls {
                let id = call.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let name = call
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let args_str = call
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("{}");
                let args: Value = serde_json::from_str(args_str).unwrap_or_else(|_| json!({}));

                // Pre-hooks compose: `Replace` mutates the args the
                // policy + tool will see; the first `Skip` short-circuits
                // dispatch with a synthetic result. Post-hooks still
                // fire afterwards so observers and redactors run on
                // whatever the model is about to see.
                let decision = self.hooks.dispatch_pre_tool_use(name, args).await;
                let (final_args, raw_result) = match decision {
                    PreToolDecision::Continue { args } => {
                        let r = self.dispatch_with_policy(name, args.clone()).await;
                        (args, r)
                    }
                    PreToolDecision::Skip { args, result } => (args, result),
                };
                let result = self
                    .hooks
                    .dispatch_post_tool_use(name, &final_args, raw_result)
                    .await;

                self.memory
                    .record(json!({
                        "role": "tool",
                        "tool_call_id": id,
                        "content": result,
                    }))
                    .await?;
            }
            latency.tool_ms = as_ms(tools_started.elapsed());
            self.ledger
                .record_started(request_started_at_ms, turn, estimated, resp.usage, latency)
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::ChatResponse;
    use crate::providers::fake::FakeProvider;
    use crate::tools::Tool;
    use async_trait::async_trait;
    use serde_json::json;

    fn assistant_text(content: &str) -> Value {
        json!({ "role": "assistant", "content": content })
    }

    fn tool_call(id: &str, name: &str, args: Value) -> Value {
        json!({
            "id": id,
            "type": "function",
            "function": {
                "name": name,
                "arguments": args.to_string(),
            }
        })
    }

    fn assistant_with_tool_calls(calls: Vec<Value>) -> Value {
        json!({
            "role": "assistant",
            "content": null,
            "tool_calls": calls
        })
    }

    struct StaticTool {
        name: &'static str,
        out: &'static str,
    }

    #[async_trait]
    impl Tool for StaticTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "static"
        }
        fn parameters(&self) -> Value {
            json!({"type": "object", "properties": {}})
        }
        async fn run(&self, _args: Value, _ctx: &ToolContext) -> Result<String> {
            Ok(self.out.to_string())
        }
    }

    #[tokio::test]
    async fn returns_assistant_content_when_no_tool_calls() {
        let provider = FakeProvider::new(vec![assistant_text("done")]);
        let mut agent = Agent::new(Box::new(provider), Registry::new(), "m".into());
        let out = agent.run("hi").await.unwrap();
        assert_eq!(out, RunOutcome::Completed("done".into()));
    }

    #[tokio::test]
    async fn dispatches_tool_call_and_continues_loop() {
        let provider = FakeProvider::new(vec![
            assistant_with_tool_calls(vec![tool_call("c1", "Echo", json!({}))]),
            assistant_text("after-tool"),
        ]);
        let mut tools = Registry::new();
        tools.register(StaticTool {
            name: "Echo",
            out: "tool-output",
        });

        let mut agent = Agent::new(Box::new(provider), tools, "m".into());
        let out = agent.run("hi").await.unwrap();
        assert_eq!(out, RunOutcome::Completed("after-tool".into()));
    }

    #[tokio::test]
    async fn second_request_includes_tool_result_message() {
        let provider = FakeProvider::new(vec![
            assistant_with_tool_calls(vec![tool_call("c1", "Echo", json!({}))]),
            assistant_text("done"),
        ]);
        let raw = std::sync::Arc::new(provider);
        let provider_ref = raw.clone();

        let mut tools = Registry::new();
        tools.register(StaticTool {
            name: "Echo",
            out: "tool-output",
        });

        let mut agent = Agent::new(
            Box::new(ScriptedProviderHandle(provider_ref.clone())),
            tools,
            "m".into(),
        );
        agent.run("hi").await.unwrap();

        let seen = provider_ref.requests();
        assert_eq!(seen.len(), 2);
        let second_msgs = &seen[1].messages;
        // user, assistant(tool_calls), tool result
        assert_eq!(second_msgs.len(), 3);
        assert_eq!(second_msgs[2]["role"], "tool");
        assert_eq!(second_msgs[2]["tool_call_id"], "c1");
        assert_eq!(second_msgs[2]["content"], "tool-output");
    }

    #[tokio::test]
    async fn unknown_tool_surfaces_error_to_model_without_aborting() {
        let provider = FakeProvider::new(vec![
            assistant_with_tool_calls(vec![tool_call("c1", "DoesNotExist", json!({}))]),
            assistant_text("recovered"),
        ]);
        let raw = std::sync::Arc::new(provider);
        let provider_ref = raw.clone();

        let mut agent = Agent::new(
            Box::new(ScriptedProviderHandle(provider_ref.clone())),
            Registry::new(),
            "m".into(),
        );
        let out = agent.run("hi").await.unwrap();
        assert_eq!(out, RunOutcome::Completed("recovered".into()));

        let seen = provider_ref.requests();
        let tool_msg = &seen[1].messages[2];
        assert_eq!(tool_msg["role"], "tool");
        assert!(
            tool_msg["content"]
                .as_str()
                .unwrap()
                .contains("unknown tool: DoesNotExist")
        );
    }

    #[tokio::test]
    async fn system_prompt_is_pinned_and_appears_first() {
        let provider = FakeProvider::new(vec![assistant_text("ok")]);
        let raw = std::sync::Arc::new(provider);
        let mut agent = Agent::new(
            Box::new(ScriptedProviderHandle(raw.clone())),
            Registry::new(),
            "m".into(),
        )
        .pin_system_prompt("you are a coding agent")
        .await
        .unwrap();

        agent.run("hi").await.unwrap();
        let seen = raw.requests();
        let msgs = &seen[0].messages;
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "you are a coding agent");
        assert_eq!(msgs[1]["role"], "user");
    }

    #[tokio::test]
    async fn empty_system_prompt_is_skipped() {
        let provider = FakeProvider::new(vec![assistant_text("ok")]);
        let raw = std::sync::Arc::new(provider);
        let mut agent = Agent::new(
            Box::new(ScriptedProviderHandle(raw.clone())),
            Registry::new(),
            "m".into(),
        )
        .pin_system_prompt("")
        .await
        .unwrap();

        agent.run("hi").await.unwrap();
        let seen = raw.requests();
        let msgs = &seen[0].messages;
        assert_eq!(msgs[0]["role"], "user");
    }

    #[tokio::test]
    async fn run_streaming_emits_content_via_sink() {
        let provider = FakeProvider::new(vec![assistant_text("hello world")]);
        let mut agent = Agent::new(Box::new(provider), Registry::new(), "m".into());
        let mut chunks: Vec<String> = Vec::new();
        let mut sink = |ev: StreamEvent<'_>| {
            if let StreamEvent::Content(s) = ev {
                chunks.push(s.to_string());
            }
        };
        let out = agent.run_streaming("hi", &mut sink).await.unwrap();
        assert_eq!(out, RunOutcome::Completed("hello world".into()));
        // FakeProvider splits at the midpoint char boundary into two chunks.
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks.concat(), "hello world");
    }

    #[tokio::test]
    async fn second_turn_keeps_history_from_first_turn() {
        let provider = FakeProvider::new(vec![assistant_text("first"), assistant_text("second")]);
        let raw = std::sync::Arc::new(provider);

        let mut agent = Agent::new(
            Box::new(ScriptedProviderHandle(raw.clone())),
            Registry::new(),
            "m".into(),
        )
        .pin_system_prompt("sys")
        .await
        .unwrap();

        agent.run("a").await.unwrap();
        agent.run("b").await.unwrap();

        let seen = raw.requests();
        // Second turn's request must contain: system, user(a), assistant(first), user(b)
        let msgs = &seen[1].messages;
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[1]["content"], "a");
        assert_eq!(msgs[2]["role"], "assistant");
        assert_eq!(msgs[2]["content"], "first");
        assert_eq!(msgs[3]["content"], "b");
    }

    #[tokio::test]
    async fn clear_drops_history_but_keeps_system_prompt() {
        let provider = FakeProvider::new(vec![assistant_text("first"), assistant_text("second")]);
        let raw = std::sync::Arc::new(provider);

        let mut agent = Agent::new(
            Box::new(ScriptedProviderHandle(raw.clone())),
            Registry::new(),
            "m".into(),
        )
        .pin_system_prompt("sys")
        .await
        .unwrap();

        agent.run("a").await.unwrap();
        agent.clear().await.unwrap();
        agent.run("b").await.unwrap();

        let seen = raw.requests();
        // After clear, second turn looks like a fresh session: system + user(b).
        let msgs = &seen[1].messages;
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[1]["content"], "b");
    }

    #[tokio::test]
    async fn last_usage_is_captured_when_provider_supplies_it() {
        struct UsageProvider;
        #[async_trait]
        impl Provider for UsageProvider {
            async fn chat(&self, _: ChatRequest) -> Result<ChatResponse> {
                Ok(ChatResponse {
                    message: assistant_text("done"),
                    usage: Some(Usage {
                        prompt_tokens: Some(12),
                        completion_tokens: Some(5),
                        total_tokens: Some(17),
                        ..Usage::default()
                    }),
                })
            }
        }
        let mut agent = Agent::new(Box::new(UsageProvider), Registry::new(), "m".into());
        agent.run("hi").await.unwrap();
        let u = agent.last_usage.expect("usage should be captured");
        assert_eq!(u.prompt_tokens, Some(12));
        assert_eq!(u.completion_tokens, Some(5));
        assert_eq!(u.total_tokens, Some(17));
    }

    #[tokio::test]
    async fn hooks_fire_around_tool_use_and_on_stop() {
        use crate::hooks::{Hook, HookOutcome, HookPayload, HookRegistry};
        use std::sync::{Arc, Mutex};

        struct TraceHook(Arc<Mutex<Vec<String>>>);
        #[async_trait]
        impl Hook for TraceHook {
            fn name(&self) -> &str {
                "trace"
            }
            async fn handle(&self, payload: &HookPayload<'_>) -> HookOutcome {
                let line = match payload {
                    HookPayload::PreToolUse { tool, .. } => format!("pre:{}", tool),
                    HookPayload::PostToolUse { tool, result, .. } => {
                        format!("post:{}:{}", tool, result)
                    }
                    HookPayload::Stop { final_content } => format!("stop:{}", final_content),
                };
                self.0.lock().unwrap().push(line);
                HookOutcome::Continue
            }
        }

        let provider = FakeProvider::new(vec![
            assistant_with_tool_calls(vec![tool_call("c1", "Echo", json!({}))]),
            assistant_text("done"),
        ]);
        let mut tools = Registry::new();
        tools.register(StaticTool {
            name: "Echo",
            out: "tool-output",
        });

        let trace = Arc::new(Mutex::new(Vec::new()));
        let mut hooks = HookRegistry::new();
        hooks.register(TraceHook(trace.clone()));

        let mut agent = Agent::new(Box::new(provider), tools, "m".into()).with_hooks(hooks);
        let out = agent.run("hi").await.unwrap();
        assert_eq!(out, RunOutcome::Completed("done".into()));

        let log = trace.lock().unwrap().clone();
        assert_eq!(log.len(), 3);
        assert_eq!(log[0], "pre:Echo");
        assert_eq!(log[1], "post:Echo:tool-output");
        assert_eq!(log[2], "stop:done");
    }

    #[tokio::test]
    async fn pre_hook_fires_even_when_policy_denies() {
        use crate::hooks::{Hook, HookOutcome, HookPayload, HookRegistry};
        use std::sync::{Arc, Mutex};

        struct TraceHook(Arc<Mutex<Vec<String>>>);
        #[async_trait]
        impl Hook for TraceHook {
            fn name(&self) -> &str {
                "trace"
            }
            async fn handle(&self, payload: &HookPayload<'_>) -> HookOutcome {
                let line = match payload {
                    HookPayload::PreToolUse { tool, .. } => format!("pre:{}", tool),
                    HookPayload::PostToolUse { result, .. } => format!("post:{}", result),
                    HookPayload::Stop { .. } => "stop".into(),
                };
                self.0.lock().unwrap().push(line);
                HookOutcome::Continue
            }
        }

        struct DenyAll;
        impl Policy for DenyAll {
            fn check(&self, _: &str, _: &Value) -> Decision {
                Decision::Deny("nope".into())
            }
        }

        let provider = FakeProvider::new(vec![
            assistant_with_tool_calls(vec![tool_call("c1", "Echo", json!({}))]),
            assistant_text("recovered"),
        ]);
        let mut tools = Registry::new();
        tools.register(StaticTool {
            name: "Echo",
            out: "tool-output",
        });

        let trace = Arc::new(Mutex::new(Vec::new()));
        let mut hooks = HookRegistry::new();
        hooks.register(TraceHook(trace.clone()));

        let mut agent = Agent::new(Box::new(provider), tools, "m".into())
            .with_policy(Box::new(DenyAll))
            .with_hooks(hooks);
        agent.run("hi").await.unwrap();

        let log = trace.lock().unwrap().clone();
        // pre fires before policy → still see Echo. post sees the
        // policy-denied result string.
        assert_eq!(log[0], "pre:Echo");
        assert!(log[1].starts_with("post:policy denied"));
    }

    #[tokio::test]
    async fn fallback_parser_dispatches_text_mode_tool_calls() {
        // Two scripted responses:
        //   1. assistant content is bare JSON tool call (qwen-style),
        //      no structured `tool_calls` field.
        //   2. plain text final answer.
        let provider = FakeProvider::new(vec![
            json!({
                "role": "assistant",
                "content": r#"{"name":"Echo","arguments":{}}"#,
            }),
            assistant_text("done"),
        ]);

        let mut tools = Registry::new();
        tools.register(StaticTool {
            name: "Echo",
            out: "tool-output",
        });

        // qwen2.5-coder:7b has supports_native_tool_calls = false in
        // the capability registry, which is what gates the parser.
        let mut agent = Agent::new(Box::new(provider), tools, "qwen2.5-coder:7b".into());
        let out = agent.run("hi").await.unwrap();
        assert_eq!(out, RunOutcome::Completed("done".into()));

        // The recorded assistant message should now have synthesized
        // `tool_calls` even though the provider didn't emit them.
        let snap = agent.memory.snapshot().await;
        let assistant = snap
            .iter()
            .find(|m| m["role"] == "assistant" && m.get("tool_calls").is_some())
            .expect("parser should have spliced tool_calls onto the assistant message");
        let calls = assistant["tool_calls"].as_array().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["function"]["name"], "Echo");
    }

    #[tokio::test]
    async fn fallback_parser_skipped_for_models_with_native_tool_support() {
        // Same JSON-as-content payload, but model claims native tool
        // support — the agent should NOT engage the parser, so no tool
        // dispatch happens and the JSON-shaped content becomes the final
        // answer.
        let provider = FakeProvider::new(vec![json!({
            "role": "assistant",
            "content": r#"{"name":"Echo","arguments":{}}"#,
        })]);
        let mut tools = Registry::new();
        tools.register(StaticTool {
            name: "Echo",
            out: "tool-output",
        });
        let mut agent = Agent::new(
            Box::new(provider),
            tools,
            "anthropic/claude-haiku-4.5".into(),
        );
        let out = agent.run("hi").await.unwrap();
        assert_eq!(
            out,
            RunOutcome::Completed(r#"{"name":"Echo","arguments":{}}"#.into())
        );
    }

    #[tokio::test]
    async fn policy_denial_surfaces_as_tool_result_not_error() {
        struct DenyAll;
        impl Policy for DenyAll {
            fn check(&self, tool: &str, _: &Value) -> Decision {
                Decision::Deny(format!("nope: {}", tool))
            }
        }

        let provider = FakeProvider::new(vec![
            assistant_with_tool_calls(vec![tool_call("c1", "Echo", json!({}))]),
            assistant_text("recovered"),
        ]);
        let raw = std::sync::Arc::new(provider);
        let provider_ref = raw.clone();

        let mut tools = Registry::new();
        tools.register(StaticTool {
            name: "Echo",
            out: "tool-output",
        });

        let mut agent = Agent::new(
            Box::new(ScriptedProviderHandle(provider_ref.clone())),
            tools,
            "m".into(),
        )
        .with_policy(Box::new(DenyAll));

        let out = agent.run("hi").await.unwrap();
        assert_eq!(out, RunOutcome::Completed("recovered".into()));

        let seen = provider_ref.requests();
        let tool_msg = &seen[1].messages[2];
        assert_eq!(tool_msg["role"], "tool");
        assert!(
            tool_msg["content"]
                .as_str()
                .unwrap()
                .contains("policy denied")
        );
    }

    #[tokio::test]
    async fn ask_decision_resolved_by_approver_returning_false_is_user_declined() {
        struct AskAll;
        impl Policy for AskAll {
            fn check(&self, _: &str, _: &Value) -> Decision {
                Decision::Ask("you sure?".into())
            }
        }
        struct No;
        #[async_trait]
        impl crate::policy::Approver for No {
            async fn approve(&self, _: &str, _: &Value, _: &str) -> bool {
                false
            }
        }

        let provider = FakeProvider::new(vec![
            assistant_with_tool_calls(vec![tool_call("c1", "Echo", json!({}))]),
            assistant_text("got it"),
        ]);
        let raw = std::sync::Arc::new(provider);
        let provider_ref = raw.clone();

        let mut tools = Registry::new();
        tools.register(StaticTool {
            name: "Echo",
            out: "tool-output",
        });

        let mut agent = Agent::new(
            Box::new(ScriptedProviderHandle(provider_ref.clone())),
            tools,
            "m".into(),
        )
        .with_policy(Box::new(AskAll))
        .with_approver(Box::new(No));

        agent.run("hi").await.unwrap();
        let seen = provider_ref.requests();
        let tool_msg = &seen[1].messages[2];
        assert!(
            tool_msg["content"]
                .as_str()
                .unwrap()
                .contains("user declined")
        );
    }

    #[tokio::test]
    async fn allow_decision_runs_the_tool_unchanged() {
        struct AllowAll;
        impl Policy for AllowAll {
            fn check(&self, _: &str, _: &Value) -> Decision {
                Decision::Allow
            }
        }

        let provider = FakeProvider::new(vec![
            assistant_with_tool_calls(vec![tool_call("c1", "Echo", json!({}))]),
            assistant_text("ok"),
        ]);
        let raw = std::sync::Arc::new(provider);
        let provider_ref = raw.clone();

        let mut tools = Registry::new();
        tools.register(StaticTool {
            name: "Echo",
            out: "tool-output",
        });

        let mut agent = Agent::new(
            Box::new(ScriptedProviderHandle(provider_ref.clone())),
            tools,
            "m".into(),
        )
        .with_policy(Box::new(AllowAll));

        agent.run("hi").await.unwrap();
        let seen = provider_ref.requests();
        assert_eq!(seen[1].messages[2]["content"], "tool-output");
    }

    #[tokio::test]
    async fn with_memory_swaps_in_an_alternative_strategy() {
        // Sanity check that a custom `Memory` impl flows through the
        // agent loop unchanged — the swap-out point that justifies the
        // trait-based design.
        struct CountingMemory {
            inner: LinearWithCompact,
            recorded: usize,
        }
        #[async_trait]
        impl Memory for CountingMemory {
            async fn record(&mut self, m: Value) -> Result<()> {
                self.recorded += 1;
                self.inner.record(m).await
            }
            async fn snapshot(&self) -> Vec<Value> {
                self.inner.snapshot().await
            }
            async fn pin(&mut self, m: Value) -> Result<()> {
                self.inner.pin(m).await
            }
            fn len(&self) -> usize {
                self.inner.len()
            }
            async fn truncate(&mut self, n: usize) -> Result<()> {
                self.inner.truncate(n).await
            }
            async fn clear(&mut self) -> Result<()> {
                self.inner.clear().await
            }
        }

        let provider = FakeProvider::new(vec![assistant_text("ok")]);
        let mut agent = Agent::new(Box::new(provider), Registry::new(), "m".into()).with_memory(
            Box::new(CountingMemory {
                inner: LinearWithCompact::new(),
                recorded: 0,
            }),
        );
        agent.run("hi").await.unwrap();
        // user + assistant.
        assert_eq!(agent.memory.len(), 2);
    }

    #[tokio::test]
    async fn session_usage_accumulates_across_turns() {
        struct FixedUsageProvider;
        #[async_trait]
        impl Provider for FixedUsageProvider {
            async fn chat(&self, _: ChatRequest) -> Result<ChatResponse> {
                Ok(ChatResponse {
                    message: assistant_text("ok"),
                    usage: Some(Usage {
                        prompt_tokens: Some(10),
                        completion_tokens: Some(3),
                        total_tokens: Some(13),
                        ..Usage::default()
                    }),
                })
            }
        }
        let mut agent = Agent::new(Box::new(FixedUsageProvider), Registry::new(), "m".into());
        agent.run("a").await.unwrap();
        agent.run("b").await.unwrap();
        agent.run("c").await.unwrap();
        // last_usage reflects only the most recent round.
        assert_eq!(agent.last_usage.unwrap().prompt_tokens, Some(10));
        // session_usage sums across all three.
        assert_eq!(agent.session_usage.calls, 3);
        assert_eq!(agent.session_usage.prompt_tokens.reported(), Some(30));
        assert_eq!(agent.session_usage.completion_tokens.reported(), Some(9));
        assert_eq!(agent.session_usage.total_tokens.reported(), Some(39));
    }

    #[tokio::test]
    async fn a_call_the_provider_did_not_report_is_counted_never_summed_as_zero() {
        struct SometimesUsageProvider {
            seen: std::sync::atomic::AtomicUsize,
        }
        #[async_trait]
        impl Provider for SometimesUsageProvider {
            async fn chat(&self, _: ChatRequest) -> Result<ChatResponse> {
                let n = self.seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(ChatResponse {
                    message: assistant_text("ok"),
                    usage: (n == 0).then(|| Usage {
                        prompt_tokens: Some(10),
                        completion_tokens: Some(3),
                        total_tokens: Some(13),
                        ..Usage::default()
                    }),
                })
            }
        }
        let mut agent = Agent::new(
            Box::new(SometimesUsageProvider {
                seen: std::sync::atomic::AtomicUsize::new(0),
            }),
            Registry::new(),
            "m".into(),
        );
        agent.run("a").await.unwrap();
        agent.run("b").await.unwrap();

        assert_eq!(agent.session_usage.calls, 2);
        assert_eq!(agent.session_usage.prompt_tokens.reported(), Some(10));
        assert_eq!(agent.session_usage.prompt_tokens.unreported_calls, 1);
        // Nothing was ever reported here, so the total stays unknown
        // rather than collapsing to a confident zero.
        assert_eq!(agent.session_usage.cache_read_tokens.reported(), None);
        assert_eq!(agent.session_usage.cache_read_tokens.unreported_calls, 2);
        // last_usage still holds the most recent *reported* call.
        assert_eq!(agent.last_usage.unwrap().prompt_tokens, Some(10));
    }

    #[tokio::test]
    async fn an_unknown_prompt_count_leaves_compaction_at_zero_tokens() {
        // The compaction trigger has always treated "no usage" as zero
        // tokens used. An unreported prompt count must land in the same
        // place, not somewhere new.
        use std::sync::{Arc, Mutex};
        struct ObservingMemory {
            inner: LinearWithCompact,
            observed: Arc<Mutex<Vec<usize>>>,
        }
        #[async_trait]
        impl Memory for ObservingMemory {
            async fn record(&mut self, m: Value) -> Result<()> {
                self.inner.record(m).await
            }
            async fn snapshot(&self) -> Vec<Value> {
                self.inner.snapshot().await
            }
            async fn pin(&mut self, m: Value) -> Result<()> {
                self.inner.pin(m).await
            }
            fn len(&self) -> usize {
                self.inner.len()
            }
            async fn truncate(&mut self, n: usize) -> Result<()> {
                self.inner.truncate(n).await
            }
            async fn clear(&mut self) -> Result<()> {
                self.inner.clear().await
            }
            async fn maybe_compact(
                &mut self,
                ctx: CompactContext<'_>,
            ) -> Result<Option<memory::CompactionReport>> {
                self.observed.lock().unwrap().push(ctx.current_tokens);
                Ok(None)
            }
        }

        struct CompletionOnlyProvider;
        #[async_trait]
        impl Provider for CompletionOnlyProvider {
            async fn chat(&self, _: ChatRequest) -> Result<ChatResponse> {
                Ok(ChatResponse {
                    message: assistant_text("ok"),
                    usage: Some(Usage {
                        completion_tokens: Some(4),
                        ..Usage::default()
                    }),
                })
            }
        }

        let observed = Arc::new(Mutex::new(Vec::new()));
        let mut agent = Agent::new(
            Box::new(CompletionOnlyProvider),
            Registry::new(),
            "m".into(),
        )
        .with_memory(Box::new(ObservingMemory {
            inner: LinearWithCompact::new(),
            observed: observed.clone(),
        }));
        agent.run("a").await.unwrap();
        agent.run("b").await.unwrap();

        assert_eq!(*observed.lock().unwrap(), vec![0, 0]);
    }

    #[tokio::test]
    async fn clear_resets_session_usage() {
        struct FixedUsageProvider;
        #[async_trait]
        impl Provider for FixedUsageProvider {
            async fn chat(&self, _: ChatRequest) -> Result<ChatResponse> {
                Ok(ChatResponse {
                    message: assistant_text("ok"),
                    usage: Some(Usage {
                        prompt_tokens: Some(5),
                        completion_tokens: Some(2),
                        total_tokens: Some(7),
                        ..Usage::default()
                    }),
                })
            }
        }
        let mut agent = Agent::new(Box::new(FixedUsageProvider), Registry::new(), "m".into());
        agent.run("hi").await.unwrap();
        assert_eq!(agent.session_usage.total_tokens.reported(), Some(7));
        agent.clear().await.unwrap();
        assert_eq!(agent.session_usage, UsageTotals::default());
        assert_eq!(agent.last_usage, None);
    }

    #[tokio::test]
    async fn undo_last_user_turn_drops_user_and_assistant_pair() {
        let provider = FakeProvider::new(vec![assistant_text("done")]);
        let mut agent = Agent::new(Box::new(provider), Registry::new(), "m".into())
            .pin_system_prompt("sys")
            .await
            .unwrap();
        agent.run("hello").await.unwrap();
        // Snapshot before undo: [system, user, assistant].
        let pre = agent.memory.snapshot().await;
        assert_eq!(pre.len(), 3);

        let body = agent.undo_last_user_turn().await;
        assert_eq!(body.as_deref(), Some("hello"));
        let post = agent.memory.snapshot().await;
        // Only the pinned system prompt remains; user + assistant gone.
        assert_eq!(post.len(), 1);
        assert_eq!(post[0]["role"], "system");
    }

    #[tokio::test]
    async fn undo_last_user_turn_returns_none_when_no_user_message() {
        let provider = FakeProvider::new(vec![assistant_text("done")]);
        let mut agent = Agent::new(Box::new(provider), Registry::new(), "m".into())
            .pin_system_prompt("sys")
            .await
            .unwrap();
        // No `run` yet — pinned-only memory.
        assert!(agent.undo_last_user_turn().await.is_none());
    }

    #[tokio::test]
    async fn undo_last_user_turn_walks_back_only_one_pair() {
        // Two user→assistant round trips. Undo should leave the
        // earlier one intact.
        let provider = FakeProvider::new(vec![assistant_text("a1"), assistant_text("a2")]);
        let mut agent = Agent::new(Box::new(provider), Registry::new(), "m".into())
            .pin_system_prompt("sys")
            .await
            .unwrap();
        agent.run("first").await.unwrap();
        agent.run("second").await.unwrap();
        let pre = agent.memory.snapshot().await;
        assert_eq!(pre.len(), 5); // sys + u1 + a1 + u2 + a2

        let popped = agent.undo_last_user_turn().await;
        assert_eq!(popped.as_deref(), Some("second"));
        let post = agent.memory.snapshot().await;
        assert_eq!(post.len(), 3); // sys + u1 + a1
        assert_eq!(post[1]["content"], "first");
        assert_eq!(post[2]["content"], "a1");
    }

    #[tokio::test]
    async fn undo_last_user_turn_keeps_a_live_system_record_it_used_to_swallow() {
        // The old role-sniffing approximation treated any system message
        // sitting first after the pinned prefix as a rolling summary and
        // shifted the truncate target one record too far back.
        let provider = FakeProvider::new(vec![assistant_text("done")]);
        let mut agent = Agent::new(Box::new(provider), Registry::new(), "m".into())
            .pin_system_prompt("sys")
            .await
            .unwrap();
        agent
            .memory
            .record(json!({"role":"system","content":"a live system note"}))
            .await
            .unwrap();
        agent.run("hello").await.unwrap();

        assert_eq!(agent.undo_last_user_turn().await.as_deref(), Some("hello"));
        let post = agent.memory.snapshot().await;
        assert_eq!(post.len(), 2);
        assert_eq!(post[1]["content"], "a live system note");
    }

    #[tokio::test]
    async fn undo_last_user_turn_after_compaction_targets_the_live_window() {
        struct StubSummary;
        #[async_trait]
        impl Provider for StubSummary {
            async fn chat(&self, _: ChatRequest) -> Result<ChatResponse> {
                Ok(ChatResponse {
                    message: assistant_text("S"),
                    usage: None,
                })
            }
        }

        let mut mem = LinearWithCompact::new();
        mem.pin(json!({"role":"system","content":"sys"}))
            .await
            .unwrap();
        for i in 0..6 {
            let role = if i % 2 == 0 { "user" } else { "assistant" };
            mem.record(json!({"role": role, "content": format!("msg{}", i)}))
                .await
                .unwrap();
        }
        let stub = StubSummary;
        mem.maybe_compact(CompactContext {
            provider: &stub,
            model: "m",
            target_tokens: 10,
            current_tokens: 1_000,
        })
        .await
        .unwrap();

        let provider = FakeProvider::new(vec![]);
        let mut agent =
            Agent::new(Box::new(provider), Registry::new(), "m".into()).with_memory(Box::new(mem));

        // Live window is msg4 (user) + msg5 (assistant); undo drops both
        // and leaves the summary intact.
        assert_eq!(agent.undo_last_user_turn().await.as_deref(), Some("msg4"));
        let parts = agent.memory.snapshot_parts().await;
        assert_eq!(parts.summary.len(), 1);
        assert!(parts.recent.is_empty());
    }

    #[tokio::test]
    async fn last_usage_stays_none_when_provider_omits_it() {
        let provider = FakeProvider::new(vec![assistant_text("ok")]);
        let mut agent = Agent::new(Box::new(provider), Registry::new(), "m".into());
        agent.run("hi").await.unwrap();
        assert_eq!(agent.last_usage, None);
    }

    #[tokio::test]
    async fn pre_hook_skip_short_circuits_dispatch_and_post_hook_still_runs() {
        use crate::hooks::{Hook, HookOutcome, HookPayload, HookRegistry};
        use std::sync::{Arc, Mutex};

        struct Skipper;
        #[async_trait]
        impl Hook for Skipper {
            fn name(&self) -> &str {
                "skipper"
            }
            async fn handle(&self, p: &HookPayload<'_>) -> HookOutcome {
                if matches!(p, HookPayload::PreToolUse { .. }) {
                    HookOutcome::Skip("blocked by hook".into())
                } else {
                    HookOutcome::Continue
                }
            }
        }
        struct PostObserver(Arc<Mutex<Vec<String>>>);
        #[async_trait]
        impl Hook for PostObserver {
            fn name(&self) -> &str {
                "post"
            }
            async fn handle(&self, p: &HookPayload<'_>) -> HookOutcome {
                if let HookPayload::PostToolUse { result, .. } = p {
                    self.0.lock().unwrap().push((*result).into());
                }
                HookOutcome::Continue
            }
        }

        struct ExplodingTool;
        #[async_trait]
        impl Tool for ExplodingTool {
            fn name(&self) -> &str {
                "Explode"
            }
            fn description(&self) -> &str {
                "must not run"
            }
            fn parameters(&self) -> Value {
                json!({"type":"object","properties":{}})
            }
            async fn run(&self, _: Value, _: &ToolContext) -> Result<String> {
                panic!("tool dispatched despite Skip outcome");
            }
        }

        let provider = FakeProvider::new(vec![
            assistant_with_tool_calls(vec![tool_call("c1", "Explode", json!({}))]),
            assistant_text("recovered"),
        ]);
        let raw = std::sync::Arc::new(provider);
        let provider_ref = raw.clone();

        let mut tools = Registry::new();
        tools.register(ExplodingTool);

        let post_log = Arc::new(Mutex::new(Vec::new()));
        let mut hooks = HookRegistry::new();
        hooks.register(Skipper);
        hooks.register(PostObserver(post_log.clone()));

        let mut agent = Agent::new(
            Box::new(ScriptedProviderHandle(provider_ref.clone())),
            tools,
            "m".into(),
        )
        .with_hooks(hooks);
        let out = agent.run("hi").await.unwrap();
        assert_eq!(out, RunOutcome::Completed("recovered".into()));

        // Tool message the model saw on its second turn carries the
        // synthetic result, not a real dispatch.
        let seen = provider_ref.requests();
        assert_eq!(seen[1].messages[2]["content"], "blocked by hook");

        // Post-hook still observed the synthetic result.
        let post = post_log.lock().unwrap().clone();
        assert_eq!(post, vec!["blocked by hook"]);
    }

    #[tokio::test]
    async fn pre_hook_replace_mutates_args_seen_by_tool() {
        use crate::hooks::{Hook, HookOutcome, HookPayload, HookRegistry};
        use std::sync::{Arc, Mutex};

        struct Injector;
        #[async_trait]
        impl Hook for Injector {
            fn name(&self) -> &str {
                "injector"
            }
            async fn handle(&self, p: &HookPayload<'_>) -> HookOutcome {
                if let HookPayload::PreToolUse { args, .. } = p {
                    let mut new_args: Value = (*args).clone();
                    if let Some(o) = new_args.as_object_mut() {
                        o.insert("x".into(), json!(99));
                    }
                    HookOutcome::Replace(new_args)
                } else {
                    HookOutcome::Continue
                }
            }
        }

        struct RecordingTool(Arc<Mutex<Option<Value>>>);
        #[async_trait]
        impl Tool for RecordingTool {
            fn name(&self) -> &str {
                "Record"
            }
            fn description(&self) -> &str {
                "records its args"
            }
            fn parameters(&self) -> Value {
                json!({"type":"object","properties":{}})
            }
            async fn run(&self, args: Value, _: &ToolContext) -> Result<String> {
                *self.0.lock().unwrap() = Some(args);
                Ok("ok".into())
            }
        }

        let seen_args = Arc::new(Mutex::new(None));
        let provider = FakeProvider::new(vec![
            assistant_with_tool_calls(vec![tool_call("c1", "Record", json!({"orig": 1}))]),
            assistant_text("done"),
        ]);
        let mut tools = Registry::new();
        tools.register(RecordingTool(seen_args.clone()));

        let mut hooks = HookRegistry::new();
        hooks.register(Injector);

        let mut agent = Agent::new(Box::new(provider), tools, "m".into()).with_hooks(hooks);
        agent.run("hi").await.unwrap();

        let saw = seen_args.lock().unwrap().clone().expect("tool ran");
        assert_eq!(saw["orig"], json!(1));
        assert_eq!(saw["x"], json!(99));
    }

    #[tokio::test]
    async fn post_hook_replace_redacts_result_seen_by_model() {
        use crate::hooks::{Hook, HookOutcome, HookPayload, HookRegistry};

        struct Redactor;
        #[async_trait]
        impl Hook for Redactor {
            fn name(&self) -> &str {
                "redactor"
            }
            async fn handle(&self, p: &HookPayload<'_>) -> HookOutcome {
                if matches!(p, HookPayload::PostToolUse { .. }) {
                    HookOutcome::Replace(Value::String("[redacted]".into()))
                } else {
                    HookOutcome::Continue
                }
            }
        }

        let provider = FakeProvider::new(vec![
            assistant_with_tool_calls(vec![tool_call("c1", "Echo", json!({}))]),
            assistant_text("done"),
        ]);
        let raw = std::sync::Arc::new(provider);
        let provider_ref = raw.clone();

        let mut tools = Registry::new();
        tools.register(StaticTool {
            name: "Echo",
            out: "secret-token-abc",
        });

        let mut hooks = HookRegistry::new();
        hooks.register(Redactor);

        let mut agent = Agent::new(
            Box::new(ScriptedProviderHandle(provider_ref.clone())),
            tools,
            "m".into(),
        )
        .with_hooks(hooks);
        agent.run("hi").await.unwrap();

        let seen = provider_ref.requests();
        assert_eq!(seen[1].messages[2]["content"], "[redacted]");
    }

    #[tokio::test]
    async fn stop_hook_replace_substitutes_final_content_returned_to_caller() {
        use crate::hooks::{Hook, HookOutcome, HookPayload, HookRegistry};

        struct Auditor;
        #[async_trait]
        impl Hook for Auditor {
            fn name(&self) -> &str {
                "auditor"
            }
            async fn handle(&self, p: &HookPayload<'_>) -> HookOutcome {
                if let HookPayload::Stop { final_content } = p {
                    HookOutcome::Replace(Value::String(format!("[audited] {}", final_content)))
                } else {
                    HookOutcome::Continue
                }
            }
        }

        let provider = FakeProvider::new(vec![assistant_text("done")]);
        let mut hooks = HookRegistry::new();
        hooks.register(Auditor);
        let mut agent =
            Agent::new(Box::new(provider), Registry::new(), "m".into()).with_hooks(hooks);
        let out = agent.run("hi").await.unwrap();
        assert_eq!(out, RunOutcome::Completed("[audited] done".into()));
    }

    #[tokio::test]
    async fn stop_hook_can_reword_exhaustion_without_hiding_typed_outcome() {
        use crate::hooks::{Hook, HookOutcome, HookPayload, HookRegistry};

        struct FriendlyStop;
        #[async_trait]
        impl Hook for FriendlyStop {
            fn name(&self) -> &str {
                "friendly-stop"
            }
            async fn handle(&self, p: &HookPayload<'_>) -> HookOutcome {
                if matches!(p, HookPayload::Stop { .. }) {
                    HookOutcome::Replace(Value::String("turn budget used; resume anytime".into()))
                } else {
                    HookOutcome::Continue
                }
            }
        }

        let provider = FakeProvider::new(vec![]);
        let mut hooks = HookRegistry::new();
        hooks.register(FriendlyStop);
        let mut agent = Agent::new(Box::new(provider), Registry::new(), "m".into())
            .with_hooks(hooks)
            .with_max_turns(0);

        let outcome = agent.run("hi").await.unwrap();

        assert_eq!(
            outcome,
            RunOutcome::MaxTurnsExhausted {
                limit: 0,
                message: "turn budget used; resume anytime".into(),
            }
        );
    }

    /// Newtype around `Arc<FakeProvider>` so we can both feed the agent and
    /// inspect captured requests after the run completes.
    struct ScriptedProviderHandle(std::sync::Arc<FakeProvider>);

    #[async_trait]
    impl Provider for ScriptedProviderHandle {
        async fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
            self.0.chat(req).await
        }

        async fn chat_stream(
            &self,
            req: ChatRequest,
            sink: StreamSink<'_>,
        ) -> Result<ChatResponse> {
            self.0.chat_stream(req, sink).await
        }
    }
}
