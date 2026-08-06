//! Hook dispatcher — shared between built-in hooks and Lua-plugin
//! hooks. One trait, one registry, two registration sources.
//!
//! Three event kinds fire from `Agent::run_streaming`:
//! - `PreToolUse` — before the policy check, so a hook sees the model's
//!   intent regardless of whether it ends up running.
//! - `PostToolUse` — after dispatch, with the result string the model
//!   will see (including "policy denied" / "user declined" outcomes).
//! - `Stop` — once the loop returns a final assistant message with no
//!   further tool calls.
//!
//! ## Outcomes
//!
//! Hooks return a `HookOutcome` to influence the loop:
//! - `Continue`: no change.
//! - `Skip(result)` (PreToolUse only): short-circuit the tool dispatch
//!   and use `result` as the synthetic tool result. `PostToolUse`
//!   hooks still fire on that synthetic string. Subsequent
//!   `PreToolUse` hooks in the chain are *not* called.
//! - `Replace(value)`: mutate the relevant field for the rest of the
//!   chain.
//!   - `PreToolUse`: `value` becomes the new args (any JSON value).
//!   - `PostToolUse`: `value` becomes the new result string (extracted
//!     from a JSON string, or stringified for non-string values).
//!   - `Stop`: `value` becomes the new final assistant content.

use async_trait::async_trait;
use serde_json::Value;

#[derive(Clone, Debug)]
pub enum HookPayload<'a> {
    PreToolUse {
        tool: &'a str,
        args: &'a Value,
    },
    PostToolUse {
        tool: &'a str,
        args: &'a Value,
        result: &'a str,
    },
    Stop {
        final_content: &'a str,
    },
}

#[derive(Clone, Debug, Default)]
pub enum HookOutcome {
    #[default]
    Continue,
    Skip(String),
    Replace(Value),
}

#[async_trait]
pub trait Hook: Send + Sync {
    /// Display name. `/hooks` (future) and the plugin loader use this
    /// for diagnostics.
    fn name(&self) -> &str;

    /// Handle a payload and return whether to continue the loop, skip
    /// the tool, or replace the relevant field. Default returns
    /// `Continue` so observe-only hooks can omit it.
    async fn handle(&self, payload: &HookPayload<'_>) -> HookOutcome;
}

/// Outcome of running every `PreToolUse` hook in sequence.
#[derive(Clone, Debug)]
pub enum PreToolDecision {
    /// Carry on with dispatch. `args` is the (possibly hook-replaced)
    /// args the policy + tool will see.
    Continue { args: Value },
    /// A hook short-circuited; `result` is the synthetic tool result
    /// the model will see in place of a real dispatch. `args` is the
    /// last hook-visible state, threaded through to `PostToolUse`.
    Skip { args: Value, result: String },
}

#[derive(Default)]
pub struct HookRegistry {
    hooks: Vec<Box<dyn Hook>>,
}

impl HookRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<H: Hook + 'static>(&mut self, hook: H) {
        self.hooks.push(Box::new(hook));
    }

    /// Insert an already-boxed hook. The plugin loader uses this to
    /// register Lua-backed `Hook` impls without re-boxing.
    pub fn register_box(&mut self, hook: Box<dyn Hook>) {
        self.hooks.push(hook);
    }

    /// Hook count. Useful for diagnostics / `/plugins` output;
    /// not on the agent's hot path, hence the dead-code allow.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.hooks.len()
    }

    /// Iterate registered hooks. Public so the binary or future
    /// debug commands can introspect the registry.
    #[allow(dead_code)]
    pub fn iter(&self) -> impl Iterator<Item = &dyn Hook> {
        self.hooks.iter().map(|b| b.as_ref())
    }

    /// Drop every hook whose `name()` matches `name`. Used by
    /// `/plugins reload` to clear stale plugin hooks before
    /// re-registering. Returns the number of hooks removed.
    /// Plugin-registered hooks share the plugin id as their name, so a
    /// single removal call sweeps every event handler that plugin
    /// installed.
    pub fn remove_by_name(&mut self, name: &str) -> usize {
        let before = self.hooks.len();
        self.hooks.retain(|h| h.name() != name);
        before - self.hooks.len()
    }

    /// Compose every hook's outcome on a `PreToolUse` event. `Replace`
    /// mutates `args` for the remaining chain; the first `Skip`
    /// terminates the chain (subsequent pre hooks do not fire) and the
    /// synthetic result becomes the tool result. `Continue` is a no-op.
    pub async fn dispatch_pre_tool_use(&self, tool: &str, mut args: Value) -> PreToolDecision {
        for hook in &self.hooks {
            let payload = HookPayload::PreToolUse { tool, args: &args };
            match hook.handle(&payload).await {
                HookOutcome::Continue => {}
                HookOutcome::Replace(v) => args = v,
                HookOutcome::Skip(result) => return PreToolDecision::Skip { args, result },
            }
        }
        PreToolDecision::Continue { args }
    }

    /// Compose every hook's outcome on a `PostToolUse` event. `Replace`
    /// mutates the result string for the remaining chain. `Skip` is
    /// not meaningful here (it's pre-only) and is treated as
    /// `Continue` — observers still get to see and audit the result
    /// even if a pre-hook short-circuited the dispatch.
    pub async fn dispatch_post_tool_use(
        &self,
        tool: &str,
        args: &Value,
        mut result: String,
    ) -> String {
        for hook in &self.hooks {
            let payload = HookPayload::PostToolUse {
                tool,
                args,
                result: &result,
            };
            match hook.handle(&payload).await {
                HookOutcome::Continue | HookOutcome::Skip(_) => {}
                HookOutcome::Replace(v) => result = value_to_string(v),
            }
        }
        result
    }

    /// Compose every hook's outcome on a `Stop` event. `Replace`
    /// mutates the final assistant content for the remaining chain
    /// and for the agent's return value. `Skip` is not meaningful
    /// here and is treated as `Continue`.
    pub async fn dispatch_stop(&self, mut content: String) -> String {
        for hook in &self.hooks {
            let payload = HookPayload::Stop {
                final_content: &content,
            };
            match hook.handle(&payload).await {
                HookOutcome::Continue | HookOutcome::Skip(_) => {}
                HookOutcome::Replace(v) => content = value_to_string(v),
            }
        }
        content
    }
}

/// Coerce a `Replace(Value)` outcome into the string field it's about
/// to overwrite. JSON strings are unwrapped (so a hook returning
/// `Value::String("x")` lands as `"x"`, not `"\"x\""`); everything
/// else is JSON-stringified.
fn value_to_string(v: Value) -> String {
    match v {
        Value::String(s) => s,
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::Mutex;

    /// Records every payload it sees for later assertion. Returns
    /// `Continue` so it's purely observational.
    struct Recorder {
        log: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Hook for Recorder {
        fn name(&self) -> &str {
            "recorder"
        }
        async fn handle(&self, payload: &HookPayload<'_>) -> HookOutcome {
            let entry = match payload {
                HookPayload::PreToolUse { tool, .. } => format!("pre:{}", tool),
                HookPayload::PostToolUse { tool, result, .. } => {
                    format!("post:{}:{}", tool, result)
                }
                HookPayload::Stop { final_content } => format!("stop:{}", final_content),
            };
            self.log.lock().unwrap().push(entry);
            HookOutcome::Continue
        }
    }

    #[tokio::test]
    async fn observers_run_in_order_for_pre_post_and_stop() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut reg = HookRegistry::new();
        reg.register(Recorder { log: log.clone() });
        reg.register(Recorder { log: log.clone() });

        let args = json!({});
        let _ = reg.dispatch_pre_tool_use("Read", args.clone()).await;
        let _ = reg.dispatch_post_tool_use("Read", &args, "ok".into()).await;
        let _ = reg.dispatch_stop("done".into()).await;

        let recorded = log.lock().unwrap().clone();
        assert_eq!(
            recorded,
            vec![
                "pre:Read",
                "pre:Read",
                "post:Read:ok",
                "post:Read:ok",
                "stop:done",
                "stop:done",
            ]
        );
    }

    #[tokio::test]
    async fn pre_hook_skip_short_circuits_and_returns_synthetic_result() {
        struct Skipper;
        #[async_trait]
        impl Hook for Skipper {
            fn name(&self) -> &str {
                "skipper"
            }
            async fn handle(&self, p: &HookPayload<'_>) -> HookOutcome {
                if matches!(p, HookPayload::PreToolUse { .. }) {
                    HookOutcome::Skip("synthetic".into())
                } else {
                    HookOutcome::Continue
                }
            }
        }

        let mut reg = HookRegistry::new();
        reg.register(Skipper);
        let decision = reg.dispatch_pre_tool_use("Read", json!({})).await;
        match decision {
            PreToolDecision::Skip { result, .. } => assert_eq!(result, "synthetic"),
            _ => panic!("expected Skip"),
        }
    }

    #[tokio::test]
    async fn pre_hook_skip_terminates_chain_subsequent_hooks_do_not_fire() {
        struct Skipper;
        #[async_trait]
        impl Hook for Skipper {
            fn name(&self) -> &str {
                "skipper"
            }
            async fn handle(&self, _: &HookPayload<'_>) -> HookOutcome {
                HookOutcome::Skip("blocked".into())
            }
        }
        struct ShouldNotRun;
        #[async_trait]
        impl Hook for ShouldNotRun {
            fn name(&self) -> &str {
                "snr"
            }
            async fn handle(&self, _: &HookPayload<'_>) -> HookOutcome {
                panic!("subsequent pre hook ran after Skip");
            }
        }

        let mut reg = HookRegistry::new();
        reg.register(Skipper);
        reg.register(ShouldNotRun);
        let _ = reg.dispatch_pre_tool_use("Read", json!({})).await;
    }

    #[tokio::test]
    async fn pre_hook_replace_mutates_args_for_downstream_hooks_and_caller() {
        struct Replacer;
        #[async_trait]
        impl Hook for Replacer {
            fn name(&self) -> &str {
                "replacer"
            }
            async fn handle(&self, p: &HookPayload<'_>) -> HookOutcome {
                if let HookPayload::PreToolUse { args, .. } = p {
                    let mut new_args: Value = (*args).clone();
                    if let Some(o) = new_args.as_object_mut() {
                        o.insert("injected".into(), json!(true));
                    }
                    HookOutcome::Replace(new_args)
                } else {
                    HookOutcome::Continue
                }
            }
        }
        struct Inspector(Arc<Mutex<Option<Value>>>);
        #[async_trait]
        impl Hook for Inspector {
            fn name(&self) -> &str {
                "inspector"
            }
            async fn handle(&self, p: &HookPayload<'_>) -> HookOutcome {
                if let HookPayload::PreToolUse { args, .. } = p {
                    *self.0.lock().unwrap() = Some((*args).clone());
                }
                HookOutcome::Continue
            }
        }

        let seen = Arc::new(Mutex::new(None));
        let mut reg = HookRegistry::new();
        reg.register(Replacer);
        reg.register(Inspector(seen.clone()));

        let decision = reg.dispatch_pre_tool_use("Read", json!({"x": 1})).await;
        let downstream_args = seen.lock().unwrap().clone().unwrap();
        assert_eq!(downstream_args["injected"], json!(true));
        match decision {
            PreToolDecision::Continue { args } => {
                assert_eq!(args["injected"], json!(true));
                assert_eq!(args["x"], json!(1));
            }
            _ => panic!("expected Continue"),
        }
    }

    #[tokio::test]
    async fn post_hook_replace_substitutes_result_string() {
        struct Redactor;
        #[async_trait]
        impl Hook for Redactor {
            fn name(&self) -> &str {
                "redactor"
            }
            async fn handle(&self, p: &HookPayload<'_>) -> HookOutcome {
                if let HookPayload::PostToolUse { result, .. } = p {
                    if result.contains("secret") {
                        return HookOutcome::Replace(Value::String("[redacted]".into()));
                    }
                }
                HookOutcome::Continue
            }
        }

        let mut reg = HookRegistry::new();
        reg.register(Redactor);
        let r = reg
            .dispatch_post_tool_use("Bash", &json!({}), "secret token: abc".into())
            .await;
        assert_eq!(r, "[redacted]");
    }

    #[tokio::test]
    async fn stop_hook_replace_substitutes_final_content() {
        struct Appender;
        #[async_trait]
        impl Hook for Appender {
            fn name(&self) -> &str {
                "appender"
            }
            async fn handle(&self, p: &HookPayload<'_>) -> HookOutcome {
                if let HookPayload::Stop { final_content } = p {
                    HookOutcome::Replace(Value::String(format!("{} [audited]", final_content)))
                } else {
                    HookOutcome::Continue
                }
            }
        }

        let mut reg = HookRegistry::new();
        reg.register(Appender);
        let s = reg.dispatch_stop("done".into()).await;
        assert_eq!(s, "done [audited]");
    }

    #[tokio::test]
    async fn replace_with_non_string_value_is_json_stringified() {
        struct ObjReplacer;
        #[async_trait]
        impl Hook for ObjReplacer {
            fn name(&self) -> &str {
                "obj"
            }
            async fn handle(&self, _: &HookPayload<'_>) -> HookOutcome {
                HookOutcome::Replace(json!({"k": 1}))
            }
        }
        let mut reg = HookRegistry::new();
        reg.register(ObjReplacer);
        let s = reg
            .dispatch_post_tool_use("X", &json!({}), "raw".into())
            .await;
        assert_eq!(s, r#"{"k":1}"#);
    }

    #[tokio::test]
    async fn empty_registry_passes_through_unchanged() {
        let reg = HookRegistry::new();
        let pre = reg.dispatch_pre_tool_use("X", json!({"a": 1})).await;
        match pre {
            PreToolDecision::Continue { args } => assert_eq!(args["a"], json!(1)),
            _ => panic!("expected Continue"),
        }
        let post = reg
            .dispatch_post_tool_use("X", &json!({}), "r".into())
            .await;
        assert_eq!(post, "r");
        let stop = reg.dispatch_stop("c".into()).await;
        assert_eq!(stop, "c");
    }

    #[test]
    fn len_tracks_registrations() {
        let mut reg = HookRegistry::new();
        let log = Arc::new(Mutex::new(Vec::new()));
        assert_eq!(reg.len(), 0);
        reg.register(Recorder { log: log.clone() });
        assert_eq!(reg.len(), 1);
        reg.register(Recorder { log });
        assert_eq!(reg.len(), 2);
    }
}
