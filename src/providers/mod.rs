//! Chat-completion providers. The `Provider` trait abstracts the
//! one-shot `chat()` and the streaming `chat_stream()` used by headless runs and the line REPL over whatever wire
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
            let base_url = pcfg.resolved_base_url(provider_name)?;
            let cache = openai_compat::CacheStrategy::resolve(
                pcfg.cache.as_deref(),
                &base_url,
                &model_hint,
            );
            Ok(Box::new(openai_compat::OpenAICompatProvider::with_cache(
                base_url, api_key, cache,
            )))
        }
        "anthropic" => {
            let api_key = cfg.resolve_api_key(provider_name)?;
            Ok(Box::new(anthropic::AnthropicProvider::new(
                pcfg.resolved_base_url(provider_name)?,
                api_key,
            )))
        }
        // ChatGPT subscription auth. Note this deliberately does *not*
        // call `resolve_api_key` — credentials come from `oli login`,
        // and refresh has to happen per-request rather than once here.
        // `base_url` is optional for this kind: there is exactly one
        // endpoint it could mean.
        "openai-chatgpt" => {
            let base_url = pcfg.resolved_base_url(provider_name)?;
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

#[cfg(test)]
mod build_tests {
    //! Regression cover for the provider factory.
    //!
    //! The point of these is not the new provider kind — it is that
    //! adding it changed nothing for the existing ones. Subscription
    //! auth must never become a path an API-key user can fall into by
    //! accident, so these assert the old kinds still resolve keys the
    //! same way, still produce the same errors, and never consult the
    //! token store.

    use super::*;

    /// An OpenRouter provider reading `env_var`. Parameterised because
    /// tests run in one process: two tests sharing an env var name
    /// would race, one clearing what the other just set.
    fn openrouter_config(env_var: &str) -> Config {
        Config::from_str(&format!(
            r#"
            default_provider = "openrouter"

            [providers.openrouter]
            kind          = "openai-compat"
            base_url      = "https://openrouter.ai/api/v1"
            api_key_env   = "{env_var}"
            default_model = "anthropic/claude-haiku-4.5"
            "#
        ))
        .unwrap()
    }

    const OLLAMA: &str = r#"
        default_provider = "ollama"

        [providers.ollama]
        kind     = "openai-compat"
        base_url = "http://localhost:11434/v1"
        api_key  = "ollama"
    "#;

    const ANTHROPIC: &str = r#"
        default_provider = "anthropic"

        [providers.anthropic]
        kind        = "anthropic"
        base_url    = "https://api.anthropic.com"
        api_key     = "sk-ant-test"
    "#;

    #[test]
    fn openrouter_still_builds_from_its_env_var() {
        const VAR: &str = "__OLI_TEST_OPENROUTER_KEY_PRESENT";
        // SAFETY: uniquely-named test var, removed at the end.
        unsafe { std::env::set_var(VAR, "sk-or-test") };
        let cfg = openrouter_config(VAR);

        assert!(build(&cfg, "openrouter").is_ok());
        assert_eq!(cfg.resolve_api_key("openrouter").unwrap(), "sk-or-test");

        unsafe { std::env::remove_var(VAR) };
    }

    #[test]
    fn openrouter_without_its_env_var_fails_exactly_as_before() {
        const VAR: &str = "__OLI_TEST_OPENROUTER_KEY_ABSENT";
        // SAFETY: as above.
        unsafe { std::env::remove_var(VAR) };
        let cfg = openrouter_config(VAR);

        let err = build(&cfg, "openrouter").err().unwrap().to_string();
        assert!(err.contains(VAR), "{err}");
        assert!(err.contains("is not set"), "{err}");
        // Crucially: it must not suggest, mention, or fall back to
        // subscription auth.
        assert!(!err.contains("oli login"), "{err}");
        assert!(!err.contains("ChatGPT"), "{err}");
    }

    #[test]
    fn a_literal_api_key_provider_still_builds() {
        let cfg = Config::from_str(OLLAMA).unwrap();
        assert!(build(&cfg, "ollama").is_ok());
    }

    #[test]
    fn the_anthropic_kind_still_builds() {
        let cfg = Config::from_str(ANTHROPIC).unwrap();
        assert!(build(&cfg, "anthropic").is_ok());
    }

    #[test]
    fn env_default_config_still_targets_openrouter_over_chat_completions() {
        // The zero-config path must be untouched by any of this.
        let cfg = Config::env_default();
        assert_eq!(cfg.default_provider, "openrouter");
        let p = cfg.provider("openrouter").unwrap();
        assert_eq!(p.kind, "openai-compat");
        assert_eq!(p.api_key_env.as_deref(), Some("OPENROUTER_API_KEY"));
    }

    #[test]
    fn openrouter_claude_cache_autodetection_is_unchanged() {
        // Phase D behaviour: an OpenRouter base_url plus a Claude-shaped
        // model implies Anthropic-style cache breakpoints.
        use openai_compat::CacheStrategy;
        assert_eq!(
            CacheStrategy::resolve(
                None,
                "https://openrouter.ai/api/v1",
                "anthropic/claude-haiku-4.5"
            ),
            CacheStrategy::Anthropic
        );
        assert_eq!(
            CacheStrategy::resolve(None, "http://localhost:11434/v1", "qwen3-coder:30b"),
            CacheStrategy::None
        );
    }

    #[test]
    fn an_unknown_kind_lists_all_three_supported_kinds() {
        let cfg = Config::from_str(
            r#"
            default_provider = "x"
            [providers.x]
            kind     = "nope"
            base_url = "http://example"
            "#,
        )
        .unwrap();

        let err = build(&cfg, "x").err().unwrap().to_string();
        assert!(err.contains("openai-compat"), "{err}");
        assert!(err.contains("anthropic"), "{err}");
        assert!(err.contains("openai-chatgpt"), "{err}");
    }

    #[test]
    fn the_chatgpt_kind_does_not_require_an_api_key() {
        // Its credentials come from `oli login`, so a provider block
        // with no api_key must still construct — the store is read
        // lazily on first request, not here.
        let cfg = Config::from_str(
            r#"
            default_provider = "chatgpt"
            [providers.chatgpt]
            kind     = "openai-chatgpt"
            base_url = "https://chatgpt.com/backend-api/codex"
            "#,
        )
        .unwrap();

        // Only fails if no config directory can be resolved at all.
        if std::env::var_os("HOME").is_some() || std::env::var_os("XDG_CONFIG_HOME").is_some() {
            assert!(build(&cfg, "chatgpt").is_ok());
        }
    }

    #[tokio::test]
    async fn a_chatgpt_provider_with_no_credentials_names_the_api_key_fallback() {
        // The end-to-end shape of the "rejected subscription auth"
        // path: no tokens on disk, so the first request explains both
        // how to sign in and how to stop needing to.
        use crate::auth::session::ChatGptAuth;
        use crate::auth::store::AuthStore;

        let dir = tempfile::tempdir().unwrap();
        let auth = ChatGptAuth::with_store(AuthStore::at(dir.path().join("auth.json")));
        let provider = openai_responses::ResponsesProvider::new("http://127.0.0.1:1", auth);

        let err = provider
            .chat(ChatRequest {
                model: "gpt-5.1-codex".into(),
                messages: vec![serde_json::json!({"role": "user", "content": "hi"})],
                tools: vec![],
            })
            .await
            .unwrap_err()
            .to_string();

        assert!(err.contains("not signed in"), "{err}");
        assert!(err.contains("oli login"), "{err}");
        assert!(err.contains("OPENAI_API_KEY"), "{err}");
    }
}

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

/// Per-call token accounting. Every field is optional because "the
/// provider didn't report this" has to stay distinguishable from "the
/// provider reported zero": a count that degrades to 0 understates
/// without ever saying so. `Usage::default()` is "nothing known".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    pub total_tokens: Option<u32>,
    /// Prompt tokens the provider served from its cache.
    pub cache_read_tokens: Option<u32>,
    /// Prompt tokens the provider wrote into its cache.
    pub cache_write_tokens: Option<u32>,
    /// Completion tokens spent on reasoning the model doesn't return.
    pub reasoning_tokens: Option<u32>,
}

impl Usage {
    /// Parse the OpenAI chat-completions `usage` object. Returns `None`
    /// when the object carries no counts at all.
    pub fn from_value(v: &Value) -> Option<Self> {
        let prompt = token_count(v, "prompt_tokens");
        let completion = token_count(v, "completion_tokens");
        let usage = Self {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: token_count(v, "total_tokens")
                .or_else(|| derived_total(prompt, completion)),
            cache_read_tokens: v
                .get("prompt_tokens_details")
                .and_then(|d| token_count(d, "cached_tokens")),
            // Only GPT-5.6-class models report cache writes; earlier
            // ones omit the key, which is unknown rather than zero.
            cache_write_tokens: v
                .get("prompt_tokens_details")
                .and_then(|d| token_count(d, "cache_write_tokens")),
            reasoning_tokens: v
                .get("completion_tokens_details")
                .and_then(|d| token_count(d, "reasoning_tokens")),
        };
        usage.reports_anything().then_some(usage)
    }

    pub(crate) fn reports_anything(&self) -> bool {
        self.prompt_tokens
            .or(self.completion_tokens)
            .or(self.total_tokens)
            .or(self.cache_read_tokens)
            .or(self.cache_write_tokens)
            .or(self.reasoning_tokens)
            .is_some()
    }
}

pub(crate) fn token_count(v: &Value, key: &str) -> Option<u32> {
    v.get(key)?.as_u64().map(|n| n as u32)
}

/// A total the wire omitted is the sum of its parts — but only once
/// both parts are known. Deriving it from one known part would be
/// inventing the other.
pub(crate) fn derived_total(prompt: Option<u32>, completion: Option<u32>) -> Option<u32> {
    Some(prompt?.saturating_add(completion?))
}

/// Sum of one token category across a session, alongside the number of
/// calls that never reported it — so the sum can say how much of the
/// session it actually covers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TokenTotal {
    tokens: Option<u64>,
    pub unreported_calls: u32,
}

impl TokenTotal {
    fn add(&mut self, value: Option<u32>) {
        match value {
            Some(n) => *self.tokens.get_or_insert(0) += u64::from(n),
            None => self.unreported_calls += 1,
        }
    }

    /// `None` when no call reported this category: the sum of nothing
    /// is unknown, not zero.
    pub fn reported(&self) -> Option<u64> {
        self.tokens
    }
}

/// Running token accounting for a session. Sums only reported values
/// and counts the silent calls per category, so a total can render as
/// "12,340 (2 of 5 calls did not report)" instead of quietly passing
/// off missing data as zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UsageTotals {
    /// Provider calls observed, whether or not they reported anything.
    pub calls: u32,
    pub prompt_tokens: TokenTotal,
    pub completion_tokens: TokenTotal,
    pub total_tokens: TokenTotal,
    pub cache_read_tokens: TokenTotal,
    pub cache_write_tokens: TokenTotal,
    pub reasoning_tokens: TokenTotal,
}

impl UsageTotals {
    pub fn add(&mut self, usage: Option<Usage>) {
        self.calls += 1;
        self.prompt_tokens.add(usage.and_then(|u| u.prompt_tokens));
        self.completion_tokens
            .add(usage.and_then(|u| u.completion_tokens));
        self.total_tokens.add(usage.and_then(|u| u.total_tokens));
        self.cache_read_tokens
            .add(usage.and_then(|u| u.cache_read_tokens));
        self.cache_write_tokens
            .add(usage.and_then(|u| u.cache_write_tokens));
        self.reasoning_tokens
            .add(usage.and_then(|u| u.reasoning_tokens));
    }
}

#[cfg(test)]
mod usage_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_usage_object_with_no_counts_at_all_is_not_usage() {
        assert_eq!(Usage::from_value(&json!({})), None);
        assert_eq!(Usage::from_value(&json!({"queue_time": 0.4})), None);
    }

    #[test]
    fn a_category_the_wire_omits_stays_unknown_rather_than_zero() {
        let u = Usage::from_value(&json!({"prompt_tokens": 10, "completion_tokens": 4})).unwrap();
        assert_eq!(u.prompt_tokens, Some(10));
        assert_eq!(u.completion_tokens, Some(4));
        assert_eq!(u.cache_read_tokens, None);
        assert_eq!(u.cache_write_tokens, None);
        assert_eq!(u.reasoning_tokens, None);
    }

    #[test]
    fn a_reported_zero_is_kept_apart_from_an_unreported_category() {
        let u = Usage::from_value(&json!({
            "prompt_tokens": 10,
            "completion_tokens": 4,
            "prompt_tokens_details": {"cached_tokens": 0}
        }))
        .unwrap();
        assert_eq!(u.cache_read_tokens, Some(0));
        assert_eq!(u.reasoning_tokens, None);
    }

    #[test]
    fn openai_token_details_populate_cache_read_write_and_reasoning() {
        let u = Usage::from_value(&json!({
            "prompt_tokens": 100,
            "completion_tokens": 20,
            "total_tokens": 120,
            "prompt_tokens_details": {"cached_tokens": 1920, "cache_write_tokens": 512},
            "completion_tokens_details": {"reasoning_tokens": 12}
        }))
        .unwrap();
        assert_eq!(u.cache_read_tokens, Some(1920));
        assert_eq!(u.cache_write_tokens, Some(512));
        assert_eq!(u.reasoning_tokens, Some(12));
    }

    #[test]
    fn a_model_too_old_to_report_cache_writes_leaves_them_unknown() {
        // Pre-GPT-5.6 models report cached_tokens and omit the write
        // counter entirely — which is unknown, not zero.
        let u = Usage::from_value(&json!({
            "prompt_tokens": 100,
            "completion_tokens": 20,
            "prompt_tokens_details": {"cached_tokens": 64}
        }))
        .unwrap();
        assert_eq!(u.cache_read_tokens, Some(64));
        assert_eq!(u.cache_write_tokens, None);
    }

    #[test]
    fn an_absent_total_is_derived_only_when_both_parts_are_known() {
        let both = Usage::from_value(&json!({"prompt_tokens": 3, "completion_tokens": 5})).unwrap();
        assert_eq!(both.total_tokens, Some(8));

        let half = Usage::from_value(&json!({"prompt_tokens": 3})).unwrap();
        assert_eq!(half.total_tokens, None);
    }

    #[test]
    fn a_wire_total_is_taken_as_given_even_when_it_differs_from_the_sum() {
        // The provider bills the total; deriving over the top of a
        // reported one would substitute our arithmetic for its answer.
        let u = Usage::from_value(&json!({
            "prompt_tokens": 3,
            "completion_tokens": 5,
            "total_tokens": 100
        }))
        .unwrap();
        assert_eq!(u.total_tokens, Some(100));
    }

    #[test]
    fn session_totals_sum_only_the_calls_that_reported() {
        let mut totals = UsageTotals::default();
        totals.add(Some(Usage {
            prompt_tokens: Some(10),
            completion_tokens: Some(4),
            total_tokens: Some(14),
            ..Usage::default()
        }));
        totals.add(None);
        totals.add(Some(Usage {
            prompt_tokens: Some(6),
            ..Usage::default()
        }));

        assert_eq!(totals.calls, 3);
        assert_eq!(totals.prompt_tokens.reported(), Some(16));
        assert_eq!(totals.prompt_tokens.unreported_calls, 1);
        assert_eq!(totals.completion_tokens.reported(), Some(4));
        assert_eq!(totals.completion_tokens.unreported_calls, 2);
    }

    #[test]
    fn a_category_no_call_reported_totals_to_unknown_not_zero() {
        let mut totals = UsageTotals::default();
        totals.add(Some(Usage {
            prompt_tokens: Some(10),
            ..Usage::default()
        }));
        totals.add(None);

        assert_eq!(totals.cache_read_tokens.reported(), None);
        assert_eq!(totals.cache_read_tokens.unreported_calls, 2);
    }

    #[test]
    fn a_call_that_reported_nothing_still_counts_as_a_call() {
        let mut totals = UsageTotals::default();
        totals.add(None);
        totals.add(None);
        assert_eq!(totals.calls, 2);
        assert_eq!(totals.prompt_tokens.reported(), None);
        assert_eq!(totals.prompt_tokens.unreported_calls, 2);
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
