//! Chat-completion providers. The `Provider` trait abstracts the
//! one-shot `chat()` (used by `-p` mode) and the streaming
//! `chat_stream()` (used by the TUI / REPL) over whatever wire
//! protocol the underlying API speaks.
//!
//! Bundled implementations:
//! - [`anthropic`] — Anthropic's native Messages API. Streams via
//!   SSE. Knows how to surface usage tokens and stream
//!   `tool_use` blocks back to the agent loop.
//! - [`openai_compat`] — OpenAI / OpenRouter / Ollama / vLLM /
//!   anything that speaks OpenAI's `/chat/completions` shape.
//! - [`openai_responses`] — ChatGPT Plus/Pro subscription auth. Not a
//!   variant of the above: it speaks the Responses API against
//!   `chatgpt.com/backend-api/codex` and authenticates with OAuth
//!   tokens from `oli login` rather than an API key.
//!
//! `build()` is the factory the binary calls at startup: it reads
//! `default_provider` from `Config`, instantiates the matching
//! `[providers.<name>]` block, and returns a boxed `Provider`.
//!
//! Adding a new provider: implement `Provider`, return the boxed
//! impl from `build()` for the new `kind`, and the rest of the
//! harness (agent loop, tool registry, policy, memory) doesn't
//! need to know about it.

use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;

use crate::config::Config;
use crate::error::{AgentError, Result};

pub mod anthropic;
pub mod openai_compat;
pub mod openai_responses;

/// Centralized provider factory. Mapping `kind -> impl Provider`
/// lives here so adding a new provider doesn't require touching
/// three separate dispatch sites (top-level `main.rs`, the
/// `AgentSpawner` for subagents, and the `/provider` slash command).
pub fn build(cfg: &Config, provider_name: &str) -> Result<Box<dyn Provider>> {
    let pcfg = cfg.provider(provider_name)?;
    match pcfg.kind.as_str() {
        "openai-compat" => {
            let api_key = cfg.resolve_api_key(provider_name)?;
            // The agent-loop active-model id is the right input for
            // cache auto-detection. Tracking which model is active at
            // factory-call time isn't trivial without a wider refactor,
            // so we use the provider's `default_model` as a hint here;
            // the OpenAI-compat provider gets the explicit
            // `cache = "anthropic"` config for non-default models.
            let model_hint = pcfg
                .default_model
                .clone()
                .or_else(|| cfg.default_model.clone())
                .unwrap_or_default();
            let cache = openai_compat::CacheStrategy::resolve(
                pcfg.cache.as_deref(),
                &pcfg.base_url,
                &model_hint,
            );
            Ok(Box::new(openai_compat::OpenAICompatProvider::with_cache(
                pcfg.base_url.clone(),
                api_key,
                cache,
            )))
        }
        "anthropic" => {
            let api_key = cfg.resolve_api_key(provider_name)?;
            Ok(Box::new(anthropic::AnthropicProvider::new(
                pcfg.base_url.clone(),
                api_key,
            )))
        }
        // ChatGPT subscription auth. Note this deliberately does *not*
        // call `resolve_api_key` — credentials come from `oli login`,
        // and refresh has to happen per-request rather than once here.
        // An empty `base_url` falls back to the subscription endpoint,
        // so a minimal provider block is just `kind` plus a model.
        "openai-chatgpt" => {
            let base_url = if pcfg.base_url.trim().is_empty() {
                crate::auth::CHATGPT_BASE_URL.to_string()
            } else {
                pcfg.base_url.clone()
            };
            let auth = crate::auth::session::ChatGptAuth::new()?;
            Ok(Box::new(openai_responses::ResponsesProvider::new(
                base_url, auth,
            )))
        }
        other => Err(AgentError::Config(format!(
            "unsupported provider kind '{other}' for '{provider_name}' \
             (try 'openai-compat', 'anthropic', or 'openai-chatgpt')"
        ))),
    }
}

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

/// Event emitted by a streaming provider to its sink. Phase Y2 widened the
/// sink from a `FnMut(&str)` (content-only) to an enum so providers can also
/// surface incremental tool-call arguments to the UI before the call is
/// dispatched. New variants land here when the streaming protocol grows
/// (thinking deltas, image deltas, etc.) — keep this enum non-exhaustive in
/// spirit by ignoring unknown variants at the consumer when possible.
#[derive(Debug)]
pub enum StreamEvent<'a> {
    /// One token (or run of tokens) of assistant *text* content. The same
    /// payload that the old `ContentSink(&str)` carried.
    Content(&'a str),
    /// One chunk of the streaming JSON `arguments` for a tool call the model
    /// is composing. `provider_tool_id` is the provider's own id for the
    /// call (Anthropic `tool_use.id`, OpenAI `tool_calls[].id`); the same id
    /// will be present on the dispatched tool's hook event so the UI can
    /// correlate the streaming preview with the eventual `ToolStart`.
    /// `accumulated_json` is the full JSON-so-far (sink doesn't need to
    /// re-buffer); `partial_json` is just the delta from this chunk.
    ToolArgsChunk {
        provider_tool_id: &'a str,
        name: &'a str,
        partial_json: &'a str,
        accumulated_json: &'a str,
    },
}

/// Sink for streamed assistant events. Re-borrowed across multiple loop
/// iterations so callers can reuse a single closure for a whole session.
pub type StreamSink<'a> = &'a mut (dyn FnMut(StreamEvent<'_>) + Send);

#[async_trait]
pub trait Provider: Send + Sync {
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse>;

    /// Streaming variant. Calls `sink` with stream events as they arrive and
    /// returns the fully assembled assistant message — including any
    /// `tool_calls` — once the stream completes.
    ///
    /// The default implementation falls back to non-streaming `chat` and
    /// emits the entire content in a single `StreamEvent::Content` call.
    /// Real providers should override this to deliver tokens incrementally.
    async fn chat_stream(&self, req: ChatRequest, sink: StreamSink<'_>) -> Result<ChatResponse> {
        let resp = self.chat(req).await?;
        if let Some(s) = resp.message.get("content").and_then(|v| v.as_str()) {
            if !s.is_empty() {
                sink(StreamEvent::Content(s));
            }
        }
        Ok(resp)
    }

    /// Enumerate model ids the provider can serve. Used by the `/model`
    /// slash command. Default returns an empty list — providers that
    /// don't expose a discovery endpoint shouldn't error, just say
    /// nothing.
    async fn list_models(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
}
