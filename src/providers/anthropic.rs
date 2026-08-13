//! Native Anthropic Messages API provider.
//!
//! The OpenAI-compat path already reaches Anthropic models via
//! OpenRouter, so this provider exists for one reason: **prompt
//! caching**. On long agent sessions, caching the system prompt + tool
//! definitions cuts repeat-input tokens by >90% on cache hits, which is
//! the difference between a usable and an expensive workflow.
//!
//! The Anthropic Messages API has a different shape from OpenAI's:
//! - `system` is a separate top-level field, not a message.
//! - Assistant tool calls are `content` blocks of type `tool_use`.
//! - Tool results come back as `user` messages whose content is a
//!   `tool_result` block.
//! - Tools have `input_schema` rather than `parameters`.
//!
//! This module owns the bidirectional conversion. The agent loop
//! continues to operate in OpenAI shape; the conversion is invisible
//! upstream.

use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde_json::{Value, json};

use crate::error::{AgentError, Result};
use crate::providers::{
    ChatRequest, ChatResponse, Provider, StreamEvent, StreamSink, Usage, derived_total, token_count,
};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 4096;

pub struct AnthropicProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl AnthropicProvider {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        let base = base_url.into();
        let base_url = if base.is_empty() {
            DEFAULT_BASE_URL.to_string()
        } else {
            base.trim_end_matches('/').to_string()
        };
        Self {
            client: reqwest::Client::new(),
            base_url,
            api_key: api_key.into(),
        }
    }

    fn endpoint(&self) -> String {
        format!("{}/v1/messages", self.base_url)
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
        let body = build_request_body(&req, false);
        let resp = self
            .client
            .post(self.endpoint())
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| AgentError::Provider(format!("anthropic request: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(AgentError::Provider(format!(
                "anthropic {}: {}",
                status, text
            )));
        }

        let json: Value = resp
            .json()
            .await
            .map_err(|e| AgentError::Provider(format!("anthropic parse: {}", e)))?;
        let (message, usage) = anthropic_to_openai_response(&json);
        Ok(ChatResponse { message, usage })
    }

    async fn list_models(&self) -> Result<Vec<String>> {
        let url = format!("{}/v1/models", self.base_url);
        let resp = self
            .client
            .get(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .send()
            .await
            .map_err(|e| AgentError::Provider(format!("anthropic models request: {}", e)))?;

        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| AgentError::Provider(format!("anthropic models read body: {}", e)))?;

        if !status.is_success() {
            // The Messages API surfaces errors under `{error: {message, type}}`.
            // Render whichever message is most useful, falling back to status
            // alone for empty bodies.
            let text = String::from_utf8_lossy(&bytes);
            if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
                if let Some(msg) = v
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                {
                    return Err(AgentError::Provider(format!(
                        "anthropic models {}: {}",
                        status, msg
                    )));
                }
            }
            return Err(AgentError::Provider(format!(
                "anthropic models {}: {}",
                status, text
            )));
        }

        parse_models_response(&bytes)
    }

    async fn chat_stream(&self, req: ChatRequest, sink: StreamSink<'_>) -> Result<ChatResponse> {
        let body = build_request_body(&req, true);
        let resp = self
            .client
            .post(self.endpoint())
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| AgentError::Provider(format!("anthropic stream request: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(AgentError::Provider(format!(
                "anthropic {}: {}",
                status, text
            )));
        }

        let mut stream = resp.bytes_stream().eventsource();
        let mut acc = StreamAcc::default();

        while let Some(event) = stream.next().await {
            let event = event.map_err(|e| AgentError::Provider(format!("anthropic SSE: {}", e)))?;
            let parsed: Value = match serde_json::from_str(&event.data) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let event_type = parsed.get("type").and_then(|v| v.as_str()).unwrap_or("");
            handle_stream_event(event_type, &parsed, &mut acc, sink);
        }

        let (message, usage) = acc.finalize();
        Ok(ChatResponse { message, usage })
    }
}

#[derive(Default)]
struct StreamAcc {
    text: String,
    tool_calls: Vec<ToolUseAcc>,
    /// `(id, name)` of each tool_use block currently being streamed,
    /// keyed by content block index.
    tool_meta: std::collections::HashMap<usize, (String, String)>,
    /// Accumulating `arguments` JSON string, keyed by content block index.
    tool_args: std::collections::HashMap<usize, String>,
    usage: Option<Usage>,
}

#[derive(Default)]
struct ToolUseAcc {
    id: String,
    name: String,
    arguments: String,
}

impl StreamAcc {
    fn finalize(self) -> (Value, Option<Usage>) {
        let mut message = json!({"role": "assistant"});
        if self.text.is_empty() {
            message["content"] = Value::Null;
        } else {
            message["content"] = Value::String(self.text);
        }
        if !self.tool_calls.is_empty() {
            let arr: Vec<Value> = self
                .tool_calls
                .into_iter()
                .map(|t| {
                    json!({
                        "id": t.id,
                        "type": "function",
                        "function": { "name": t.name, "arguments": t.arguments }
                    })
                })
                .collect();
            message["tool_calls"] = Value::Array(arr);
        }
        (message, self.usage)
    }
}

fn handle_stream_event(
    event_type: &str,
    payload: &Value,
    acc: &mut StreamAcc,
    sink: StreamSink<'_>,
) {
    match event_type {
        "message_start" => {
            if let Some(u) = payload
                .get("message")
                .and_then(|m| m.get("usage"))
                .and_then(anthropic_usage_from_value)
            {
                acc.usage = Some(u);
            }
        }
        "content_block_start" => {
            let idx = payload.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let block = match payload.get("content_block") {
                Some(b) => b,
                None => return,
            };
            let kind = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if kind == "tool_use" {
                let id = block
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = block
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                acc.tool_meta.insert(idx, (id, name));
                acc.tool_args.insert(idx, String::new());
            }
        }
        "content_block_delta" => {
            let delta = match payload.get("delta") {
                Some(d) => d,
                None => return,
            };
            let dtype = delta.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match dtype {
                "text_delta" => {
                    if let Some(t) = delta.get("text").and_then(|v| v.as_str()) {
                        if !t.is_empty() {
                            acc.text.push_str(t);
                            sink(StreamEvent::Content(t));
                        }
                    }
                }
                "input_json_delta" => {
                    let idx = payload.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    if let Some(part) = delta.get("partial_json").and_then(|v| v.as_str()) {
                        let entry = acc.tool_args.entry(idx).or_default();
                        entry.push_str(part);
                        // Only surface chunks for blocks we already saw a
                        // `content_block_start` for — guards against a
                        // malformed stream that delivers a delta before its
                        // header. `(id, name)` are stable across the block.
                        if let Some((id, name)) = acc.tool_meta.get(&idx) {
                            sink(StreamEvent::ToolArgsChunk {
                                provider_tool_id: id,
                                name,
                                partial_json: part,
                                accumulated_json: entry.as_str(),
                            });
                        }
                    }
                }
                _ => {}
            }
        }
        "content_block_stop" => {
            let idx = payload.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            if let Some((id, name)) = acc.tool_meta.remove(&idx) {
                let arguments = acc.tool_args.remove(&idx).unwrap_or_default();
                acc.tool_calls.push(ToolUseAcc {
                    id,
                    name,
                    arguments,
                });
            }
        }
        "message_delta" => {
            if let Some(u) = payload.get("usage").and_then(anthropic_usage_from_value) {
                merge_usage(&mut acc.usage, u);
            }
        }
        _ => {}
    }
}

/// Parse the body of a `GET /v1/models` response. Anthropic returns
/// `{"data": [{"id": ..., ...}, ...], "has_more": ..., ...}`; we
/// only care about the `id`s. Extracted from `list_models` so it
/// can be unit-tested without a live HTTP roundtrip.
fn parse_models_response(bytes: &[u8]) -> Result<Vec<String>> {
    let v: Value = serde_json::from_slice(bytes)
        .map_err(|e| AgentError::Provider(format!("anthropic models parse: {}", e)))?;
    let data = v
        .get("data")
        .and_then(|x| x.as_array())
        .ok_or_else(|| AgentError::Provider("models response missing `data` array".into()))?;
    Ok(data
        .iter()
        .filter_map(|m| m.get("id").and_then(|v| v.as_str()).map(String::from))
        .collect())
}

fn anthropic_usage_from_value(v: &Value) -> Option<Usage> {
    let input = token_count(v, "input_tokens");
    let output = token_count(v, "output_tokens");
    let usage = Usage {
        prompt_tokens: input,
        completion_tokens: output,
        total_tokens: derived_total(input, output),
        cache_read_tokens: token_count(v, "cache_read_input_tokens"),
        cache_write_tokens: token_count(v, "cache_creation_input_tokens"),
        // Extended thinking is billed inside output_tokens and never
        // broken out.
        reasoning_tokens: None,
    };
    usage.reports_anything().then_some(usage)
}

fn merge_usage(slot: &mut Option<Usage>, fresh: Usage) {
    match slot {
        Some(u) => {
            // message_delta carries the running output count; the input
            // and cache counts only ever arrive on message_start.
            if fresh.completion_tokens.is_some() {
                u.completion_tokens = fresh.completion_tokens;
            }
            fill_if_unknown(&mut u.prompt_tokens, fresh.prompt_tokens);
            fill_if_unknown(&mut u.cache_read_tokens, fresh.cache_read_tokens);
            fill_if_unknown(&mut u.cache_write_tokens, fresh.cache_write_tokens);
            u.total_tokens = derived_total(u.prompt_tokens, u.completion_tokens);
        }
        None => *slot = Some(fresh),
    }
}

fn fill_if_unknown(slot: &mut Option<u32>, fresh: Option<u32>) {
    if slot.is_none() {
        *slot = fresh;
    }
}

/// Convert an OpenAI-shaped `ChatRequest` into the JSON body Anthropic
/// expects. Includes prompt-cache breakpoints on the system prompt and
/// the last tool definition — these are the high-leverage points (the
/// system prompt is the largest stable prefix; tools change rarely).
pub(crate) fn build_request_body(req: &ChatRequest, stream: bool) -> Value {
    let (system, messages) = split_messages(&req.messages);

    let mut body = json!({
        "model": req.model,
        "max_tokens": DEFAULT_MAX_TOKENS,
        "messages": messages,
        "stream": stream,
    });

    if !system.is_empty() {
        body["system"] = json!([{
            "type": "text",
            "text": system,
            "cache_control": { "type": "ephemeral" }
        }]);
    }

    if !req.tools.is_empty() {
        let mut tools = convert_tools(&req.tools);
        // Mark the last tool as a cache breakpoint so the whole tools
        // array is cached. If a future user adds/removes a tool,
        // caching invalidates — that's the intended trade-off.
        if let Some(last) = tools.last_mut() {
            if let Some(obj) = last.as_object_mut() {
                obj.insert("cache_control".to_string(), json!({"type": "ephemeral"}));
            }
        }
        body["tools"] = Value::Array(tools);
    }

    body
}

fn split_messages(msgs: &[Value]) -> (String, Vec<Value>) {
    let mut system = String::new();
    let mut out = Vec::new();
    for m in msgs {
        let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("");
        match role {
            "system" => {
                if let Some(c) = m.get("content").and_then(|v| v.as_str()) {
                    if !system.is_empty() {
                        system.push_str("\n\n");
                    }
                    system.push_str(c);
                }
            }
            "tool" => {
                let id = m.get("tool_call_id").and_then(|v| v.as_str()).unwrap_or("");
                let content = m.get("content").and_then(|v| v.as_str()).unwrap_or("");
                out.push(json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": id,
                        "content": content,
                    }]
                }));
            }
            "assistant" => {
                let mut blocks: Vec<Value> = Vec::new();
                if let Some(c) = m.get("content").and_then(|v| v.as_str()) {
                    if !c.is_empty() {
                        blocks.push(json!({"type":"text","text":c}));
                    }
                }
                if let Some(calls) = m.get("tool_calls").and_then(|v| v.as_array()) {
                    for call in calls {
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
                        let input: Value =
                            serde_json::from_str(args_str).unwrap_or_else(|_| json!({}));
                        blocks.push(json!({
                            "type": "tool_use",
                            "id": id,
                            "name": name,
                            "input": input,
                        }));
                    }
                }
                if blocks.is_empty() {
                    blocks.push(json!({"type":"text","text":""}));
                }
                out.push(json!({"role":"assistant","content":blocks}));
            }
            "user" => {
                if let Some(c) = m.get("content").and_then(|v| v.as_str()) {
                    out.push(json!({"role":"user","content":c}));
                } else {
                    out.push(json!({"role":"user","content":""}));
                }
            }
            _ => {}
        }
    }
    (system, out)
}

fn convert_tools(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .filter_map(|t| {
            let f = t.get("function")?;
            let name = f.get("name")?.as_str()?.to_string();
            let description = f
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let input_schema = f
                .get("parameters")
                .cloned()
                .unwrap_or_else(|| json!({"type":"object","properties":{}}));
            Some(json!({
                "name": name,
                "description": description,
                "input_schema": input_schema,
            }))
        })
        .collect()
}

/// Convert a non-streaming Anthropic response back into the OpenAI
/// shape the agent expects.
pub(crate) fn anthropic_to_openai_response(resp: &Value) -> (Value, Option<Usage>) {
    let mut text = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();

    if let Some(blocks) = resp.get("content").and_then(|v| v.as_array()) {
        for block in blocks {
            match block.get("type").and_then(|v| v.as_str()) {
                Some("text") => {
                    if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                        text.push_str(t);
                    }
                }
                Some("tool_use") => {
                    let id = block
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = block
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
                    tool_calls.push(json!({
                        "id": id,
                        "type": "function",
                        "function": { "name": name, "arguments": input.to_string() }
                    }));
                }
                _ => {}
            }
        }
    }

    let mut message = json!({"role":"assistant"});
    if text.is_empty() {
        message["content"] = Value::Null;
    } else {
        message["content"] = Value::String(text);
    }
    if !tool_calls.is_empty() {
        message["tool_calls"] = Value::Array(tool_calls);
    }

    let usage = resp.get("usage").and_then(anthropic_usage_from_value);
    (message, usage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_messages_extracts_system_and_keeps_user_assistant() {
        let msgs = vec![
            json!({"role":"system","content":"sys A"}),
            json!({"role":"user","content":"hi"}),
            json!({"role":"assistant","content":"hello"}),
        ];
        let (sys, out) = split_messages(&msgs);
        assert_eq!(sys, "sys A");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["role"], "user");
        assert_eq!(out[1]["role"], "assistant");
    }

    #[test]
    fn split_messages_concatenates_multiple_system_messages() {
        let msgs = vec![
            json!({"role":"system","content":"first"}),
            json!({"role":"system","content":"second"}),
            json!({"role":"user","content":"go"}),
        ];
        let (sys, _) = split_messages(&msgs);
        assert!(sys.contains("first"));
        assert!(sys.contains("second"));
    }

    #[test]
    fn assistant_with_tool_calls_becomes_blocks_array() {
        let msgs = vec![json!({
            "role":"assistant",
            "content": null,
            "tool_calls": [{
                "id": "c1",
                "type": "function",
                "function": { "name": "Read", "arguments": "{\"file_path\":\"x\"}" }
            }]
        })];
        let (_, out) = split_messages(&msgs);
        let blocks = out[0]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "tool_use");
        assert_eq!(blocks[0]["id"], "c1");
        assert_eq!(blocks[0]["name"], "Read");
        assert_eq!(blocks[0]["input"]["file_path"], "x");
    }

    #[test]
    fn tool_role_message_becomes_user_with_tool_result_block() {
        let msgs = vec![json!({
            "role":"tool",
            "tool_call_id":"c1",
            "content":"file contents"
        })];
        let (_, out) = split_messages(&msgs);
        assert_eq!(out[0]["role"], "user");
        let blocks = out[0]["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "tool_result");
        assert_eq!(blocks[0]["tool_use_id"], "c1");
        assert_eq!(blocks[0]["content"], "file contents");
    }

    #[test]
    fn convert_tools_renames_parameters_to_input_schema() {
        let tools = vec![json!({
            "type": "function",
            "function": {
                "name": "Read",
                "description": "read a file",
                "parameters": { "type": "object", "properties": {} }
            }
        })];
        let out = convert_tools(&tools);
        assert_eq!(out[0]["name"], "Read");
        assert_eq!(out[0]["description"], "read a file");
        assert_eq!(out[0]["input_schema"]["type"], "object");
    }

    #[test]
    fn build_request_body_adds_cache_control_on_system_and_last_tool() {
        let req = ChatRequest {
            model: "claude-haiku-4-5".into(),
            messages: vec![
                json!({"role":"system","content":"sys"}),
                json!({"role":"user","content":"go"}),
            ],
            tools: vec![
                json!({"type":"function","function":{"name":"A","description":"","parameters":{}}}),
                json!({"type":"function","function":{"name":"B","description":"","parameters":{}}}),
            ],
        };
        let body = build_request_body(&req, false);
        let sys = body["system"].as_array().unwrap();
        assert_eq!(sys[0]["cache_control"]["type"], "ephemeral");
        let tools = body["tools"].as_array().unwrap();
        // Only the LAST tool gets cache_control as a breakpoint.
        assert!(tools[0].get("cache_control").is_none());
        assert_eq!(tools[1]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn build_request_body_omits_tools_field_when_none() {
        let req = ChatRequest {
            model: "x".into(),
            messages: vec![json!({"role":"user","content":"hi"})],
            tools: vec![],
        };
        let body = build_request_body(&req, false);
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn anthropic_to_openai_response_extracts_text_and_tool_calls() {
        let resp = json!({
            "content": [
                {"type": "text", "text": "Sure, "},
                {"type": "text", "text": "let me do that."},
                {"type": "tool_use", "id":"u1", "name":"Read", "input":{"file_path":"x"}}
            ],
            "usage": {"input_tokens": 10, "output_tokens": 4}
        });
        let (msg, usage) = anthropic_to_openai_response(&resp);
        assert_eq!(msg["role"], "assistant");
        assert_eq!(msg["content"], "Sure, let me do that.");
        let calls = msg["tool_calls"].as_array().unwrap();
        assert_eq!(calls[0]["id"], "u1");
        assert_eq!(calls[0]["function"]["name"], "Read");
        let args: Value =
            serde_json::from_str(calls[0]["function"]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args["file_path"], "x");
        let u = usage.unwrap();
        assert_eq!(u.prompt_tokens, Some(10));
        assert_eq!(u.completion_tokens, Some(4));
        assert_eq!(u.total_tokens, Some(14));
    }

    #[test]
    fn cache_read_and_write_counts_survive_the_response_conversion() {
        let resp = json!({
            "content": [{"type":"text","text":"hi"}],
            "usage": {
                "input_tokens": 12,
                "output_tokens": 4,
                "cache_read_input_tokens": 2048,
                "cache_creation_input_tokens": 512
            }
        });
        let (_, usage) = anthropic_to_openai_response(&resp);
        let u = usage.unwrap();
        assert_eq!(u.cache_read_tokens, Some(2048));
        assert_eq!(u.cache_write_tokens, Some(512));
    }

    #[test]
    fn absent_cache_fields_stay_unknown_rather_than_zero() {
        let usage =
            anthropic_usage_from_value(&json!({"input_tokens": 12, "output_tokens": 4})).unwrap();
        assert_eq!(usage.cache_read_tokens, None);
        assert_eq!(usage.cache_write_tokens, None);
        // Extended thinking is billed inside output_tokens.
        assert_eq!(usage.reasoning_tokens, None);
    }

    #[test]
    fn a_response_with_no_usage_block_reports_no_usage() {
        let (_, usage) = anthropic_to_openai_response(&json!({
            "content": [{"type":"text","text":"hi"}]
        }));
        assert_eq!(usage, None);
        assert_eq!(anthropic_usage_from_value(&json!({})), None);
    }

    #[test]
    fn a_cache_only_usage_block_is_still_usage() {
        // A fully-cached prompt can report zero fresh input tokens; the
        // cache counts are the whole story and must not be dropped.
        let usage = anthropic_usage_from_value(&json!({
            "input_tokens": 0,
            "output_tokens": 0,
            "cache_read_input_tokens": 4096
        }))
        .unwrap();
        assert_eq!(usage.prompt_tokens, Some(0));
        assert_eq!(usage.cache_read_tokens, Some(4096));
    }

    #[test]
    fn streaming_usage_merges_message_start_input_with_message_delta_output() {
        let mut acc = StreamAcc::default();
        let mut sink = |_: StreamEvent<'_>| {};
        handle_stream_event(
            "message_start",
            &json!({"message": {"usage": {
                "input_tokens": 100,
                "output_tokens": 1,
                "cache_read_input_tokens": 900,
                "cache_creation_input_tokens": 40
            }}}),
            &mut acc,
            &mut sink,
        );
        handle_stream_event(
            "message_delta",
            &json!({"usage": {"output_tokens": 25}}),
            &mut acc,
            &mut sink,
        );

        let (_, usage) = acc.finalize();
        let u = usage.unwrap();
        assert_eq!(u.prompt_tokens, Some(100));
        assert_eq!(u.completion_tokens, Some(25));
        assert_eq!(u.total_tokens, Some(125));
        // The delta never repeats the cache counts; they must not be
        // lost on the way through.
        assert_eq!(u.cache_read_tokens, Some(900));
        assert_eq!(u.cache_write_tokens, Some(40));
    }

    #[test]
    fn streaming_usage_from_a_delta_alone_leaves_the_prompt_count_unknown() {
        let mut acc = StreamAcc::default();
        let mut sink = |_: StreamEvent<'_>| {};
        handle_stream_event(
            "message_delta",
            &json!({"usage": {"output_tokens": 7}}),
            &mut acc,
            &mut sink,
        );
        let (_, usage) = acc.finalize();
        let u = usage.unwrap();
        assert_eq!(u.prompt_tokens, None);
        assert_eq!(u.completion_tokens, Some(7));
        assert_eq!(u.total_tokens, None);
    }

    #[test]
    fn anthropic_to_openai_response_handles_text_only_content() {
        let resp = json!({
            "content": [{"type":"text","text":"all done"}],
            "usage": {"input_tokens": 5, "output_tokens": 2}
        });
        let (msg, _) = anthropic_to_openai_response(&resp);
        assert_eq!(msg["content"], "all done");
        assert!(msg.get("tool_calls").is_none());
    }

    #[test]
    fn parse_models_response_returns_id_list_in_order() {
        let body = br#"{
            "data": [
                {"id":"claude-opus-4-1","type":"model","display_name":"Opus 4.1"},
                {"id":"claude-sonnet-4-5","type":"model"},
                {"id":"claude-haiku-4-5","type":"model"}
            ],
            "has_more": false
        }"#;
        let ids = parse_models_response(body).unwrap();
        assert_eq!(
            ids,
            vec![
                "claude-opus-4-1".to_string(),
                "claude-sonnet-4-5".to_string(),
                "claude-haiku-4-5".to_string(),
            ]
        );
    }

    #[test]
    fn parse_models_response_skips_entries_without_id() {
        let body = br#"{"data":[{"id":"a"},{"name":"no-id"},{"id":"b"}]}"#;
        let ids = parse_models_response(body).unwrap();
        assert_eq!(ids, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn handle_stream_event_emits_tool_args_chunk_on_input_json_delta() {
        // Set up a tool_use content block, then deliver two
        // input_json_delta events. The sink should see one
        // ToolArgsChunk per delta with the accumulated_json growing,
        // and zero Content events (the call is silent on the text
        // side).
        let mut acc = StreamAcc::default();
        let mut events: Vec<(String, String, String, String)> = Vec::new();
        let mut content_events: Vec<String> = Vec::new();
        {
            let mut sink = |ev: StreamEvent<'_>| match ev {
                StreamEvent::ToolArgsChunk {
                    provider_tool_id,
                    name,
                    partial_json,
                    accumulated_json,
                } => events.push((
                    provider_tool_id.to_string(),
                    name.to_string(),
                    partial_json.to_string(),
                    accumulated_json.to_string(),
                )),
                StreamEvent::Content(s) => content_events.push(s.to_string()),
            };

            handle_stream_event(
                "content_block_start",
                &json!({
                    "index": 0,
                    "content_block": {
                        "type": "tool_use",
                        "id": "toolu_42",
                        "name": "Edit",
                    }
                }),
                &mut acc,
                &mut sink,
            );
            handle_stream_event(
                "content_block_delta",
                &json!({
                    "index": 0,
                    "delta": { "type": "input_json_delta", "partial_json": "{\"file_path" }
                }),
                &mut acc,
                &mut sink,
            );
            handle_stream_event(
                "content_block_delta",
                &json!({
                    "index": 0,
                    "delta": { "type": "input_json_delta", "partial_json": "\":\"x.rs\"}" }
                }),
                &mut acc,
                &mut sink,
            );
        }
        assert!(content_events.is_empty(), "no Content events expected");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].0, "toolu_42");
        assert_eq!(events[0].1, "Edit");
        assert_eq!(events[0].2, "{\"file_path");
        assert_eq!(events[0].3, "{\"file_path");
        assert_eq!(events[1].2, "\":\"x.rs\"}");
        assert_eq!(events[1].3, "{\"file_path\":\"x.rs\"}");
    }

    #[test]
    fn handle_stream_event_skips_args_chunk_without_block_start() {
        // Malformed stream: input_json_delta before content_block_start.
        // No tool_meta entry → no chunk event surfaced. Accumulator
        // still buffers the partial JSON for finalize-time safety.
        let mut acc = StreamAcc::default();
        let mut chunks = 0usize;
        {
            let mut sink = |ev: StreamEvent<'_>| {
                if let StreamEvent::ToolArgsChunk { .. } = ev {
                    chunks += 1;
                }
            };
            handle_stream_event(
                "content_block_delta",
                &json!({
                    "index": 0,
                    "delta": { "type": "input_json_delta", "partial_json": "{}" }
                }),
                &mut acc,
                &mut sink,
            );
        }
        assert_eq!(chunks, 0);
    }

    #[test]
    fn parse_models_response_errors_when_data_missing() {
        let body = br#"{"oops": []}"#;
        let err = parse_models_response(body).unwrap_err();
        assert!(err.to_string().contains("missing `data`"));
    }
}
