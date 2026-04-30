use std::sync::Arc;

use serde_json::{Value, json};

use crate::config::Config;
use crate::error::Result;
use crate::hooks::{HookRegistry, PreToolDecision};
use crate::policy::{AlwaysApprove, Approver, ConfigPolicy, Decision, Policy};
use crate::providers::{ChatRequest, ContentSink, Provider, Usage};
use crate::tools::{Registry, ToolContext};

pub mod caps;
pub mod context;
pub mod memory;
pub mod tool_parse;

pub use caps::{ModelCaps, caps_for, caps_for_with_overrides};
pub use memory::{CompactContext, LinearWithCompact, Memory};

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
    /// so the user can spot a session that's drifting expensive.
    /// Providers that don't report usage simply don't contribute.
    pub session_usage: Usage,
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
            session_usage: Usage::default(),
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
    /// content — on `--resume` the persisted pin is authoritative; we
    /// don't want to stack a fresh system prompt on top of the loaded one.
    pub async fn pin_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        let s: String = prompt.into();
        if s.is_empty() {
            return self;
        }
        if !self.memory.pinned().await.is_empty() {
            return self;
        }
        self.memory
            .pin(json!({ "role": "system", "content": s }))
            .await;
        self
    }

    /// Reset session state — drops conversation history and the per-tool
    /// context (so `Edit`'s read-first invariant resets too). Pinned
    /// content (system prompt) is preserved and re-injected on the next
    /// turn. Usage counters reset so `/cost` reflects the new turn.
    pub async fn clear(&mut self) {
        self.memory.clear().await;
        self.ctx = ToolContext::new();
        self.last_usage = None;
        self.session_usage = Usage::default();
    }

    /// Borrow the per-session tool context. The binary uses this at
    /// startup to wire up a `ReadLogger` (so `Read` calls round-trip
    /// across `--resume`) and to seed replayed read paths.
    pub fn tool_context(&self) -> &ToolContext {
        &self.ctx
    }

    /// Force a compaction pass regardless of current token usage. Drives
    /// the `/compact` slash command. Returns whatever `maybe_compact`
    /// returns — strategies that decide there's nothing to compact (too
    /// few messages) report success without changing state.
    pub async fn force_compact(&mut self) -> Result<()> {
        self.memory
            .maybe_compact(CompactContext {
                provider: self.provider.as_ref(),
                model: &self.model,
                target_tokens: 0,
                current_tokens: usize::MAX,
            })
            .await
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
    pub async fn run(&mut self, prompt: &str) -> Result<String> {
        let mut nop = |_: &str| {};
        self.run_streaming(prompt, &mut nop).await
    }

    /// Same as `run`, but emits assistant content tokens through `sink`
    /// as the provider streams them. Tool-call rounds are silent — only
    /// model content is forwarded.
    pub async fn run_streaming<F>(&mut self, prompt: &str, sink: &mut F) -> Result<String>
    where
        F: FnMut(&str) + Send,
    {
        self.memory
            .record(json!({ "role": "user", "content": prompt }))
            .await;

        let mut turn = 0usize;
        loop {
            if let Some(cap) = self.max_turns {
                if turn >= cap {
                    let msg = format!("(max_turns reached: {})", cap);
                    let msg = self.hooks.dispatch_stop(msg).await;
                    return Ok(msg);
                }
            }
            turn += 1;

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
                .map(|u| u.prompt_tokens as usize)
                .unwrap_or(0);
            self.memory
                .maybe_compact(CompactContext {
                    provider: self.provider.as_ref(),
                    model: &self.model,
                    target_tokens: self.caps.compact_target(),
                    current_tokens,
                })
                .await?;

            let req = ChatRequest {
                model: self.model.clone(),
                messages: self.memory.snapshot().await,
                tools: self.tools.openai_schemas(),
            };

            let sink_dyn: ContentSink<'_> = sink;
            let resp = self.provider.chat_stream(req, sink_dyn).await?;
            if let Some(u) = resp.usage {
                self.last_usage = Some(u);
                self.session_usage.prompt_tokens =
                    self.session_usage.prompt_tokens.saturating_add(u.prompt_tokens);
                self.session_usage.completion_tokens = self
                    .session_usage
                    .completion_tokens
                    .saturating_add(u.completion_tokens);
                self.session_usage.total_tokens =
                    self.session_usage.total_tokens.saturating_add(u.total_tokens);
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

            self.memory.record(message).await;

            if tool_calls.is_empty() {
                let content = resp
                    .message
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let content = self.hooks.dispatch_stop(content).await;
                return Ok(content);
            }

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
                    .await;
            }
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
        assert_eq!(out, "done");
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
        assert_eq!(out, "after-tool");
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
        assert_eq!(out, "recovered");

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
        .await;

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
        .await;

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
        let mut sink = |s: &str| chunks.push(s.to_string());
        let out = agent.run_streaming("hi", &mut sink).await.unwrap();
        assert_eq!(out, "hello world");
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
        .await;

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
        .await;

        agent.run("a").await.unwrap();
        agent.clear().await;
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
                        prompt_tokens: 12,
                        completion_tokens: 5,
                        total_tokens: 17,
                    }),
                })
            }
        }
        let mut agent = Agent::new(Box::new(UsageProvider), Registry::new(), "m".into());
        agent.run("hi").await.unwrap();
        let u = agent.last_usage.expect("usage should be captured");
        assert_eq!(u.prompt_tokens, 12);
        assert_eq!(u.completion_tokens, 5);
        assert_eq!(u.total_tokens, 17);
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
        assert_eq!(out, "done");

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
        assert_eq!(out, "done");

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
        assert_eq!(out, r#"{"name":"Echo","arguments":{}}"#);
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
        assert_eq!(out, "recovered");

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
            async fn record(&mut self, m: Value) {
                self.recorded += 1;
                self.inner.record(m).await;
            }
            async fn snapshot(&self) -> Vec<Value> {
                self.inner.snapshot().await
            }
            async fn pin(&mut self, m: Value) {
                self.inner.pin(m).await;
            }
            fn len(&self) -> usize {
                self.inner.len()
            }
            async fn truncate(&mut self, n: usize) {
                self.inner.truncate(n).await;
            }
            async fn clear(&mut self) {
                self.inner.clear().await;
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
                        prompt_tokens: 10,
                        completion_tokens: 3,
                        total_tokens: 13,
                    }),
                })
            }
        }
        let mut agent = Agent::new(Box::new(FixedUsageProvider), Registry::new(), "m".into());
        agent.run("a").await.unwrap();
        agent.run("b").await.unwrap();
        agent.run("c").await.unwrap();
        // last_usage reflects only the most recent round.
        assert_eq!(agent.last_usage.unwrap().prompt_tokens, 10);
        // session_usage sums across all three.
        assert_eq!(agent.session_usage.prompt_tokens, 30);
        assert_eq!(agent.session_usage.completion_tokens, 9);
        assert_eq!(agent.session_usage.total_tokens, 39);
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
                        prompt_tokens: 5,
                        completion_tokens: 2,
                        total_tokens: 7,
                    }),
                })
            }
        }
        let mut agent = Agent::new(Box::new(FixedUsageProvider), Registry::new(), "m".into());
        agent.run("hi").await.unwrap();
        assert_eq!(agent.session_usage.total_tokens, 7);
        agent.clear().await;
        assert_eq!(agent.session_usage.total_tokens, 0);
        assert_eq!(agent.last_usage, None);
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
        assert_eq!(out, "recovered");

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

        let mut agent =
            Agent::new(Box::new(provider), tools, "m".into()).with_hooks(hooks);
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
                    HookOutcome::Replace(Value::String(format!(
                        "[audited] {}",
                        final_content
                    )))
                } else {
                    HookOutcome::Continue
                }
            }
        }

        let provider = FakeProvider::new(vec![assistant_text("done")]);
        let mut hooks = HookRegistry::new();
        hooks.register(Auditor);
        let mut agent = Agent::new(Box::new(provider), Registry::new(), "m".into())
            .with_hooks(hooks);
        let out = agent.run("hi").await.unwrap();
        assert_eq!(out, "[audited] done");
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
            sink: ContentSink<'_>,
        ) -> Result<ChatResponse> {
            self.0.chat_stream(req, sink).await
        }
    }
}
