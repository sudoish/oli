use serde_json::{Value, json};

use crate::error::Result;
use crate::providers::{ChatRequest, ContentSink, Provider, Usage};
use crate::tools::{Registry, ToolContext};

pub mod caps;
pub mod context;
pub mod memory;
pub mod tool_parse;

pub use caps::{ModelCaps, caps_for};
pub use memory::{CompactContext, LinearWithCompact, Memory};

pub struct Agent {
    pub provider: Box<dyn Provider>,
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
    /// Drives `Memory::maybe_compact` decisions and is the data source for
    /// the eventual `/cost` slash command.
    pub last_usage: Option<Usage>,
    ctx: ToolContext,
}

impl Agent {
    pub fn new(provider: Box<dyn Provider>, tools: Registry, model: String) -> Self {
        let caps = caps_for(&model);
        Self {
            provider,
            tools,
            model,
            caps,
            memory: Box::new(LinearWithCompact::new()),
            last_usage: None,
            ctx: ToolContext::new(),
        }
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
    /// strings are ignored. Async because `Memory::pin` is async on the
    /// trait — strategies that go to disk can do their I/O here.
    pub async fn pin_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        let s: String = prompt.into();
        if !s.is_empty() {
            self.memory
                .pin(json!({ "role": "system", "content": s }))
                .await;
        }
        self
    }

    /// Reset session state — drops conversation history and the per-tool
    /// context (so `Edit`'s read-first invariant resets too). Pinned
    /// content (system prompt) is preserved and re-injected on the next
    /// turn.
    pub async fn clear(&mut self) {
        self.memory.clear().await;
        self.ctx = ToolContext::new();
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

        loop {
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

                let result = match self.tools.dispatch(name, args, &self.ctx).await {
                    Ok(s) => s,
                    Err(e) => format!("Error: {}", e),
                };

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
    async fn last_usage_stays_none_when_provider_omits_it() {
        let provider = FakeProvider::new(vec![assistant_text("ok")]);
        let mut agent = Agent::new(Box::new(provider), Registry::new(), "m".into());
        agent.run("hi").await.unwrap();
        assert_eq!(agent.last_usage, None);
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
