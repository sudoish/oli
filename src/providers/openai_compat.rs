use async_openai::{Client, config::OpenAIConfig};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};

use crate::error::{AgentError, Result};
use crate::providers::{ChatRequest, ChatResponse, ContentSink, Provider};

/// OpenAI-compatible provider. Works against OpenAI, OpenRouter, Ollama
/// (via `http://localhost:11434/v1`), LM Studio, vLLM, llama.cpp's server,
/// and anything else that speaks the OpenAI Chat Completions API.
pub struct OpenAICompatProvider {
    client: Client<OpenAIConfig>,
}

impl OpenAICompatProvider {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        let config = OpenAIConfig::new()
            .with_api_base(base_url.into())
            .with_api_key(api_key.into());
        Self {
            client: Client::with_config(config),
        }
    }
}

#[async_trait]
impl Provider for OpenAICompatProvider {
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
        let payload = json!({
            "model": req.model,
            "messages": req.messages,
            "tools": req.tools,
        });

        let response: Value = self
            .client
            .chat()
            .create_byot(&payload)
            .await
            .map_err(|e| AgentError::Provider(e.to_string()))?;

        let message = response
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .cloned()
            .ok_or_else(|| {
                AgentError::Provider(format!("response missing choices[0].message: {}", response))
            })?;

        Ok(ChatResponse { message })
    }

    async fn chat_stream(&self, req: ChatRequest, sink: ContentSink<'_>) -> Result<ChatResponse> {
        let payload = json!({
            "model": req.model,
            "messages": req.messages,
            "tools": req.tools,
            "stream": true,
        });

        let mut stream = self
            .client
            .chat()
            .create_stream_byot::<_, Value>(&payload)
            .await
            .map_err(|e| AgentError::Provider(e.to_string()))?;

        let mut role: Option<String> = None;
        let mut content = String::new();
        let mut calls: Vec<ToolCallAcc> = Vec::new();

        while let Some(chunk) = stream.next().await {
            let chunk: Value = chunk.map_err(|e| AgentError::Provider(e.to_string()))?;
            let Some(delta) = chunk
                .get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("delta"))
            else {
                continue;
            };

            if role.is_none() {
                if let Some(r) = delta.get("role").and_then(|v| v.as_str()) {
                    role = Some(r.to_string());
                }
            }
            if let Some(c) = delta.get("content").and_then(|v| v.as_str()) {
                if !c.is_empty() {
                    content.push_str(c);
                    sink(c);
                }
            }
            if let Some(tcs) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                for tc in tcs {
                    let idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    while calls.len() <= idx {
                        calls.push(ToolCallAcc::default());
                    }
                    let acc = &mut calls[idx];
                    if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                        if !id.is_empty() {
                            acc.id = id.to_string();
                        }
                    }
                    if let Some(f) = tc.get("function") {
                        if let Some(name) = f.get("name").and_then(|v| v.as_str()) {
                            if !name.is_empty() {
                                acc.name = name.to_string();
                            }
                        }
                        if let Some(args) = f.get("arguments").and_then(|v| v.as_str()) {
                            acc.arguments.push_str(args);
                        }
                    }
                }
            }
        }

        let mut message = json!({ "role": role.unwrap_or_else(|| "assistant".into()) });
        if content.is_empty() {
            message["content"] = Value::Null;
        } else {
            message["content"] = Value::String(content);
        }
        if !calls.is_empty() {
            let arr: Vec<Value> = calls
                .into_iter()
                .map(|c| {
                    json!({
                        "id": c.id,
                        "type": "function",
                        "function": { "name": c.name, "arguments": c.arguments }
                    })
                })
                .collect();
            message["tool_calls"] = Value::Array(arr);
        }

        Ok(ChatResponse { message })
    }
}

#[derive(Default)]
struct ToolCallAcc {
    id: String,
    name: String,
    arguments: String,
}
