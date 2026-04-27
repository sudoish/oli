use serde_json::{Value, json};

use crate::error::Result;
use crate::providers::{ChatRequest, ContentSink, Provider};
use crate::tools::{Registry, ToolContext};

pub mod context;

pub struct Agent {
    pub provider: Box<dyn Provider>,
    pub tools: Registry,
    pub model: String,
    pub system_prompt: Option<String>,
    /// Conversation history accumulated across turns. Cleared by `clear()`.
    pub messages: Vec<Value>,
    ctx: ToolContext,
}

impl Agent {
    pub fn new(provider: Box<dyn Provider>, tools: Registry, model: String) -> Self {
        Self {
            provider,
            tools,
            model,
            system_prompt: None,
            messages: Vec::new(),
            ctx: ToolContext::new(),
        }
    }

    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        let s = prompt.into();
        self.system_prompt = if s.is_empty() { None } else { Some(s) };
        self
    }

    /// Reset session state — drops conversation history and the per-tool
    /// context (so `Edit`'s read-first invariant resets too). System prompt
    /// is preserved and re-injected on the next turn.
    pub fn clear(&mut self) {
        self.messages.clear();
        self.ctx = ToolContext::new();
    }

    /// Append `prompt` as a user turn, run the loop until the assistant
    /// produces a response without tool calls, and return that final content.
    /// Non-streaming path; uses a no-op sink under the hood.
    pub async fn run(&mut self, prompt: &str) -> Result<String> {
        let mut nop = |_: &str| {};
        self.run_streaming(prompt, &mut nop).await
    }

    /// Same as `run`, but emits assistant content tokens through `sink` as
    /// the provider streams them. Tool-call rounds are silent — only model
    /// content is forwarded.
    pub async fn run_streaming<F>(&mut self, prompt: &str, sink: &mut F) -> Result<String>
    where
        F: FnMut(&str) + Send,
    {
        if self.messages.is_empty() {
            if let Some(sys) = &self.system_prompt {
                self.messages
                    .push(json!({ "role": "system", "content": sys }));
            }
        }
        self.messages
            .push(json!({ "role": "user", "content": prompt }));

        loop {
            let req = ChatRequest {
                model: self.model.clone(),
                messages: self.messages.clone(),
                tools: self.tools.openai_schemas(),
            };

            let sink_dyn: ContentSink<'_> = sink;
            let resp = self.provider.chat_stream(req, sink_dyn).await?;
            self.messages.push(resp.message.clone());

            let tool_calls = resp
                .message
                .get("tool_calls")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

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

                self.messages.push(json!({
                    "role": "tool",
                    "tool_call_id": id,
                    "content": result,
                }));
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
    async fn system_prompt_is_prepended_when_set() {
        let provider = FakeProvider::new(vec![assistant_text("ok")]);
        let raw = std::sync::Arc::new(provider);
        let mut agent = Agent::new(
            Box::new(ScriptedProviderHandle(raw.clone())),
            Registry::new(),
            "m".into(),
        )
        .with_system_prompt("you are a coding agent");

        agent.run("hi").await.unwrap();
        let seen = raw.requests();
        let msgs = &seen[0].messages;
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "you are a coding agent");
        assert_eq!(msgs[1]["role"], "user");
    }

    #[tokio::test]
    async fn no_system_message_when_unset() {
        let provider = FakeProvider::new(vec![assistant_text("ok")]);
        let raw = std::sync::Arc::new(provider);
        let mut agent = Agent::new(
            Box::new(ScriptedProviderHandle(raw.clone())),
            Registry::new(),
            "m".into(),
        );

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
        .with_system_prompt("sys");

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
        .with_system_prompt("sys");

        agent.run("a").await.unwrap();
        agent.clear();
        agent.run("b").await.unwrap();

        let seen = raw.requests();
        // After clear, second turn looks like a fresh session: system + user(b).
        let msgs = &seen[1].messages;
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[1]["content"], "b");
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
