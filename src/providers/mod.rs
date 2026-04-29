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
    /// Per-call token accounting, when the provider supplies it. Streaming
    /// requests must opt into `stream_options.include_usage` to populate
    /// this field.
    pub usage: Option<Usage>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

impl Usage {
    pub fn from_value(v: &Value) -> Option<Self> {
        let prompt = v.get("prompt_tokens").and_then(|x| x.as_u64())?;
        let completion = v.get("completion_tokens").and_then(|x| x.as_u64())?;
        let total = v
            .get("total_tokens")
            .and_then(|x| x.as_u64())
            .unwrap_or(prompt + completion);
        Some(Self {
            prompt_tokens: prompt as u32,
            completion_tokens: completion as u32,
            total_tokens: total as u32,
        })
    }
}

/// Sink for streamed assistant content tokens. Re-borrowed across multiple
/// loop iterations so callers can reuse a single closure for a whole session.
pub type ContentSink<'a> = &'a mut (dyn FnMut(&str) + Send);

#[async_trait]
pub trait Provider: Send + Sync {
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse>;

    /// Streaming variant. Calls `sink` with content deltas as they arrive and
    /// returns the fully assembled assistant message — including any
    /// `tool_calls` — once the stream completes.
    ///
    /// The default implementation falls back to non-streaming `chat` and
    /// emits the entire content in a single sink call. Real providers should
    /// override this to deliver tokens incrementally.
    async fn chat_stream(&self, req: ChatRequest, sink: ContentSink<'_>) -> Result<ChatResponse> {
        let resp = self.chat(req).await?;
        if let Some(s) = resp.message.get("content").and_then(|v| v.as_str()) {
            if !s.is_empty() {
                sink(s);
            }
        }
        Ok(resp)
    }
}
