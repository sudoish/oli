//! Tool-call fallback parser for models that emit calls as text rather
//! than in the structured `tool_calls` field.
//!
//! Engaged from `Agent::run_streaming` only when
//! `caps.supports_native_tool_calls == false` — capable models keep their
//! own behavior. The smoke test against `qwen2.5-coder:7b` motivated this:
//! the model emits raw JSON like `{"name":"Read","arguments":{"file_path":"..."}}`
//! in `content` with no structured field, so the agent loop today treats
//! it as a final answer. With the parser in place, we splice the parsed
//! call into the assistant message before the loop checks `tool_calls`.
//!
//! Patterns covered:
//! - Bare JSON object anywhere in the content (qwen-style).
//! - JSON wrapped in `<tool_call>...</tool_call>` tags (Hermes-style).
//! - JSON inside a fenced ```json ... ``` block.
//!
//! The implementation finds JSON objects via `serde_json::Deserializer`'s
//! streaming parser starting at each `{` byte, which incidentally handles
//! all three patterns the same way (preamble text and wrapping markers
//! are simply skipped).

use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};

static SYNTH_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Try to extract one or more tool calls from free-form assistant content.
/// Returns the calls in OpenAI `tool_calls` shape with synthesized
/// globally-unique ids. Returns `None` when no candidate object with a
/// `name` field is found.
pub fn parse_text_tool_calls(content: &str) -> Option<Vec<Value>> {
    let mut out: Vec<Value> = Vec::new();
    let mut cursor = 0;

    while let Some(start) = content[cursor..].find('{').map(|i| cursor + i) {
        let slice = &content[start..];
        let mut iter = serde_json::Deserializer::from_str(slice).into_iter::<Value>();
        match iter.next() {
            Some(Ok(v)) => {
                let consumed = iter.byte_offset();
                if let Some(call) = into_tool_call(&v) {
                    out.push(call);
                }
                cursor = start + consumed;
            }
            _ => {
                // Not a parseable JSON object here — advance one char and
                // keep scanning. We can't always step by 1 byte safely
                // because of UTF-8, so use char_indices to find the next
                // boundary.
                let next = content[start..]
                    .char_indices()
                    .nth(1)
                    .map(|(i, _)| start + i)
                    .unwrap_or(content.len());
                cursor = next;
            }
        }
    }

    if out.is_empty() { None } else { Some(out) }
}

fn into_tool_call(v: &Value) -> Option<Value> {
    let name = v.get("name").and_then(|n| n.as_str())?;
    if name.is_empty() {
        return None;
    }
    let args = v.get("arguments").cloned().unwrap_or(json!({}));
    // OpenAI's tool_calls schema expects arguments as a string (it's the
    // raw JSON the model would have emitted). Encode whatever we found.
    let args_str = match &args {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    Some(json!({
        "id": next_synth_id(),
        "type": "function",
        "function": {
            "name": name,
            "arguments": args_str,
        }
    }))
}

fn next_synth_id() -> String {
    let n = SYNTH_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("call_synth_{}", n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name_of(call: &Value) -> &str {
        call["function"]["name"].as_str().unwrap()
    }

    fn args_of(call: &Value) -> Value {
        let s = call["function"]["arguments"].as_str().unwrap();
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn parses_bare_qwen_style_json() {
        // Output that the smoke test produced verbatim.
        let content = r#"{"name": "Read", "arguments": {"file_path": "Cargo.toml"}}"#;
        let calls = parse_text_tool_calls(content).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(name_of(&calls[0]), "Read");
        assert_eq!(args_of(&calls[0])["file_path"], "Cargo.toml");
    }

    #[test]
    fn parses_json_with_text_preamble() {
        let content = r#"I'll read the file:
{"name": "Read", "arguments": {"file_path": "Cargo.toml"}}"#;
        let calls = parse_text_tool_calls(content).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(name_of(&calls[0]), "Read");
    }

    #[test]
    fn parses_fenced_json_block() {
        let content = r#"
Here's the call:
```json
{"name": "Glob", "arguments": {"pattern": "**/*.rs"}}
```
"#;
        let calls = parse_text_tool_calls(content).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(name_of(&calls[0]), "Glob");
    }

    #[test]
    fn parses_tag_wrapped_json() {
        let content = r#"<tool_call>{"name": "Bash", "arguments": {"command": "ls"}}</tool_call>"#;
        let calls = parse_text_tool_calls(content).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(name_of(&calls[0]), "Bash");
    }

    #[test]
    fn parses_two_calls_in_one_message() {
        let content = r#"
{"name": "Read", "arguments": {"file_path": "a.rs"}}
{"name": "Read", "arguments": {"file_path": "b.rs"}}
"#;
        let calls = parse_text_tool_calls(content).unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(args_of(&calls[0])["file_path"], "a.rs");
        assert_eq!(args_of(&calls[1])["file_path"], "b.rs");
    }

    #[test]
    fn returns_none_for_plain_text() {
        assert!(parse_text_tool_calls("just some assistant prose, no tool call").is_none());
    }

    #[test]
    fn returns_none_when_object_lacks_name() {
        assert!(parse_text_tool_calls(r#"{"arguments": {"foo": 1}}"#).is_none());
        assert!(parse_text_tool_calls(r#"{}"#).is_none());
        assert!(parse_text_tool_calls(r#"{"unrelated": "json"}"#).is_none());
    }

    #[test]
    fn returns_none_for_invalid_json_braces() {
        // Looks like JSON but isn't (unquoted keys, trailing comma, etc.).
        assert!(parse_text_tool_calls("{name: notvalid}").is_none());
    }

    #[test]
    fn synthetic_ids_are_unique_within_a_message() {
        let content = r#"
{"name":"X","arguments":{}}
{"name":"X","arguments":{}}
"#;
        let calls = parse_text_tool_calls(content).unwrap();
        let id0 = calls[0]["id"].as_str().unwrap();
        let id1 = calls[1]["id"].as_str().unwrap();
        assert_ne!(id0, id1);
        assert!(id0.starts_with("call_synth_"));
    }

    #[test]
    fn arguments_default_to_empty_object_when_missing() {
        let content = r#"{"name": "Glob"}"#;
        let calls = parse_text_tool_calls(content).unwrap();
        assert_eq!(args_of(&calls[0]), json!({}));
    }

    #[test]
    fn arguments_already_string_pass_through_unchanged() {
        // Some models emit `arguments` as a pre-stringified JSON blob.
        let content = r#"{"name": "Read", "arguments": "{\"file_path\":\"x.rs\"}"}"#;
        let calls = parse_text_tool_calls(content).unwrap();
        let raw = calls[0]["function"]["arguments"].as_str().unwrap();
        assert_eq!(raw, r#"{"file_path":"x.rs"}"#);
    }
}
