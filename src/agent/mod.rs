use serde_json::{Value, json};

use crate::error::Result;
use crate::providers::{ChatRequest, Provider};
use crate::tools::Registry;

pub struct Agent {
    pub provider: Box<dyn Provider>,
    pub tools: Registry,
    pub model: String,
}

impl Agent {
    pub fn new(provider: Box<dyn Provider>, tools: Registry, model: String) -> Self {
        Self {
            provider,
            tools,
            model,
        }
    }

    /// Run the agent loop with a single user prompt. Returns the assistant's
    /// final text content once it stops requesting tool calls.
    pub async fn run(&self, prompt: &str) -> Result<String> {
        let mut messages: Vec<Value> = vec![json!({
            "role": "user",
            "content": prompt,
        })];

        loop {
            let req = ChatRequest {
                model: self.model.clone(),
                messages: messages.clone(),
                tools: self.tools.openai_schemas(),
            };

            let resp = self.provider.chat(req).await?;
            messages.push(resp.message.clone());

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

                let result = match self.tools.dispatch(name, args).await {
                    Ok(s) => s,
                    Err(e) => format!("Error: {}", e),
                };

                messages.push(json!({
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
        async fn run(&self, _args: Value) -> Result<String> {
            Ok(self.out.to_string())
        }
    }

    #[tokio::test]
    async fn returns_assistant_content_when_no_tool_calls() {
        let provider = FakeProvider::new(vec![assistant_text("done")]);
        let agent = Agent::new(Box::new(provider), Registry::new(), "m".into());
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

        let agent = Agent::new(Box::new(provider), tools, "m".into());
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

        let agent = Agent {
            provider: Box::new(ScriptedProviderHandle(provider_ref.clone())),
            tools,
            model: "m".into(),
        };
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

        let agent = Agent {
            provider: Box::new(ScriptedProviderHandle(provider_ref.clone())),
            tools: Registry::new(),
            model: "m".into(),
        };
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

    /// Newtype around `Arc<FakeProvider>` so we can both feed the agent and
    /// inspect captured requests after the run completes.
    struct ScriptedProviderHandle(std::sync::Arc<FakeProvider>);

    #[async_trait]
    impl Provider for ScriptedProviderHandle {
        async fn chat(&self, req: ChatRequest) -> Result<crate::providers::ChatResponse> {
            self.0.chat(req).await
        }
    }
}
