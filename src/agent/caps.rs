//! Model-capability registry.
//!
//! The harness needs three things per model:
//! - context window size (drives compaction target),
//! - whether the model emits tool calls in the structured `tool_calls`
//!   field (otherwise the fallback parser scans `content` for them),
//! - whether the streaming endpoint emits tool-call deltas incrementally
//!   (assembly behavior we already do is fine either way, but logging
//!   and future heuristics may want to know).
//!
//! Lookup is by longest-matching prefix, with a conservative default for
//! anything we don't recognize. The hardcoded `ENTRIES` table covers the
//! common families; users can extend or override via `[[caps]]` config
//! entries (handled by `caps_for_with_overrides`).

use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelCaps {
    pub ctx_window: usize,
    pub supports_native_tool_calls: bool,
    pub supports_streaming_tool_deltas: bool,
}

impl ModelCaps {
    /// Soft target for compaction — leaves headroom for the response and
    /// any in-flight tool round before the model truly hits its window.
    pub fn compact_target(&self) -> usize {
        self.ctx_window * 80 / 100
    }
}

const DEFAULT: ModelCaps = ModelCaps {
    ctx_window: 8_000,
    supports_native_tool_calls: false,
    supports_streaming_tool_deltas: false,
};

/// Hardcoded entries. Order matters: the first matching prefix wins, so
/// keep more specific names ahead of more general ones.
const ENTRIES: &[(&str, ModelCaps)] = &[
    // --- Local (Ollama) ---
    (
        "qwen2.5-coder",
        ModelCaps {
            ctx_window: 32_000,
            // Smoke test confirmed qwen2.5-coder:7b emits tool calls as
            // raw JSON in `content`, not in `tool_calls`. Capability
            // turned off so the fallback parser engages.
            supports_native_tool_calls: false,
            supports_streaming_tool_deltas: false,
        },
    ),
    (
        "llama3.2",
        ModelCaps {
            ctx_window: 128_000,
            supports_native_tool_calls: true,
            supports_streaming_tool_deltas: true,
        },
    ),
    (
        "llama3.1",
        ModelCaps {
            ctx_window: 128_000,
            supports_native_tool_calls: true,
            supports_streaming_tool_deltas: true,
        },
    ),
    (
        "llama3",
        ModelCaps {
            ctx_window: 8_000,
            // Ollama refuses requests with `tools` against this model
            // (smoke-tested). Treat as no native support; user should
            // pick a different local model for tool use.
            supports_native_tool_calls: false,
            supports_streaming_tool_deltas: false,
        },
    ),
    // --- Cloud (OpenRouter / Anthropic / OpenAI) ---
    (
        "anthropic/claude",
        ModelCaps {
            ctx_window: 200_000,
            supports_native_tool_calls: true,
            supports_streaming_tool_deltas: true,
        },
    ),
    (
        "claude-",
        ModelCaps {
            ctx_window: 200_000,
            supports_native_tool_calls: true,
            supports_streaming_tool_deltas: true,
        },
    ),
    (
        "gpt-4",
        ModelCaps {
            ctx_window: 128_000,
            supports_native_tool_calls: true,
            supports_streaming_tool_deltas: true,
        },
    ),
    (
        "gpt-",
        ModelCaps {
            ctx_window: 16_000,
            supports_native_tool_calls: true,
            supports_streaming_tool_deltas: true,
        },
    ),
];

pub fn caps_for(model: &str) -> ModelCaps {
    for (prefix, caps) in ENTRIES {
        if model.starts_with(prefix) {
            return *caps;
        }
    }
    DEFAULT
}

/// User-supplied capability override. Matches by `prefix.starts_with`.
/// `[[caps]]` config entries deserialize into this; a caller's overrides
/// take precedence over the hardcoded `ENTRIES` table.
#[derive(Clone, Debug, Deserialize)]
pub struct CapsOverride {
    pub prefix: String,
    pub ctx_window: usize,
    #[serde(default)]
    pub supports_native_tool_calls: bool,
    #[serde(default)]
    pub supports_streaming_tool_deltas: bool,
}

impl CapsOverride {
    pub fn to_caps(&self) -> ModelCaps {
        ModelCaps {
            ctx_window: self.ctx_window,
            supports_native_tool_calls: self.supports_native_tool_calls,
            supports_streaming_tool_deltas: self.supports_streaming_tool_deltas,
        }
    }
}

/// Lookup capabilities for `model`. User-supplied overrides win over the
/// hardcoded table. Within a list, the first matching prefix wins, so
/// callers can shadow a built-in entry by listing a more specific one
/// earlier.
pub fn caps_for_with_overrides(model: &str, overrides: &[CapsOverride]) -> ModelCaps {
    for ov in overrides {
        if model.starts_with(&ov.prefix) {
            return ov.to_caps();
        }
    }
    caps_for(model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_model_yields_conservative_default() {
        let c = caps_for("totally-made-up:1b");
        assert_eq!(c, DEFAULT);
    }

    #[test]
    fn qwen_coder_disables_native_tools_per_smoke_test() {
        let c = caps_for("qwen2.5-coder:7b");
        assert_eq!(c.ctx_window, 32_000);
        assert!(!c.supports_native_tool_calls);
    }

    #[test]
    fn llama3_2_keeps_native_tools_unlike_llama3_base() {
        assert!(caps_for("llama3.2:latest").supports_native_tool_calls);
        assert!(!caps_for("llama3:latest").supports_native_tool_calls);
    }

    #[test]
    fn anthropic_models_match_via_prefix() {
        assert!(caps_for("anthropic/claude-haiku-4.5").supports_native_tool_calls);
        assert_eq!(caps_for("anthropic/claude-haiku-4.5").ctx_window, 200_000);
    }

    #[test]
    fn claude_dash_prefix_catches_native_anthropic_ids() {
        assert!(caps_for("claude-opus-4-7").supports_native_tool_calls);
    }

    #[test]
    fn gpt_4_specific_overrides_gpt_general() {
        assert_eq!(caps_for("gpt-4o").ctx_window, 128_000);
        assert_eq!(caps_for("gpt-3.5-turbo").ctx_window, 16_000);
    }

    #[test]
    fn compact_target_leaves_headroom_for_response_and_tools() {
        let c = caps_for("qwen2.5-coder:7b");
        assert_eq!(c.compact_target(), 32_000 * 80 / 100);
        assert!(c.compact_target() < c.ctx_window);
    }

    #[test]
    fn override_takes_precedence_over_hardcoded_entry() {
        let overrides = vec![CapsOverride {
            prefix: "qwen2.5-coder".into(),
            ctx_window: 64_000,
            supports_native_tool_calls: true,
            supports_streaming_tool_deltas: true,
        }];
        let c = caps_for_with_overrides("qwen2.5-coder:7b", &overrides);
        assert_eq!(c.ctx_window, 64_000);
        assert!(c.supports_native_tool_calls);
    }

    #[test]
    fn override_can_introduce_brand_new_model_family() {
        let overrides = vec![CapsOverride {
            prefix: "my-custom".into(),
            ctx_window: 4_096,
            supports_native_tool_calls: false,
            supports_streaming_tool_deltas: false,
        }];
        let c = caps_for_with_overrides("my-custom:13b", &overrides);
        assert_eq!(c.ctx_window, 4_096);
    }

    #[test]
    fn no_override_match_falls_through_to_hardcoded_then_default() {
        // Model unknown to both lists → DEFAULT.
        let overrides = vec![CapsOverride {
            prefix: "irrelevant".into(),
            ctx_window: 1,
            supports_native_tool_calls: true,
            supports_streaming_tool_deltas: true,
        }];
        let c = caps_for_with_overrides("totally-made-up:1b", &overrides);
        assert_eq!(c, DEFAULT);
    }

    #[test]
    fn override_order_decides_which_wins_when_two_prefixes_overlap() {
        let overrides = vec![
            CapsOverride {
                prefix: "qwen2.5-coder:7b".into(),
                ctx_window: 16_000,
                supports_native_tool_calls: true,
                supports_streaming_tool_deltas: true,
            },
            CapsOverride {
                prefix: "qwen2.5-coder".into(),
                ctx_window: 99_000,
                supports_native_tool_calls: false,
                supports_streaming_tool_deltas: false,
            },
        ];
        // Specific entry comes first → it wins.
        let c = caps_for_with_overrides("qwen2.5-coder:7b", &overrides);
        assert_eq!(c.ctx_window, 16_000);
        // Lookup for a sibling tag falls through to the broader prefix.
        let c = caps_for_with_overrides("qwen2.5-coder:32b", &overrides);
        assert_eq!(c.ctx_window, 99_000);
    }
}
