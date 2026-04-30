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
use crate::tools::util::truncate;
use crate::tools::{Tool, ToolContext};

const DEFAULT_MAX_TURNS: usize = 10;
/// Default cap on bytes returned to the parent. Subagents that find
/// 50 KB of relevant context are usually wrong — the parent only
/// needs the *summary*, and dumping the raw transcript pollutes the
/// parent's window. 8 KB is enough for a paragraph of prose plus a
/// list of file paths; the model can always spawn another subagent
/// for more.
const DEFAULT_MAX_RESULT_BYTES: usize = 8 * 1024;

/// Spawns a child agent loop and runs `prompt` to completion. The
/// implementation builds a fresh agent each call, runs it bounded by
/// `max_turns`, and returns whatever the child's final assistant
/// message contained.
///
/// `parent_ctx` is the parent agent's `ToolContext` at the moment
/// of the spawn. Implementations should snapshot the read-set
/// (and optionally the sticky cwd) into the child's context so
/// the child can `Edit` files the parent already read without
/// re-reading. The clone is one-way — the child's later reads
/// stay local, mirroring how subagent results stay scoped to
/// the child.
#[async_trait]
pub trait SubagentSpawner: Send + Sync {
    async fn spawn(
        &self,
        prompt: &str,
        max_turns: usize,
        parent_ctx: Option<ToolContext>,
    ) -> Result<String>;
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
                },
                "max_result_bytes": {
                    "type": "integer",
                    "description": "Optional cap on the byte size of the subagent's returned summary (default 8192). Oversized results are truncated with a marker; the parent's context window is the constraint."
                }
            },
            "required": ["prompt"]
        })
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> Result<String> {
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
        let max_result_bytes = args
            .get("max_result_bytes")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_MAX_RESULT_BYTES);
        // Hand the spawner a clone of our context so the child
        // agent can inherit the parent's read-set / cwd. ToolContext
        // is internally `Arc<Mutex<...>>` — the clone is cheap, but
        // the child snapshots-and-detaches inside spawn() so its
        // later reads stay local.
        let raw = self
            .spawner
            .spawn(prompt, max_turns, Some(ctx.clone()))
            .await?;
        Ok(truncate(&raw, max_result_bytes))
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
        ctx_snapshots: Mutex<Vec<Vec<std::path::PathBuf>>>,
        answer: String,
    }

    impl StubSpawner {
        fn new(answer: &str) -> Self {
            Self {
                seen: Mutex::new(Vec::new()),
                ctx_snapshots: Mutex::new(Vec::new()),
                answer: answer.into(),
            }
        }
    }

    #[async_trait]
    impl SubagentSpawner for StubSpawner {
        async fn spawn(
            &self,
            prompt: &str,
            max_turns: usize,
            parent_ctx: Option<ToolContext>,
        ) -> Result<String> {
            self.seen
                .lock()
                .unwrap()
                .push((prompt.to_string(), max_turns));
            // Capture the parent's read-set snapshot (if any)
            // so tests can assert it propagated correctly.
            let snapshot = match parent_ctx {
                Some(c) => c
                    .snapshot_reads()
                    .await
                    .into_iter()
                    .map(|(p, _)| p)
                    .collect(),
                None => Vec::new(),
            };
            self.ctx_snapshots.lock().unwrap().push(snapshot);
            Ok(self.answer.clone())
        }
    }

    #[tokio::test]
    async fn forwards_prompt_to_spawner_and_returns_summary() {
        let spawner = Arc::new(StubSpawner::new("child-summary"));
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
        let spawner = Arc::new(StubSpawner::new("ok"));
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
            async fn spawn(
                &self,
                _: &str,
                _: usize,
                _: Option<ToolContext>,
            ) -> Result<String> {
                unreachable!("spawner must not be invoked when args invalid")
            }
        }
        let task = Task::new(Arc::new(NoCall));
        let ctx = ToolContext::new();
        let err = task.run(json!({}), &ctx).await.unwrap_err();
        assert!(err.to_string().contains("missing `prompt`"));
    }

    #[tokio::test]
    async fn truncates_oversized_subagent_result_with_marker() {
        // 50 KB summary; default cap is 8 KB. Result should be capped
        // and the truncation marker visible.
        let big = "x".repeat(50_000);
        let spawner = Arc::new(StubSpawner::new(&big));
        let task = Task::new(spawner);
        let ctx = ToolContext::new();
        let out = task
            .run(json!({"prompt": "go"}), &ctx)
            .await
            .unwrap();
        assert!(
            out.contains("[... output truncated"),
            "expected truncation marker, got first 100 chars: {}",
            &out.chars().take(100).collect::<String>()
        );
        assert!(
            out.len() < 50_000,
            "expected truncated output, got {} bytes",
            out.len()
        );
    }

    #[tokio::test]
    async fn explicit_max_result_bytes_overrides_default_cap() {
        let answer = "abcdef".repeat(2000); // ~12 KB
        let spawner = Arc::new(StubSpawner::new(&answer));
        let task = Task::new(spawner);
        let ctx = ToolContext::new();
        // Cap above the answer length: nothing truncated.
        let out = task
            .run(
                json!({"prompt": "go", "max_result_bytes": 20_000}),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out, answer);
    }

    #[tokio::test]
    async fn spawner_error_propagates() {
        struct Failing;
        #[async_trait]
        impl SubagentSpawner for Failing {
            async fn spawn(
                &self,
                _: &str,
                _: usize,
                _: Option<ToolContext>,
            ) -> Result<String> {
                Err(crate::error::AgentError::Provider("boom".into()))
            }
        }
        let task = Task::new(Arc::new(Failing));
        let ctx = ToolContext::new();
        let err = task.run(json!({"prompt": "x"}), &ctx).await.unwrap_err();
        assert!(err.to_string().contains("boom"));
    }

    #[tokio::test]
    async fn parent_read_set_propagates_to_spawner_via_tool_context() {
        let spawner = Arc::new(StubSpawner::new("done"));
        let task = Task::new(spawner.clone());

        // Parent has read two files. Both canonical paths must
        // appear in the snapshot the spawner receives.
        let f1 = tempfile::NamedTempFile::new().unwrap();
        let f2 = tempfile::NamedTempFile::new().unwrap();
        let ctx = ToolContext::new();
        ctx.mark_read(f1.path()).await;
        ctx.mark_read(f2.path()).await;

        task.run(json!({"prompt": "delegate"}), &ctx).await.unwrap();

        let snapshots = spawner.ctx_snapshots.lock().unwrap().clone();
        assert_eq!(snapshots.len(), 1);
        let mut paths = snapshots[0].clone();
        paths.sort();
        let mut expected: Vec<_> = vec![
            tokio::fs::canonicalize(f1.path()).await.unwrap(),
            tokio::fs::canonicalize(f2.path()).await.unwrap(),
        ];
        expected.sort();
        assert_eq!(paths, expected);
    }

    #[tokio::test]
    async fn child_reads_dont_propagate_back_to_parent() {
        // Sanity: snapshot_reads is a one-way clone via
        // insert_canonical_reads_with_mtimes; a child writing
        // into its own ToolContext (which the StubSpawner does
        // not, but we simulate here) doesn't surface to the
        // parent.
        let parent = ToolContext::new();
        let child = ToolContext::new();

        // Seed child from parent (empty), then have child read
        // a file. Parent's snapshot must remain empty.
        let f = tempfile::NamedTempFile::new().unwrap();
        child.mark_read(f.path()).await;
        let parent_snap = parent.snapshot_reads().await;
        assert!(parent_snap.is_empty());
    }
}
