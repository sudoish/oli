//! `Task` — subagent tool. Spawns a child agent loop with isolated
//! memory and returns only the child's final summary, not its inner tool
//! steps. Useful for delegating well-scoped work ("find all callers of X
//! and summarize") without polluting the parent's context.
//!
//! The same machinery will power plugin `ctx:prompt(...)` in Phase 3
//! step 4 — both go through `SubagentSpawner`. The trait lets us
//! decouple the tool from the binary's startup wiring; tests inject a
//! fixed-output spawner and assert the tool surfaces it correctly.

use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

use crate::error::{Result, ToolError};
use crate::tools::{Tool, ToolContext};

const DEFAULT_MAX_TURNS: usize = 10;

/// Spawns a child agent loop and runs `prompt` to completion. The
/// implementation builds a fresh agent each call, runs it bounded by
/// `max_turns`, and returns whatever the child's final assistant
/// message contained.
#[async_trait]
pub trait SubagentSpawner: Send + Sync {
    async fn spawn(&self, prompt: &str, max_turns: usize) -> Result<String>;
}

pub struct Task {
    spawner: Arc<dyn SubagentSpawner>,
}

impl Task {
    pub fn new(spawner: Arc<dyn SubagentSpawner>) -> Self {
        Self { spawner }
    }
}

#[async_trait]
impl Tool for Task {
    fn name(&self) -> &str {
        "Task"
    }

    fn description(&self) -> &str {
        "Spawn a focused subagent to handle a self-contained subtask. \
         Returns only the subagent's final summary; its tool calls and \
         intermediate reasoning don't pollute the parent conversation. \
         Use for searches, multi-step lookups, and delegated work."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "Self-contained instruction for the subagent. Include any context it needs — the subagent has no view of the parent conversation."
                },
                "max_turns": {
                    "type": "integer",
                    "description": "Optional cap on subagent turns (default 10). Increase only when the task genuinely needs more steps."
                }
            },
            "required": ["prompt"]
        })
    }

    async fn run(&self, args: Value, _ctx: &ToolContext) -> Result<String> {
        let prompt = args.get("prompt").and_then(|v| v.as_str()).ok_or_else(|| {
            ToolError::InvalidArguments {
                tool: "Task".into(),
                detail: "missing `prompt`".into(),
            }
        })?;
        let max_turns = args
            .get("max_turns")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_MAX_TURNS);
        self.spawner.spawn(prompt, max_turns).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Spawner that records what it was asked to do and returns a fixed
    /// answer. Lets tests assert plumbing without spinning a real agent.
    struct StubSpawner {
        seen: Mutex<Vec<(String, usize)>>,
        answer: String,
    }

    #[async_trait]
    impl SubagentSpawner for StubSpawner {
        async fn spawn(&self, prompt: &str, max_turns: usize) -> Result<String> {
            self.seen
                .lock()
                .unwrap()
                .push((prompt.to_string(), max_turns));
            Ok(self.answer.clone())
        }
    }

    #[tokio::test]
    async fn forwards_prompt_to_spawner_and_returns_summary() {
        let spawner = Arc::new(StubSpawner {
            seen: Mutex::new(Vec::new()),
            answer: "child-summary".into(),
        });
        let task = Task::new(spawner.clone());
        let ctx = ToolContext::new();

        let out = task
            .run(json!({"prompt": "find all callers of foo"}), &ctx)
            .await
            .unwrap();
        assert_eq!(out, "child-summary");

        let seen = spawner.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, "find all callers of foo");
        assert_eq!(seen[0].1, DEFAULT_MAX_TURNS);
    }

    #[tokio::test]
    async fn honors_explicit_max_turns_argument() {
        let spawner = Arc::new(StubSpawner {
            seen: Mutex::new(Vec::new()),
            answer: "ok".into(),
        });
        let task = Task::new(spawner.clone());
        let ctx = ToolContext::new();

        task.run(json!({"prompt": "p", "max_turns": 3}), &ctx)
            .await
            .unwrap();
        assert_eq!(spawner.seen.lock().unwrap()[0].1, 3);
    }

    #[tokio::test]
    async fn missing_prompt_returns_invalid_arguments_error() {
        struct NoCall;
        #[async_trait]
        impl SubagentSpawner for NoCall {
            async fn spawn(&self, _: &str, _: usize) -> Result<String> {
                unreachable!("spawner must not be invoked when args invalid")
            }
        }
        let task = Task::new(Arc::new(NoCall));
        let ctx = ToolContext::new();
        let err = task.run(json!({}), &ctx).await.unwrap_err();
        assert!(err.to_string().contains("missing `prompt`"));
    }

    #[tokio::test]
    async fn spawner_error_propagates() {
        struct Failing;
        #[async_trait]
        impl SubagentSpawner for Failing {
            async fn spawn(&self, _: &str, _: usize) -> Result<String> {
                Err(crate::error::AgentError::Provider("boom".into()))
            }
        }
        let task = Task::new(Arc::new(Failing));
        let ctx = ToolContext::new();
        let err = task.run(json!({"prompt": "x"}), &ctx).await.unwrap_err();
        assert!(err.to_string().contains("boom"));
    }
}
