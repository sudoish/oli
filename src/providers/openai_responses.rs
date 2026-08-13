//! ChatGPT-subscription provider — OpenAI's **Responses API**.
//!
//! This is not a variant of [`openai_compat`](super::openai_compat).
//! Subscription-authenticated requests go to
//! `https://chatgpt.com/backend-api/codex/responses`, which speaks a
//! different protocol from Chat Completions:
//!
//! | | Chat Completions | Responses |
//! |---|---|---|
//! | conversation | `messages[]` | `input[]` items |
//! | system prompt | a `system` message | top-level `instructions` |
//! | tool schema | `{type, function:{…}}` | flat `{type:"function", name, …}` |
//! | tool call | `message.tool_calls[]` | `function_call` items |
//! | tool result | `role:"tool"` message | `function_call_output` items |
//! | streaming | `choices[].delta` | typed `response.*` events |
//!
//! Oli's [`ChatRequest`]/[`ChatResponse`] stay Chat-Completions-shaped,
//! so this module is mostly a translation layer in both directions.
//! The translation is pure and unit-tested against fixtures; only
//! [`ResponsesProvider`] itself touches the network.
//!
//! # Models
//!
//! The subscription serves a **different, smaller model set than the
//! public API, with different slugs**. Neither `gpt-4o` nor any
//! `gpt-5.x-codex` name is accepted; at the time of writing the list
//! was `gpt-5.6-terra`, `gpt-5.6-luna`, `gpt-5.5`, `gpt-5.4-mini`.
//! Since that will change, nothing here hardcodes a slug —
//! [`ResponsesProvider::fetch_models`] asks the `/models` endpoint,
//! and `oli login` writes the answer into the config.
//!
//! A wrong slug is rejected with `"The '<model>' model is not
//! supported when using Codex with a ChatGPT account."`, which is
//! surfaced verbatim.
//!
//! # Verified against the live endpoint
//!
//! The request shape, auth headers and streaming decode have been
//! exercised end-to-end against a real ChatGPT subscription. Fields
//! the backend *requires* are still undocumented, so errors are
//! surfaced verbatim rather than interpreted — see
//! [`describe_endpoint_error`].
//!
//! # Auth
//!
//! Credentials come from [`ChatGptAuth`], which refreshes before
//! expiry. A 401 is retried exactly once after a forced refresh,
//! because the server is the authority on token validity. A second
//! 401 fails loudly and names API-key auth.

use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::HashMap;

use crate::auth::session::ChatGptAuth;
use crate::error::{AgentError, Result};
use crate::providers::{
    ChatRequest, ChatResponse, Provider, StreamEvent, StreamSink, Usage, derived_total, token_count,
};

/// Client identifier sent as the `originator` header.
///
/// The subscription backend is not a public API and appears to gate on
/// this. Oli sends the same value OpenAI's own CLI does, for the same
/// reason it uses the same OAuth client id: there is no registration
/// path that would let it identify itself as anything else and still
/// be served. Override with [`ORIGINATOR_ENV`].
pub const ORIGINATOR: &str = "codex_cli_rs";

/// Env var overriding [`ORIGINATOR`].
pub const ORIGINATOR_ENV: &str = "OLI_CHATGPT_ORIGINATOR";

fn originator() -> String {
    std::env::var(ORIGINATOR_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| ORIGINATOR.to_string())
}

/// `client_version` sent to `/models`.
///
/// This is a **capability gate, not an identity**: the server filters
/// the catalogue by it, returning only models the stated version is
/// expected to handle. Sending Oli's own `0.1.0` returns an empty
/// list — as do `0.9.0`, `0.20.0` and `0.45.0` — while `0.104.0`
/// returns only the two oldest models. Anything from `1.0.0` up
/// returns the current catalogue.
///
/// So this declares "implements the current Responses protocol",
/// which is accurate: every model behind this gate was verified to
/// work with the plain streaming request built by [`build_request`].
/// The `use_responses_lite` and `code_mode_only` flags in the
/// catalogue are hints for OpenAI's own client optimisations, not
/// requirements.
///
/// Override with [`CLIENT_VERSION_ENV`] if the gate moves again.
pub const MODELS_CLIENT_VERSION: &str = "1.0.0";

/// Env var overriding [`MODELS_CLIENT_VERSION`].
pub const CLIENT_VERSION_ENV: &str = "OLI_CHATGPT_CLIENT_VERSION";

fn models_client_version() -> String {
    std::env::var(CLIENT_VERSION_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| MODELS_CLIENT_VERSION.to_string())
}

/// Provider that speaks the Responses API with subscription auth.
pub struct ResponsesProvider {
    client: reqwest::Client,
    base_url: String,
    auth: ChatGptAuth,
    /// Stable per-process id, echoed on every request the way a
    /// session-scoped client would.
    session_id: String,
}

impl ResponsesProvider {
    pub fn new(base_url: impl Into<String>, auth: ChatGptAuth) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            auth,
            session_id: random_session_id(),
        }
    }

    fn endpoint(&self) -> String {
        format!("{}/responses", self.base_url)
    }

    /// Send one request, attaching auth headers. Returns the raw
    /// response so callers can decide how to read the body.
    async fn send(&self, payload: &Value, retry_after_401: bool) -> Result<reqwest::Response> {
        let creds = self.auth.credentials().await?;

        let mut builder = self
            .client
            .post(self.endpoint())
            .bearer_auth(&creds.bearer)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .header("originator", originator())
            .header("session_id", &self.session_id);
        if let Some(account_id) = &creds.account_id {
            // Workspace accounts are rejected without this.
            builder = builder.header("ChatGPT-Account-ID", account_id);
        }

        let resp = builder.json(payload).send().await.map_err(|e| {
            AgentError::Provider(format!(
                "request to {} failed: {e}. If ChatGPT subscription access is not working, \
                 switch this provider to API-key auth (`kind = \"openai-compat\"`, \
                 `base_url = \"https://api.openai.com/v1\"`, \
                 `api_key_env = \"OPENAI_API_KEY\"`).",
                self.endpoint()
            ))
        })?;

        // The server is the authority on token validity, so a 401 gets
        // exactly one forced-refresh retry before we believe it.
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED && retry_after_401 {
            self.auth.force_refresh().await?;
            return Box::pin(self.send(payload, false)).await;
        }
        Ok(resp)
    }

    /// Model slugs this subscription can actually use.
    ///
    /// The subscription backend serves a *different, smaller* model set
    /// than the public API, and the slugs do not match the public ones
    /// — no `gpt-4o`, and no `gpt-5.x-codex`. Guessing produces
    /// `"The '<model>' model is not supported when using Codex with a
    /// ChatGPT account."`, so this asks instead.
    pub async fn fetch_models(&self) -> Result<Vec<ModelInfo>> {
        let creds = self.auth.credentials().await?;
        // `client_version` gates which models come back — see
        // MODELS_CLIENT_VERSION. Sending Oli's own version returns an
        // empty catalogue.
        let url = format!(
            "{}/models?client_version={}",
            self.base_url,
            models_client_version()
        );

        let mut builder = self
            .client
            .get(&url)
            .bearer_auth(&creds.bearer)
            .header("originator", originator());
        if let Some(account_id) = &creds.account_id {
            builder = builder.header("ChatGPT-Account-ID", account_id);
        }

        let resp = builder
            .send()
            .await
            .map_err(|e| AgentError::Provider(format!("could not reach {url}: {e}")))?;
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        if !(200..300).contains(&status) {
            return Err(AgentError::Provider(describe_endpoint_error(
                status,
                &body,
                self.auth.store_path(),
            )));
        }

        let models = parse_models(&body)?;
        if models.is_empty() {
            // Never a legitimate answer for a working subscription:
            // it means the version gate moved and now excludes us.
            return Err(AgentError::Provider(format!(
                "the subscription returned an empty model list for client_version \
                 {}. OpenAI gates the catalogue on that value, so it has probably \
                 moved. Try setting {CLIENT_VERSION_ENV} to a higher version, or set \
                 `default_model` by hand.",
                models_client_version()
            )));
        }
        Ok(models)
    }

    /// Turn a non-2xx response into a loud provider error.
    async fn error_from(&self, resp: reqwest::Response) -> AgentError {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        AgentError::Provider(describe_endpoint_error(
            status,
            &body,
            self.auth.store_path(),
        ))
    }
}

/// One entry from the subscription's model catalogue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelInfo {
    pub slug: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub context_window: Option<u32>,
}

/// Parse the `/models` response.
///
/// Only `slug` is required; every other field is presentation. The
/// catalogue gains fields regularly, so unknown ones are ignored
/// rather than rejected.
pub fn parse_models(body: &str) -> Result<Vec<ModelInfo>> {
    let parsed: Value = serde_json::from_str(body)
        .map_err(|e| AgentError::Provider(format!("could not parse the model list ({e})")))?;
    let entries = parsed
        .get("models")
        .and_then(|m| m.as_array())
        .ok_or_else(|| {
            AgentError::Provider("the model list response had no `models` array".to_string())
        })?;

    Ok(entries
        .iter()
        .filter_map(|m| {
            let slug = m.get("slug").and_then(|v| v.as_str())?;
            Some(ModelInfo {
                slug: slug.to_string(),
                display_name: m
                    .get("display_name")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                description: m
                    .get("description")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                context_window: m
                    .get("context_window")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32),
            })
        })
        .collect())
}

/// Pick a sensible default from a catalogue.
///
/// The list arrives newest-first, so the first general-purpose entry
/// is the right default. Entries that are clearly not conversational
/// models are skipped — `codex-auto-review` is a review-only model and
/// would be a poor default for an interactive agent.
pub fn preferred_model(models: &[ModelInfo]) -> Option<&ModelInfo> {
    models
        .iter()
        .find(|m| !m.slug.contains("auto-review"))
        .or_else(|| models.first())
}

/// Compose the user-facing message for a rejected request.
///
/// Kept separate and tested because this is the message that appears
/// the day OpenAI stops serving third-party clients, and a cryptic
/// failure that day is the worst outcome.
pub fn describe_endpoint_error(status: u16, body: &str, credentials: &std::path::Path) -> String {
    let detail = extract_error_message(body);
    let hint = match status {
        401 | 403 => {
            "The ChatGPT credentials were rejected. Run `oli login` to sign in again. \
             If it still fails, your plan may not include this, or OpenAI may have stopped \
             serving third-party clients."
        }
        429 => "Subscription rate limit or quota reached. Wait and retry.",
        404 => "The subscription endpoint was not found. OpenAI may have moved or withdrawn it.",
        _ => "The subscription endpoint returned an error.",
    };
    format!(
        "ChatGPT subscription request failed (HTTP {status}): {detail}. {hint} \
         API-key auth always works as a fallback: set `kind = \"openai-compat\"`, \
         `base_url = \"https://api.openai.com/v1\"`, `api_key_env = \"OPENAI_API_KEY\"` \
         on this provider. Credentials: {}",
        credentials.display()
    )
}

/// Pull a message out of an error body, whatever shape it arrived in.
fn extract_error_message(body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        for path in [
            &["error", "message"][..],
            &["error", "error"][..],
            &["message"][..],
            &["detail"][..],
        ] {
            let mut cur = &v;
            let mut ok = true;
            for key in path {
                match cur.get(key) {
                    Some(next) => cur = next,
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok && let Some(s) = cur.as_str() {
                return s.to_string();
            }
        }
    }
    let trimmed = body.trim();
    if trimmed.is_empty() {
        "no response body".to_string()
    } else {
        trimmed.chars().take(400).collect()
    }
}

/// 16 random bytes rendered as a UUIDv4-shaped string. Avoids a `uuid`
/// dependency for what is only an opaque correlation id.
fn random_session_id() -> String {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::rng().fill_bytes(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = |r: &[u8]| r.iter().map(|x| format!("{x:02x}")).collect::<String>();
    format!(
        "{}-{}-{}-{}-{}",
        h(&b[0..4]),
        h(&b[4..6]),
        h(&b[6..8]),
        h(&b[8..10]),
        h(&b[10..16])
    )
}

// ---------------------------------------------------------------
// Request translation: Chat Completions shape -> Responses shape
// ---------------------------------------------------------------

/// Build a Responses API request body from Oli's Chat-Completions-shaped
/// [`ChatRequest`].
///
/// `stream` is always true: the subscription backend is only known to
/// be exercised in streaming mode, and `chat()` drives the same path
/// with a discarding sink rather than sending an unproven shape.
pub fn build_request(req: &ChatRequest) -> Value {
    let (instructions, input) = split_messages(&req.messages);
    let mut payload = json!({
        "model": req.model,
        "input": input,
        "tools": translate_tools(&req.tools),
        "tool_choice": "auto",
        "parallel_tool_calls": true,
        "stream": true,
        // We are not using server-side conversation state, so nothing
        // should be retained on OpenAI's side beyond the request.
        "store": false,
    });
    if let Some(instructions) = instructions {
        payload["instructions"] = Value::String(instructions);
    }
    payload
}

/// Split chat messages into top-level `instructions` and `input` items.
///
/// System messages become instructions (joined, in order) because the
/// Responses API has no system role. Everything else becomes an input
/// item.
fn split_messages(messages: &[Value]) -> (Option<String>, Vec<Value>) {
    let mut instructions: Vec<String> = Vec::new();
    let mut input: Vec<Value> = Vec::new();

    for msg in messages {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");
        match role {
            "system" | "developer" => {
                if let Some(text) = content_as_text(msg.get("content")) {
                    instructions.push(text);
                }
            }
            "tool" => {
                // A tool result. `call_id` must match the id the model
                // used, which Oli carries as `tool_call_id`.
                let call_id = msg
                    .get("tool_call_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": content_as_text(msg.get("content")).unwrap_or_default(),
                }));
            }
            "assistant" => {
                if let Some(text) = content_as_text(msg.get("content"))
                    && !text.is_empty()
                {
                    input.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": text }],
                    }));
                }
                // Assistant tool calls are separate items, not a field
                // on the message.
                if let Some(calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                    for call in calls {
                        let function = call.get("function");
                        input.push(json!({
                            "type": "function_call",
                            "call_id": call.get("id").and_then(|v| v.as_str()).unwrap_or_default(),
                            "name": function
                                .and_then(|f| f.get("name"))
                                .and_then(|v| v.as_str())
                                .unwrap_or_default(),
                            "arguments": function
                                .and_then(|f| f.get("arguments"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("{}"),
                        }));
                    }
                }
            }
            _ => {
                input.push(json!({
                    "type": "message",
                    "role": "user",
                    "content": [{
                        "type": "input_text",
                        "text": content_as_text(msg.get("content")).unwrap_or_default(),
                    }],
                }));
            }
        }
    }

    let instructions = (!instructions.is_empty()).then(|| instructions.join("\n\n"));
    (instructions, input)
}

/// Flatten message content to text. Handles both the string form and
/// the array-of-blocks form Oli's cache path produces.
fn content_as_text(content: Option<&Value>) -> Option<String> {
    match content? {
        Value::String(s) => Some(s.clone()),
        Value::Array(blocks) => {
            let joined = blocks
                .iter()
                .filter_map(|b| {
                    b.get("text")
                        .and_then(|t| t.as_str())
                        .map(|s| s.to_string())
                        .or_else(|| b.as_str().map(|s| s.to_string()))
                })
                .collect::<Vec<_>>()
                .join("");
            Some(joined)
        }
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

/// Chat-Completions tool schemas are nested under `function`; Responses
/// tool schemas are flat.
fn translate_tools(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .filter_map(|tool| {
            let function = tool.get("function").unwrap_or(tool);
            let name = function.get("name").and_then(|v| v.as_str())?;
            Some(json!({
                "type": "function",
                "name": name,
                "description": function
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default(),
                "parameters": function
                    .get("parameters")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
            }))
        })
        .collect()
}

// ---------------------------------------------------------------
// Response translation: Responses events -> Chat Completions shape
// ---------------------------------------------------------------

/// A function call being assembled from stream events.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct CallAcc {
    call_id: String,
    name: String,
    arguments: String,
}

/// Accumulated stream state.
#[derive(Debug, Default)]
pub struct ResponsesAcc {
    content: String,
    calls: Vec<CallAcc>,
    /// Maps a stream `item_id` to its index in `calls`, since argument
    /// deltas reference the item rather than a positional index.
    by_item: HashMap<String, usize>,
    usage: Option<Usage>,
}

/// Something the accumulator wants surfaced to the UI. Owned rather
/// than borrowed so `apply_event` can be tested without a sink.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Emit {
    Text(String),
    ToolArgs { index: usize, partial: String },
}

impl ResponsesAcc {
    /// Feed one decoded SSE payload in. Returns anything the UI should
    /// see, or an error if the stream reported one.
    pub fn apply_event(&mut self, event: &Value) -> Result<Vec<Emit>> {
        let kind = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let mut emits = Vec::new();

        match kind {
            "response.output_text.delta" => {
                if let Some(delta) = event.get("delta").and_then(|v| v.as_str())
                    && !delta.is_empty()
                {
                    self.content.push_str(delta);
                    emits.push(Emit::Text(delta.to_string()));
                }
            }
            "response.output_item.added" => {
                if let Some(item) = event.get("item")
                    && item.get("type").and_then(|v| v.as_str()) == Some("function_call")
                {
                    let idx = self.ensure_call(item_id(event, item));
                    merge_call(&mut self.calls[idx], item);
                }
            }
            "response.function_call_arguments.delta" => {
                let id = event
                    .get("item_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let delta = event
                    .get("delta")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if !delta.is_empty() {
                    let idx = self.ensure_call(id);
                    self.calls[idx].arguments.push_str(delta);
                    emits.push(Emit::ToolArgs {
                        index: idx,
                        partial: delta.to_string(),
                    });
                }
            }
            "response.output_item.done" => {
                if let Some(item) = event.get("item") {
                    match item.get("type").and_then(|v| v.as_str()) {
                        Some("function_call") => {
                            let idx = self.ensure_call(item_id(event, item));
                            // The terminal item is authoritative: it
                            // carries the complete arguments even if a
                            // delta was dropped.
                            merge_call(&mut self.calls[idx], item);
                            if let Some(args) = item.get("arguments").and_then(|v| v.as_str()) {
                                self.calls[idx].arguments = args.to_string();
                            }
                        }
                        Some("message") => {
                            // Non-streaming text can arrive whole here.
                            if self.content.is_empty()
                                && let Some(text) = output_text_of(item)
                            {
                                self.content = text;
                            }
                        }
                        _ => {}
                    }
                }
            }
            "response.completed" => {
                if let Some(response) = event.get("response") {
                    if let Some(usage) = response.get("usage").and_then(usage_from_responses) {
                        self.usage = Some(usage);
                    }
                    // Belt and braces: harvest anything the deltas
                    // missed rather than returning an empty turn.
                    if let Some(output) = response.get("output").and_then(|v| v.as_array()) {
                        self.harvest(output);
                    }
                }
            }
            "response.failed" | "error" => {
                let detail = event
                    .get("response")
                    .and_then(|r| r.get("error"))
                    .or_else(|| event.get("error"))
                    .map(|e| extract_error_message(&e.to_string()))
                    .unwrap_or_else(|| "stream reported an error".to_string());
                return Err(AgentError::Provider(format!(
                    "ChatGPT subscription stream failed: {detail}. \
                     API-key auth remains available as a fallback."
                )));
            }
            _ => {}
        }

        Ok(emits)
    }

    /// Fold in complete output items, filling only what is missing.
    fn harvest(&mut self, output: &[Value]) {
        for item in output {
            match item.get("type").and_then(|v| v.as_str()) {
                Some("message") if self.content.is_empty() => {
                    if let Some(text) = output_text_of(item) {
                        self.content = text;
                    }
                }
                Some("function_call") => {
                    let id = item
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let known = self.by_item.contains_key(&id)
                        || self.calls.iter().any(|c| {
                            !c.call_id.is_empty()
                                && Some(c.call_id.as_str())
                                    == item.get("call_id").and_then(|v| v.as_str())
                        });
                    if !known {
                        let idx = self.ensure_call(id);
                        merge_call(&mut self.calls[idx], item);
                        if let Some(args) = item.get("arguments").and_then(|v| v.as_str()) {
                            self.calls[idx].arguments = args.to_string();
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Index of the call for `item_id`, creating it if new.
    fn ensure_call(&mut self, item_id: String) -> usize {
        if let Some(idx) = self.by_item.get(&item_id) {
            return *idx;
        }
        self.calls.push(CallAcc::default());
        let idx = self.calls.len() - 1;
        self.by_item.insert(item_id, idx);
        idx
    }

    /// The provider-visible id for the call at `index`, for
    /// [`StreamEvent::ToolArgsChunk`].
    fn call_id_at(&self, index: usize) -> &str {
        self.calls
            .get(index)
            .map(|c| c.call_id.as_str())
            .unwrap_or_default()
    }

    fn call_name_at(&self, index: usize) -> &str {
        self.calls
            .get(index)
            .map(|c| c.name.as_str())
            .unwrap_or_default()
    }

    fn call_args_at(&self, index: usize) -> &str {
        self.calls
            .get(index)
            .map(|c| c.arguments.as_str())
            .unwrap_or_default()
    }

    /// Assemble the Chat-Completions-shaped assistant message the rest
    /// of Oli expects.
    pub fn finish(self) -> ChatResponse {
        let mut message = json!({ "role": "assistant" });
        message["content"] = if self.content.is_empty() {
            Value::Null
        } else {
            Value::String(self.content)
        };
        let calls: Vec<Value> = self
            .calls
            .into_iter()
            .filter(|c| !c.name.is_empty())
            .map(|c| {
                json!({
                    "id": c.call_id,
                    "type": "function",
                    "function": {
                        "name": c.name,
                        "arguments": if c.arguments.is_empty() { "{}".to_string() } else { c.arguments },
                    }
                })
            })
            .collect();
        if !calls.is_empty() {
            message["tool_calls"] = Value::Array(calls);
        }
        ChatResponse {
            message,
            usage: self.usage,
        }
    }
}

/// Prefer the event's `item_id`, falling back to the item's own `id`.
fn item_id(event: &Value, item: &Value) -> String {
    event
        .get("item_id")
        .and_then(|v| v.as_str())
        .or_else(|| item.get("id").and_then(|v| v.as_str()))
        .unwrap_or_default()
        .to_string()
}

/// Copy non-empty fields from a `function_call` item into `acc`.
fn merge_call(acc: &mut CallAcc, item: &Value) {
    if let Some(call_id) = item.get("call_id").and_then(|v| v.as_str())
        && !call_id.is_empty()
    {
        acc.call_id = call_id.to_string();
    }
    if let Some(name) = item.get("name").and_then(|v| v.as_str())
        && !name.is_empty()
    {
        acc.name = name.to_string();
    }
}

/// Concatenate the `output_text` blocks of a message item.
fn output_text_of(item: &Value) -> Option<String> {
    let blocks = item.get("content")?.as_array()?;
    let text = blocks
        .iter()
        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
        .collect::<Vec<_>>()
        .join("");
    (!text.is_empty()).then_some(text)
}

/// Responses reports `input_tokens`/`output_tokens`; Oli's [`Usage`]
/// is named for the Chat Completions fields.
fn usage_from_responses(usage: &Value) -> Option<Usage> {
    let input = token_count(usage, "input_tokens");
    let output = token_count(usage, "output_tokens");
    let parsed = Usage {
        prompt_tokens: input,
        completion_tokens: output,
        total_tokens: token_count(usage, "total_tokens").or_else(|| derived_total(input, output)),
        cache_read_tokens: usage
            .get("input_tokens_details")
            .and_then(|d| token_count(d, "cached_tokens")),
        // The Responses API has no cache-write counter.
        cache_write_tokens: None,
        reasoning_tokens: usage
            .get("output_tokens_details")
            .and_then(|d| token_count(d, "reasoning_tokens")),
    };
    parsed.reports_anything().then_some(parsed)
}

#[async_trait]
impl Provider for ResponsesProvider {
    /// Non-streaming `chat` drives the streaming path with a
    /// discarding sink. The subscription backend is only known to be
    /// exercised with `stream: true`, and sending an unproven shape to
    /// find out would fail in a way that is hard to diagnose.
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
        let mut sink = |_: StreamEvent<'_>| {};
        self.chat_stream(req, &mut sink).await
    }

    async fn chat_stream(&self, req: ChatRequest, sink: StreamSink<'_>) -> Result<ChatResponse> {
        let payload = build_request(&req);
        let resp = self.send(&payload, true).await?;

        if !resp.status().is_success() {
            return Err(self.error_from(resp).await);
        }

        let mut stream = resp.bytes_stream().eventsource();
        let mut acc = ResponsesAcc::default();

        while let Some(event) = stream.next().await {
            let event = event.map_err(|e| AgentError::Provider(format!("SSE: {e}")))?;
            let data = event.data.trim();
            if data.is_empty() || data == "[DONE]" {
                continue;
            }
            let Ok(parsed) = serde_json::from_str::<Value>(data) else {
                continue;
            };

            for emit in acc.apply_event(&parsed)? {
                match emit {
                    Emit::Text(text) => sink(StreamEvent::Content(&text)),
                    Emit::ToolArgs { index, partial } => {
                        // Skip until the call has an id and a name; the
                        // UI keys its streaming preview on those.
                        let id = acc.call_id_at(index);
                        let name = acc.call_name_at(index);
                        if !id.is_empty() && !name.is_empty() {
                            sink(StreamEvent::ToolArgsChunk {
                                provider_tool_id: id,
                                name,
                                partial_json: &partial,
                                accumulated_json: acc.call_args_at(index),
                            });
                        }
                    }
                }
            }
        }

        Ok(acc.finish())
    }

    /// Backs the `/model` slash command. Returns only what this
    /// subscription actually serves.
    async fn list_models(&self) -> Result<Vec<String>> {
        Ok(self
            .fetch_models()
            .await?
            .into_iter()
            .map(|m| m.slug)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(messages: Vec<Value>, tools: Vec<Value>) -> ChatRequest {
        ChatRequest {
            model: "gpt-5.1-codex".into(),
            messages,
            tools,
        }
    }

    // ---- Request translation -------------------------------------

    #[test]
    fn system_messages_become_top_level_instructions() {
        // The Responses API has no system role.
        let req = request(
            vec![
                json!({"role": "system", "content": "be terse"}),
                json!({"role": "user", "content": "hi"}),
            ],
            vec![],
        );
        let body = build_request(&req);
        assert_eq!(body["instructions"], "be terse");
        assert_eq!(body["input"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn multiple_system_messages_are_joined_in_order() {
        let req = request(
            vec![
                json!({"role": "system", "content": "first"}),
                json!({"role": "system", "content": "second"}),
            ],
            vec![],
        );
        assert_eq!(build_request(&req)["instructions"], "first\n\nsecond");
    }

    #[test]
    fn a_system_message_with_content_blocks_is_flattened() {
        // The Anthropic cache path rewrites system content into blocks.
        let req = request(
            vec![json!({
                "role": "system",
                "content": [{"type": "text", "text": "cached prefix"}]
            })],
            vec![],
        );
        assert_eq!(build_request(&req)["instructions"], "cached prefix");
    }

    #[test]
    fn user_messages_become_input_text_items() {
        let req = request(vec![json!({"role": "user", "content": "hello"})], vec![]);
        let input = build_request(&req)["input"].clone();
        assert_eq!(
            input[0],
            json!({
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "hello"}]
            })
        );
    }

    #[test]
    fn assistant_tool_calls_become_separate_function_call_items() {
        let req = request(
            vec![json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "Read", "arguments": "{\"path\":\"a\"}"}
                }]
            })],
            vec![],
        );
        let input = build_request(&req)["input"].clone();
        assert_eq!(
            input[0],
            json!({
                "type": "function_call",
                "call_id": "call_1",
                "name": "Read",
                "arguments": "{\"path\":\"a\"}"
            })
        );
    }

    #[test]
    fn assistant_text_and_tool_calls_both_survive() {
        let req = request(
            vec![json!({
                "role": "assistant",
                "content": "let me look",
                "tool_calls": [{
                    "id": "call_1",
                    "function": {"name": "Read", "arguments": "{}"}
                }]
            })],
            vec![],
        );
        let input = build_request(&req)["input"].clone();
        assert_eq!(input.as_array().unwrap().len(), 2);
        assert_eq!(input[0]["type"], "message");
        assert_eq!(input[1]["type"], "function_call");
    }

    #[test]
    fn tool_results_become_function_call_output_keyed_by_call_id() {
        // `tool_call_id` on the way in, `call_id` on the way out — a
        // mismatch here orphans the result and the model loops.
        let req = request(
            vec![json!({
                "role": "tool",
                "tool_call_id": "call_1",
                "content": "file contents"
            })],
            vec![],
        );
        let input = build_request(&req)["input"].clone();
        assert_eq!(
            input[0],
            json!({
                "type": "function_call_output",
                "call_id": "call_1",
                "output": "file contents"
            })
        );
    }

    #[test]
    fn tool_schemas_are_flattened_out_of_the_function_wrapper() {
        let req = request(
            vec![],
            vec![json!({
                "type": "function",
                "function": {
                    "name": "Read",
                    "description": "read a file",
                    "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}
                }
            })],
        );
        let tools = build_request(&req)["tools"].clone();
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["name"], "Read");
        assert_eq!(tools[0]["description"], "read a file");
        assert_eq!(tools[0]["parameters"]["type"], "object");
        // Flat, not nested.
        assert!(tools[0].get("function").is_none());
    }

    #[test]
    fn a_tool_without_a_name_is_dropped_rather_than_sent_malformed() {
        let req = request(vec![], vec![json!({"function": {"description": "x"}})]);
        assert!(build_request(&req)["tools"].as_array().unwrap().is_empty());
    }

    #[test]
    fn request_always_streams_and_never_stores() {
        let req = request(vec![json!({"role": "user", "content": "x"})], vec![]);
        let body = build_request(&req);
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
        assert_eq!(body["model"], "gpt-5.1-codex");
    }

    #[test]
    fn a_request_with_no_system_message_omits_instructions() {
        let req = request(vec![json!({"role": "user", "content": "x"})], vec![]);
        assert!(build_request(&req).get("instructions").is_none());
    }

    // ---- Stream decoding -----------------------------------------

    fn apply(acc: &mut ResponsesAcc, event: Value) -> Vec<Emit> {
        acc.apply_event(&event).unwrap()
    }

    #[test]
    fn text_deltas_accumulate_and_emit() {
        let mut acc = ResponsesAcc::default();
        let first = apply(
            &mut acc,
            json!({"type": "response.output_text.delta", "delta": "Hel"}),
        );
        let second = apply(
            &mut acc,
            json!({"type": "response.output_text.delta", "delta": "lo"}),
        );

        assert_eq!(first, vec![Emit::Text("Hel".into())]);
        assert_eq!(second, vec![Emit::Text("lo".into())]);
        assert_eq!(acc.finish().message["content"], "Hello");
    }

    #[test]
    fn empty_deltas_emit_nothing() {
        let mut acc = ResponsesAcc::default();
        assert!(
            apply(
                &mut acc,
                json!({"type": "response.output_text.delta", "delta": ""})
            )
            .is_empty()
        );
    }

    #[test]
    fn function_calls_are_assembled_from_item_and_argument_deltas() {
        let mut acc = ResponsesAcc::default();
        apply(
            &mut acc,
            json!({
                "type": "response.output_item.added",
                "item_id": "item_1",
                "item": {"type": "function_call", "call_id": "call_a", "name": "Read"}
            }),
        );
        apply(
            &mut acc,
            json!({
                "type": "response.function_call_arguments.delta",
                "item_id": "item_1",
                "delta": "{\"path\":"
            }),
        );
        apply(
            &mut acc,
            json!({
                "type": "response.function_call_arguments.delta",
                "item_id": "item_1",
                "delta": "\"a.rs\"}"
            }),
        );

        let message = acc.finish().message;
        assert_eq!(message["tool_calls"][0]["id"], "call_a");
        assert_eq!(message["tool_calls"][0]["function"]["name"], "Read");
        assert_eq!(
            message["tool_calls"][0]["function"]["arguments"],
            "{\"path\":\"a.rs\"}"
        );
        // No text content on a pure tool-call turn.
        assert_eq!(message["content"], Value::Null);
    }

    #[test]
    fn argument_deltas_emit_with_the_calls_index() {
        let mut acc = ResponsesAcc::default();
        apply(
            &mut acc,
            json!({
                "type": "response.output_item.added",
                "item_id": "i1",
                "item": {"type": "function_call", "call_id": "c1", "name": "Read"}
            }),
        );
        let emits = apply(
            &mut acc,
            json!({
                "type": "response.function_call_arguments.delta",
                "item_id": "i1",
                "delta": "{}"
            }),
        );
        assert_eq!(
            emits,
            vec![Emit::ToolArgs {
                index: 0,
                partial: "{}".into()
            }]
        );
        assert_eq!(acc.call_id_at(0), "c1");
        assert_eq!(acc.call_name_at(0), "Read");
    }

    #[test]
    fn the_done_item_is_authoritative_over_deltas() {
        // A dropped delta must not produce truncated arguments.
        let mut acc = ResponsesAcc::default();
        apply(
            &mut acc,
            json!({
                "type": "response.output_item.added",
                "item_id": "i1",
                "item": {"type": "function_call", "call_id": "c1", "name": "Read"}
            }),
        );
        apply(
            &mut acc,
            json!({
                "type": "response.function_call_arguments.delta",
                "item_id": "i1",
                "delta": "{\"pa"
            }),
        );
        apply(
            &mut acc,
            json!({
                "type": "response.output_item.done",
                "item_id": "i1",
                "item": {
                    "type": "function_call", "call_id": "c1", "name": "Read",
                    "arguments": "{\"path\":\"full.rs\"}"
                }
            }),
        );

        assert_eq!(
            acc.finish().message["tool_calls"][0]["function"]["arguments"],
            "{\"path\":\"full.rs\"}"
        );
    }

    #[test]
    fn parallel_tool_calls_stay_separate() {
        let mut acc = ResponsesAcc::default();
        for (item, call, name) in [("i1", "c1", "Read"), ("i2", "c2", "Grep")] {
            apply(
                &mut acc,
                json!({
                    "type": "response.output_item.added",
                    "item_id": item,
                    "item": {"type": "function_call", "call_id": call, "name": name}
                }),
            );
        }
        apply(
            &mut acc,
            json!({"type": "response.function_call_arguments.delta", "item_id": "i2", "delta": "{\"q\":1}"}),
        );
        apply(
            &mut acc,
            json!({"type": "response.function_call_arguments.delta", "item_id": "i1", "delta": "{\"p\":2}"}),
        );

        let calls = acc.finish().message["tool_calls"].clone();
        assert_eq!(calls.as_array().unwrap().len(), 2);
        assert_eq!(calls[0]["id"], "c1");
        assert_eq!(calls[0]["function"]["arguments"], "{\"p\":2}");
        assert_eq!(calls[1]["id"], "c2");
        assert_eq!(calls[1]["function"]["arguments"], "{\"q\":1}");
    }

    #[test]
    fn usage_is_mapped_from_the_responses_field_names() {
        let mut acc = ResponsesAcc::default();
        apply(
            &mut acc,
            json!({
                "type": "response.completed",
                "response": {"usage": {"input_tokens": 10, "output_tokens": 4, "total_tokens": 14}}
            }),
        );
        assert_eq!(
            acc.finish().usage,
            Some(Usage {
                prompt_tokens: Some(10),
                completion_tokens: Some(4),
                total_tokens: Some(14),
                ..Usage::default()
            })
        );
    }

    #[test]
    fn responses_token_details_populate_cache_read_and_reasoning() {
        let mut acc = ResponsesAcc::default();
        apply(
            &mut acc,
            json!({
                "type": "response.completed",
                "response": {"usage": {
                    "input_tokens": 100,
                    "input_tokens_details": {"cached_tokens": 80},
                    "output_tokens": 40,
                    "output_tokens_details": {"reasoning_tokens": 32},
                    "total_tokens": 140
                }}
            }),
        );
        let u = acc.finish().usage.unwrap();
        assert_eq!(u.cache_read_tokens, Some(80));
        assert_eq!(u.reasoning_tokens, Some(32));
        assert_eq!(u.cache_write_tokens, None);
    }

    #[test]
    fn absent_responses_token_details_stay_unknown_rather_than_zero() {
        let u = usage_from_responses(&json!({"input_tokens": 3, "output_tokens": 5})).unwrap();
        assert_eq!(u.cache_read_tokens, None);
        assert_eq!(u.reasoning_tokens, None);
    }

    #[test]
    fn a_usage_object_with_no_counts_is_no_usage() {
        assert_eq!(usage_from_responses(&json!({})), None);
    }

    #[test]
    fn usage_total_is_derived_when_absent() {
        let mut acc = ResponsesAcc::default();
        apply(
            &mut acc,
            json!({
                "type": "response.completed",
                "response": {"usage": {"input_tokens": 3, "output_tokens": 5}}
            }),
        );
        assert_eq!(acc.finish().usage.unwrap().total_tokens, Some(8));
    }

    #[test]
    fn completed_harvests_output_the_deltas_missed() {
        // If every delta were dropped, an empty turn would be worse
        // than a late one.
        let mut acc = ResponsesAcc::default();
        apply(
            &mut acc,
            json!({
                "type": "response.completed",
                "response": {
                    "output": [
                        {"type": "message", "content": [{"type": "output_text", "text": "hi"}]},
                        {"type": "function_call", "id": "i9", "call_id": "c9",
                         "name": "Read", "arguments": "{\"path\":\"x\"}"}
                    ]
                }
            }),
        );
        let message = acc.finish().message;
        assert_eq!(message["content"], "hi");
        assert_eq!(message["tool_calls"][0]["id"], "c9");
    }

    #[test]
    fn harvest_does_not_duplicate_calls_already_streamed() {
        let mut acc = ResponsesAcc::default();
        apply(
            &mut acc,
            json!({
                "type": "response.output_item.added",
                "item_id": "i1",
                "item": {"type": "function_call", "call_id": "c1", "name": "Read"}
            }),
        );
        apply(
            &mut acc,
            json!({"type": "response.function_call_arguments.delta", "item_id": "i1", "delta": "{}"}),
        );
        apply(
            &mut acc,
            json!({
                "type": "response.completed",
                "response": {"output": [
                    {"type": "function_call", "id": "i1", "call_id": "c1",
                     "name": "Read", "arguments": "{}"}
                ]}
            }),
        );
        assert_eq!(
            acc.finish().message["tool_calls"].as_array().unwrap().len(),
            1
        );
    }

    #[test]
    fn a_failed_response_becomes_an_error_naming_the_fallback() {
        let mut acc = ResponsesAcc::default();
        let err = acc
            .apply_event(&json!({
                "type": "response.failed",
                "response": {"error": {"message": "model overloaded"}}
            }))
            .unwrap_err()
            .to_string();
        assert!(err.contains("model overloaded"), "{err}");
        assert!(err.contains("API-key auth"), "{err}");
    }

    #[test]
    fn a_bare_error_event_becomes_an_error() {
        let mut acc = ResponsesAcc::default();
        assert!(
            acc.apply_event(&json!({"type": "error", "error": {"message": "bad request"}}))
                .is_err()
        );
    }

    #[test]
    fn unknown_event_types_are_ignored() {
        // The Responses event vocabulary grows; unknown types must not
        // break a turn.
        let mut acc = ResponsesAcc::default();
        assert!(
            apply(
                &mut acc,
                json!({"type": "response.reasoning.delta", "delta": "…"})
            )
            .is_empty()
        );
        assert!(apply(&mut acc, json!({"type": "response.in_progress"})).is_empty());
    }

    #[test]
    fn a_call_that_never_got_a_name_is_dropped() {
        // Sending a nameless tool_call downstream would panic the
        // dispatcher; an empty turn is the safer failure.
        let mut acc = ResponsesAcc::default();
        apply(
            &mut acc,
            json!({"type": "response.function_call_arguments.delta", "item_id": "i1", "delta": "{}"}),
        );
        assert!(acc.finish().message.get("tool_calls").is_none());
    }

    // ---- Error rendering -----------------------------------------

    #[test]
    fn a_401_tells_the_user_to_log_in_and_names_the_fallback() {
        let msg = describe_endpoint_error(
            401,
            r#"{"error":{"message":"invalid token"}}"#,
            std::path::Path::new("/home/u/.config/oli/auth.json"),
        );
        assert!(msg.contains("invalid token"), "{msg}");
        assert!(msg.contains("oli login"), "{msg}");
        assert!(msg.contains("OPENAI_API_KEY"), "{msg}");
        assert!(msg.contains("auth.json"), "{msg}");
    }

    #[test]
    fn a_403_says_third_party_access_may_have_ended() {
        let msg = describe_endpoint_error(403, "{}", std::path::Path::new("/x"));
        assert!(msg.contains("third-party"), "{msg}");
    }

    #[test]
    fn a_404_says_the_endpoint_may_be_gone() {
        let msg = describe_endpoint_error(404, "", std::path::Path::new("/x"));
        assert!(msg.contains("withdrawn"), "{msg}");
        assert!(msg.contains("no response body"), "{msg}");
    }

    #[test]
    fn a_429_is_named_as_a_rate_limit() {
        let msg = describe_endpoint_error(429, "{}", std::path::Path::new("/x"));
        assert!(msg.contains("rate limit"), "{msg}");
    }

    #[test]
    fn error_extraction_handles_several_body_shapes() {
        assert_eq!(extract_error_message(r#"{"error":{"message":"a"}}"#), "a");
        assert_eq!(extract_error_message(r#"{"message":"b"}"#), "b");
        assert_eq!(extract_error_message(r#"{"detail":"c"}"#), "c");
        assert_eq!(extract_error_message("<html>d</html>"), "<html>d</html>");
    }

    #[test]
    fn error_extraction_caps_long_bodies() {
        assert_eq!(extract_error_message(&"x".repeat(9_000)).len(), 400);
    }

    // ---- Model discovery -----------------------------------------

    /// Trimmed from a real `/models` response. The slugs are the point:
    /// the subscription backend serves neither the public API's names
    /// nor any `gpt-5.x-codex` variant.
    const MODELS_FIXTURE: &str = r#"{
      "models": [
        {"slug": "gpt-5.6-terra", "display_name": "GPT-5.6-Terra",
         "description": "Balanced agentic coding model for everyday work.",
         "context_window": 272000, "use_responses_lite": true},
        {"slug": "gpt-5.6-luna", "display_name": "GPT-5.6-Luna", "context_window": 272000},
        {"slug": "gpt-5.5", "display_name": "GPT-5.5", "context_window": 272000},
        {"slug": "gpt-5.4-mini", "display_name": "GPT-5.4-Mini", "context_window": 272000},
        {"slug": "codex-auto-review", "display_name": "Codex Auto Review"}
      ]
    }"#;

    #[test]
    fn parses_the_model_catalogue() {
        let models = parse_models(MODELS_FIXTURE).unwrap();
        assert_eq!(models.len(), 5);
        assert_eq!(models[0].slug, "gpt-5.6-terra");
        assert_eq!(models[0].display_name.as_deref(), Some("GPT-5.6-Terra"));
        assert_eq!(models[0].context_window, Some(272_000));
    }

    #[test]
    fn unknown_catalogue_fields_are_ignored() {
        // The catalogue gains fields regularly; that must not break
        // model discovery.
        let models =
            parse_models(r#"{"models":[{"slug":"m1","some_future_field":{"nested":true}}]}"#)
                .unwrap();
        assert_eq!(models[0].slug, "m1");
    }

    #[test]
    fn entries_without_a_slug_are_skipped() {
        let models =
            parse_models(r#"{"models":[{"display_name":"nameless"},{"slug":"m1"}]}"#).unwrap();
        assert_eq!(models.len(), 1);
    }

    #[test]
    fn a_response_without_a_models_array_is_an_error() {
        let err = parse_models(r#"{"error":{"message":"nope"}}"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("`models` array"), "{err}");
    }

    #[test]
    fn preferred_model_skips_the_review_only_model() {
        let models = parse_models(MODELS_FIXTURE).unwrap();
        assert_eq!(preferred_model(&models).unwrap().slug, "gpt-5.6-terra");

        // Even when it is the only thing listed, something is better
        // than nothing.
        let only_review = parse_models(r#"{"models":[{"slug":"codex-auto-review"}]}"#).unwrap();
        assert_eq!(
            preferred_model(&only_review).unwrap().slug,
            "codex-auto-review"
        );
    }

    #[test]
    fn preferred_model_of_an_empty_catalogue_is_none() {
        assert!(preferred_model(&[]).is_none());
    }

    #[test]
    fn session_id_is_uuid_shaped_and_unique() {
        let a = random_session_id();
        assert_eq!(a.len(), 36);
        assert_eq!(a.split('-').count(), 5);
        assert_ne!(a, random_session_id());
    }

    #[test]
    fn models_client_version_is_above_the_observed_gate() {
        // SAFETY: single-purpose env var, removed straight after.
        unsafe { std::env::remove_var(CLIENT_VERSION_ENV) };
        // Observed: 0.1.0 / 0.9.0 / 0.20.0 / 0.45.0 return nothing,
        // 0.104.0 returns only the two oldest models, >=1.0.0 returns
        // the full catalogue. Sending oli's own version would return
        // an empty list, which is why this is a separate constant.
        assert_ne!(MODELS_CLIENT_VERSION, env!("CARGO_PKG_VERSION"));
        let major: u32 = MODELS_CLIENT_VERSION
            .split('.')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!(major >= 1, "{MODELS_CLIENT_VERSION} is below the gate");
    }

    #[test]
    fn client_version_can_be_overridden() {
        // SAFETY: as above.
        unsafe { std::env::set_var(CLIENT_VERSION_ENV, "2.5.0") };
        assert_eq!(models_client_version(), "2.5.0");
        unsafe { std::env::remove_var(CLIENT_VERSION_ENV) };
        assert_eq!(models_client_version(), MODELS_CLIENT_VERSION);
    }

    #[test]
    fn originator_defaults_and_can_be_overridden() {
        // SAFETY: single-purpose env var, removed straight after.
        unsafe { std::env::remove_var(ORIGINATOR_ENV) };
        assert_eq!(originator(), ORIGINATOR);
        unsafe { std::env::set_var(ORIGINATOR_ENV, "oli") };
        assert_eq!(originator(), "oli");
        unsafe { std::env::remove_var(ORIGINATOR_ENV) };
    }
}
