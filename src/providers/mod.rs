use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;

use crate::error::Result;

pub mod openai_compat;

#[cfg(test)]
pub mod fake;

#[derive(Clone, Debug, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Value>,
    pub tools: Vec<Value>,
}

#[derive(Clone, Debug)]
pub struct ChatResponse {
    /// Raw assistant message in OpenAI shape: `{role, content?, tool_calls?}`.
    pub message: Value,
}

#[async_trait]
pub trait Provider: Send + Sync {
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse>;
}
