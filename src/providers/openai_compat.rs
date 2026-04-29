use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde_json::{Value, json};

use crate::error::{AgentError, Result};
use crate::providers::{ChatRequest, ChatResponse, ContentSink, Provider, Usage};

/// OpenAI-compatible provider. Works against OpenAI, OpenRouter, Ollama
/// (via `http://localhost:11434/v1`), LM Studio, vLLM, llama.cpp's server,
/// and anything else that speaks the OpenAI Chat Completions API.
///
/// Talks to the API via `reqwest` directly. We previously routed
/// through `async-openai`'s `_byot` (bring-your-own-type) helpers, but
/// its error path expects `error.code` to deserialize as a string,
/// while OpenRouter returns it as an integer (e.g. `"code": 429`)
/// when an upstream provider rate-limits the request. The mismatch
/// surfaced as `invalid type: integer 429, expected a string` and
/// hid the real "rate-limited upstream" message from the user. Going
/// directly via `reqwest` lets us preserve the original error body
/// verbatim and recognize the OpenRouter convention of returning
/// 200 OK with a top-level `error` object embedded in the body.
pub struct OpenAICompatProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl OpenAICompatProvider {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        let base = base_url.into();
        let base_url = base.trim_end_matches('/').to_string();
        Self {
            client: reqwest::Client::new(),
            base_url,
            api_key: api_key.into(),
        }
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}/{}", self.base_url, path.trim_start_matches('/'))
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

        let resp = self
            .client
            .post(self.endpoint("chat/completions"))
            .bearer_auth(&self.api_key)
            .header("content-type", "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|e| AgentError::Provider(format!("request: {}", e)))?;

        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| AgentError::Provider(format!("read body: {}", e)))?;

        if !status.is_success() {
            return Err(AgentError::Provider(error_message_from_body(
                status.as_u16(),
                &bytes,
            )));
        }

        let response: Value = serde_json::from_slice(&bytes).map_err(|e| {
            AgentError::Provider(format!(
                "parse response ({}B): {}",
                bytes.len(),
                e
            ))
        })?;

        // OpenRouter sometimes returns 200 OK with an `error` object in
        // the body (typically when an upstream provider rejects the
        // request and OpenRouter has chosen to relay the failure). The
        // success schema doesn't contain `choices` in that case, so
        // surface the embedded message instead of falling through to a
        // confusing "missing choices" error.
        if let Some(err) = response.get("error") {
            return Err(AgentError::Provider(format_embedded_error(err)));
        }

        let message = response
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .cloned()
            .ok_or_else(|| {
                AgentError::Provider(format!("response missing choices[0].message: {}", response))
            })?;

        let usage = response.get("usage").and_then(Usage::from_value);

        Ok(ChatResponse { message, usage })
    }

    async fn chat_stream(&self, req: ChatRequest, sink: ContentSink<'_>) -> Result<ChatResponse> {
        let payload = json!({
            "model": req.model,
            "messages": req.messages,
            "tools": req.tools,
            "stream": true,
            // Required by the OpenAI spec for streaming responses to carry
            // a `usage` block in the terminal chunk. Most OpenAI-compat
            // servers (Ollama, OpenRouter) honor this; ones that don't
            // simply leave usage `None`, which the agent already handles.
            "stream_options": { "include_usage": true },
        });

        let resp = self
            .client
            .post(self.endpoint("chat/completions"))
            .bearer_auth(&self.api_key)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .json(&payload)
            .send()
            .await
            .map_err(|e| AgentError::Provider(format!("stream request: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let bytes = resp.bytes().await.unwrap_or_default();
            return Err(AgentError::Provider(error_message_from_body(
                status.as_u16(),
                &bytes,
            )));
        }

        let mut stream = resp.bytes_stream().eventsource();

        let mut role: Option<String> = None;
        let mut content = String::new();
        let mut calls: Vec<ToolCallAcc> = Vec::new();
        let mut usage: Option<Usage> = None;

        while let Some(event) = stream.next().await {
            let event = event.map_err(|e| AgentError::Provider(format!("SSE: {}", e)))?;
            // OpenAI's stream protocol terminates with `data: [DONE]`.
            // Anything else is supposed to be JSON; some upstream
            // providers occasionally emit comments or empty data lines.
            if event.data.trim() == "[DONE]" {
                break;
            }
            if event.data.trim().is_empty() {
                continue;
            }
            let chunk: Value = match serde_json::from_str(&event.data) {
                Ok(v) => v,
                Err(_) => continue,
            };

            // Stream-level error: OpenRouter relays upstream failures
            // mid-stream as a chunk with a top-level `error` field.
            if let Some(err) = chunk.get("error") {
                return Err(AgentError::Provider(format_embedded_error(err)));
            }

            // The terminal chunk in `include_usage` streams has empty
            // `choices` and a populated `usage`. Capture either or both
            // per chunk; the loop falls through naturally.
            if let Some(u) = chunk.get("usage").and_then(Usage::from_value) {
                usage = Some(u);
            }

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

        Ok(ChatResponse { message, usage })
    }

    async fn list_models(&self) -> Result<Vec<String>> {
        let resp = self
            .client
            .get(self.endpoint("models"))
            .bearer_auth(&self.api_key)
            .send()
            .await
            .map_err(|e| AgentError::Provider(format!("models request: {}", e)))?;

        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| AgentError::Provider(format!("models read body: {}", e)))?;

        if !status.is_success() {
            return Err(AgentError::Provider(error_message_from_body(
                status.as_u16(),
                &bytes,
            )));
        }

        let resp: Value = serde_json::from_slice(&bytes)
            .map_err(|e| AgentError::Provider(format!("models parse: {}", e)))?;
        let data = resp
            .get("data")
            .and_then(|v| v.as_array())
            .ok_or_else(|| AgentError::Provider("models response missing `data` array".into()))?;
        Ok(data
            .iter()
            .filter_map(|m| m.get("id").and_then(|v| v.as_str()).map(String::from))
            .collect())
    }
}

#[derive(Default)]
struct ToolCallAcc {
    id: String,
    name: String,
    arguments: String,
}

/// Pull a useful message out of an error response body. Tries to parse
/// JSON and find an embedded `error.message`; falls back to the raw
/// body text or just the HTTP status when the body is binary/empty.
fn error_message_from_body(status: u16, body: &[u8]) -> String {
    if let Ok(v) = serde_json::from_slice::<Value>(body) {
        if let Some(err) = v.get("error") {
            return format!("HTTP {}: {}", status, format_embedded_error(err));
        }
    }
    let text = String::from_utf8_lossy(body);
    if text.is_empty() {
        format!("HTTP {}", status)
    } else {
        format!("HTTP {}: {}", status, text)
    }
}

/// Render an `error` object into a single-line message. Covers the
/// OpenAI shape `{message, type, code}` and OpenRouter's relayed shape
/// `{message, code, metadata: {raw, provider_name}}`. `code` is
/// rendered untyped so an integer (`429`) renders as `429` rather than
/// `"429"`.
fn format_embedded_error(err: &Value) -> String {
    let message = err
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("(no message)");
    let code = err.get("code").map(|v| match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    });
    let provider = err
        .get("metadata")
        .and_then(|m| m.get("provider_name"))
        .and_then(|v| v.as_str());

    let mut out = String::new();
    if let Some(code) = code {
        out.push_str(&format!("[{}] ", code));
    }
    out.push_str(message);
    if let Some(p) = provider {
        out.push_str(&format!(" (upstream: {})", p));
    }
    // Include the raw upstream body when present — its details are
    // often more actionable than the relayed top-level message.
    if let Some(raw) = err
        .get("metadata")
        .and_then(|m| m.get("raw"))
        .and_then(|v| v.as_str())
    {
        let one_line: String = raw.chars().map(|c| if c == '\n' { ' ' } else { c }).collect();
        out.push_str(&format!(" — raw: {}", one_line));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_error_renders_integer_code() {
        let err = json!({
            "message": "Provider returned error",
            "code": 429,
            "metadata": {
                "raw": "deepseek/deepseek-v4-pro is temporarily rate-limited upstream.",
                "provider_name": "Together"
            }
        });
        let msg = format_embedded_error(&err);
        assert!(msg.contains("[429]"), "expected [429] code prefix: {}", msg);
        assert!(
            msg.contains("Provider returned error"),
            "expected message: {}",
            msg
        );
        assert!(
            msg.contains("(upstream: Together)"),
            "expected upstream tag: {}",
            msg
        );
        assert!(msg.contains("rate-limited upstream"), "expected raw: {}", msg);
    }

    #[test]
    fn embedded_error_renders_string_code() {
        let err = json!({
            "message": "Bad request",
            "code": "invalid_request_error"
        });
        let msg = format_embedded_error(&err);
        assert!(msg.contains("[invalid_request_error]"), "{}", msg);
        assert!(msg.contains("Bad request"), "{}", msg);
    }

    #[test]
    fn embedded_error_with_no_code_or_provider() {
        let err = json!({"message": "boom"});
        let msg = format_embedded_error(&err);
        assert_eq!(msg, "boom");
    }

    #[test]
    fn error_message_from_body_extracts_embedded_error() {
        let body = br#"{"error":{"message":"nope","code":401}}"#;
        let msg = error_message_from_body(401, body);
        assert!(msg.contains("HTTP 401"), "{}", msg);
        assert!(msg.contains("[401]"), "{}", msg);
        assert!(msg.contains("nope"), "{}", msg);
    }

    #[test]
    fn error_message_from_body_falls_back_to_text() {
        let body = b"<html>500</html>";
        let msg = error_message_from_body(500, body);
        assert!(msg.contains("HTTP 500"));
        assert!(msg.contains("<html>"));
    }

    #[test]
    fn error_message_from_body_handles_empty_body() {
        let msg = error_message_from_body(503, b"");
        assert_eq!(msg, "HTTP 503");
    }
}
