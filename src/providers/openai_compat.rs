use async_openai::{Client, config::OpenAIConfig};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::error::{AgentError, Result};
use crate::providers::{ChatRequest, ChatResponse, Provider};

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
}
