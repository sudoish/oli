//! Preflight token estimate for the request about to be sent.
//!
//! Provider-neutral by construction: it reads the same JSON the agent
//! is about to put on the wire and knows nothing about any provider's
//! tokenizer. The number is a budget, not a bill — the ledger stores it
//! next to what the provider actually charged, so the error is visible
//! rather than assumed away.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent::memory::ContextParts;

/// Characters per token. Real tokenizers disagree with each other, let
/// alone with this; 4 is the long-standing rule of thumb across English
/// prose and source code.
const CHARS_PER_TOKEN: u64 = 4;

/// Framing every message carries beyond its content — role, separators,
/// and the wrapper the provider adds around each turn.
const MESSAGE_OVERHEAD_TOKENS: u64 = 4;

/// Preflight estimate for one request, attributed to where each part of
/// the context came from. The four categories sum to `total`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextEstimate {
    /// System prompt and anything else pinned past compaction.
    pub pinned: u64,
    /// Tool definitions shipped with the request.
    pub tool_schemas: u64,
    /// The memory strategy's stand-in for records it dropped.
    pub summary: u64,
    /// Records still held verbatim.
    pub recent: u64,
    pub total: u64,
}

/// Tokens for a serialized JSON value, structure included — brackets and
/// field names are on the wire too, which is most of a tool schema.
pub fn estimate_value(v: &Value) -> u64 {
    let encoded = v.to_string();
    encoded.chars().count().div_ceil(CHARS_PER_TOKEN as usize) as u64
}

pub fn estimate_message(m: &Value) -> u64 {
    estimate_value(m) + MESSAGE_OVERHEAD_TOKENS
}

pub fn estimate_messages(ms: &[Value]) -> u64 {
    ms.iter().map(estimate_message).sum()
}

/// Tool definitions carry no per-message framing — they ride in their
/// own request field.
pub fn estimate_tool_schemas(schemas: &[Value]) -> u64 {
    schemas.iter().map(estimate_value).sum()
}

pub fn estimate_context(parts: &ContextParts, tool_schemas: &[Value]) -> ContextEstimate {
    let pinned = estimate_messages(&parts.pinned);
    let summary = estimate_messages(&parts.summary);
    let recent = estimate_messages(&parts.recent);
    let tools = estimate_tool_schemas(tool_schemas);
    ContextEstimate {
        pinned,
        tool_schemas: tools,
        summary,
        recent,
        total: pinned + tools + summary + recent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_known_message_set_estimates_to_a_stable_number() {
        // `{"role":"user","content":"hello"}` is 33 characters, so 9
        // tokens of content plus 4 of framing.
        let m = json!({"role": "user", "content": "hello"});
        assert_eq!(m.to_string().chars().count(), 33);
        assert_eq!(estimate_message(&m), 13);
    }

    #[test]
    fn a_partial_final_token_rounds_up_rather_than_vanishing() {
        // One character is still a token, not a quarter of one.
        assert_eq!(estimate_value(&json!("a")), 1);
    }

    #[test]
    fn every_category_of_context_is_attributed_and_the_parts_sum_to_the_whole() {
        let parts = ContextParts {
            pinned: vec![json!({"role":"system","content":"you are an agent"})],
            summary: vec![json!({"role":"system","content":"[Earlier conversation summary] x"})],
            recent: vec![
                json!({"role":"user","content":"do the thing"}),
                json!({"role":"assistant","content":"done"}),
            ],
        };
        let schemas = vec![json!({
            "type": "function",
            "function": {"name": "Read", "description": "read a file", "parameters": {}}
        })];

        let e = estimate_context(&parts, &schemas);
        assert!(e.pinned > 0);
        assert!(e.tool_schemas > 0);
        assert!(e.summary > 0);
        assert!(e.recent > 0);
        assert_eq!(e.pinned + e.tool_schemas + e.summary + e.recent, e.total);
    }

    #[test]
    fn an_empty_request_estimates_to_zero_in_every_category() {
        let e = estimate_context(&ContextParts::default(), &[]);
        assert_eq!(e, ContextEstimate::default());
    }

    #[test]
    fn tool_schemas_are_attributed_apart_from_the_messages_they_ship_with() {
        let parts = ContextParts {
            recent: vec![json!({"role":"user","content":"hi"})],
            ..ContextParts::default()
        };
        let schemas = vec![json!({"type":"function","function":{"name":"Read"}})];
        let with = estimate_context(&parts, &schemas);
        let without = estimate_context(&parts, &[]);
        assert_eq!(without.tool_schemas, 0);
        assert_eq!(with.recent, without.recent);
        assert_eq!(with.total - without.total, with.tool_schemas);
    }
}
