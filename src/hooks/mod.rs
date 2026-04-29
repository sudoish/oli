//! Hook dispatcher — shared between built-in hooks and (Phase 3 step 4)
//! Lua plugin-registered hooks. One trait, one registry, two sources.
//!
//! Hooks are observe-only in this revision: they receive a payload
//! describing the event but cannot short-circuit the agent loop.
//! Mutation via hooks is a Phase 4-or-later concern; today the policy
//! engine is the only veto path.
//!
//! Three event kinds fire from `Agent::run_streaming`:
//! - `PreToolUse` — before the policy check, so a hook sees the model's
//!   intent regardless of whether it ends up running.
//! - `PostToolUse` — after dispatch, with the result string the model
//!   will see (including "policy denied" / "user declined" outcomes).
//! - `Stop` — once the loop returns a final assistant message with no
//!   further tool calls.

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

#[async_trait]
pub trait Hook: Send + Sync {
    /// Display name. `/hooks` (future) and the plugin loader use this
    /// for diagnostics.
    fn name(&self) -> &str;

    /// Handle a payload. Implementations should match on the variant
    /// they care about and ignore the rest.
    async fn handle(&self, payload: &HookPayload<'_>);
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

    pub fn len(&self) -> usize {
        self.hooks.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &dyn Hook> {
        self.hooks.iter().map(|b| b.as_ref())
    }

    /// Fire a payload to every registered hook, sequentially. Errors
    /// don't propagate — a misbehaving hook should not crash the
    /// session. (Plugin-registered hooks should catch their own panics
    /// at the bridge layer.)
    pub async fn dispatch(&self, payload: HookPayload<'_>) {
        for hook in &self.hooks {
            hook.handle(&payload).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::Mutex;

    /// Records every payload it sees for later assertion.
    struct Recorder {
        log: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Hook for Recorder {
        fn name(&self) -> &str {
            "recorder"
        }
        async fn handle(&self, payload: &HookPayload<'_>) {
            let entry = match payload {
                HookPayload::PreToolUse { tool, .. } => format!("pre:{}", tool),
                HookPayload::PostToolUse { tool, result, .. } => {
                    format!("post:{}:{}", tool, result)
                }
                HookPayload::Stop { final_content } => format!("stop:{}", final_content),
            };
            self.log.lock().unwrap().push(entry);
        }
    }

    #[tokio::test]
    async fn registry_fires_each_payload_to_all_hooks_in_order() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut reg = HookRegistry::new();
        reg.register(Recorder { log: log.clone() });
        reg.register(Recorder { log: log.clone() });

        let args = json!({});
        reg.dispatch(HookPayload::PreToolUse {
            tool: "Read",
            args: &args,
        })
        .await;

        let recorded = log.lock().unwrap().clone();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0], "pre:Read");
        assert_eq!(recorded[1], "pre:Read");
    }

    #[tokio::test]
    async fn dispatch_round_trips_each_event_kind() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut reg = HookRegistry::new();
        reg.register(Recorder { log: log.clone() });

        let args = json!({});
        reg.dispatch(HookPayload::PreToolUse {
            tool: "X",
            args: &args,
        })
        .await;
        reg.dispatch(HookPayload::PostToolUse {
            tool: "X",
            args: &args,
            result: "ok",
        })
        .await;
        reg.dispatch(HookPayload::Stop {
            final_content: "done",
        })
        .await;

        let recorded = log.lock().unwrap().clone();
        assert_eq!(recorded, vec!["pre:X", "post:X:ok", "stop:done"]);
    }

    #[tokio::test]
    async fn empty_registry_is_a_noop() {
        let reg = HookRegistry::new();
        let args = json!({});
        // Should not panic.
        reg.dispatch(HookPayload::PreToolUse {
            tool: "X",
            args: &args,
        })
        .await;
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
