//! `LinearWithCompact` — the default `Memory` strategy.
//!
//! Holds messages in insertion order, pinned content at the head of every
//! snapshot, and an optional rolling summary that absorbs older turns when
//! the conversation outgrows the model's context window.
//!
//! ## Cancellation under compaction
//!
//! `len()` returns a monotonic record counter, not the physical message
//! count. This is what keeps Ctrl-C rollback honest after compaction has
//! run mid-session: the REPL captures `saved_len = memory.len()` before a
//! turn, and `truncate(saved_len)` always rolls back to that logical
//! position, even if compaction has since drained earlier records into the
//! summary. When the rollback target predates anything we still hold
//! verbatim, we drop the live message window and keep the summary —
//! best-effort, but predictable.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::error::Result;
use crate::providers::ChatRequest;

use super::{CompactContext, Memory};

const MIN_MESSAGES_TO_COMPACT: usize = 4;

#[derive(Default)]
pub struct LinearWithCompact {
    pinned: Vec<Value>,
    /// Rolling summary of records that compaction has drained. Sits between
    /// pinned content and live messages in every snapshot.
    summary: Option<Value>,
    /// Live records currently held verbatim. Drained from the front during
    /// compaction; truncated from the back during cancellation.
    messages: Vec<Value>,
    /// Total `record` calls since the last `clear()`. Monotonic except for
    /// `truncate` and `clear`. Unaffected by compaction.
    record_count: usize,
    /// `record_count` value at the time `messages[0]` was first recorded.
    /// After compaction drains k items from the front, `base` advances by
    /// k, mapping logical positions back to physical indices.
    base: usize,
}

impl LinearWithCompact {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Memory for LinearWithCompact {
    async fn record(&mut self, message: Value) -> Result<()> {
        self.messages.push(message);
        self.record_count += 1;
        Ok(())
    }

    async fn snapshot(&self) -> Vec<Value> {
        let cap = self.pinned.len() + self.messages.len() + 1;
        let mut out = Vec::with_capacity(cap);
        out.extend(self.pinned.iter().cloned());
        if let Some(s) = &self.summary {
            out.push(s.clone());
        }
        out.extend(self.messages.iter().cloned());
        out
    }

    async fn pin(&mut self, message: Value) -> Result<()> {
        self.pinned.push(message);
        Ok(())
    }

    async fn pinned(&self) -> Vec<Value> {
        self.pinned.clone()
    }

    fn len(&self) -> usize {
        self.record_count
    }

    async fn truncate(&mut self, n: usize) -> Result<()> {
        if n >= self.record_count {
            return Ok(());
        }
        if n >= self.base {
            self.messages.truncate(n - self.base);
        } else {
            // Truncate target predates our verbatim window. We can't
            // restore drained records — drop the live window and slide
            // base back to match. Summary is left intact since it's the
            // closest thing we have to those older records.
            self.messages.clear();
            self.base = n;
        }
        self.record_count = n;
        Ok(())
    }

    async fn clear(&mut self) -> Result<()> {
        self.messages.clear();
        self.summary = None;
        self.record_count = 0;
        self.base = 0;
        Ok(())
    }

    async fn maybe_compact(&mut self, ctx: CompactContext<'_>) -> Result<()> {
        if ctx.current_tokens <= ctx.target_tokens {
            return Ok(());
        }
        if self.messages.len() < MIN_MESSAGES_TO_COMPACT {
            return Ok(());
        }

        // Aim to drain roughly the older half, but snap forward to the
        // next user-message boundary so a tool_call assistant message and
        // its tool result never get split across the cut.
        let half = self.messages.len() / 2;
        let mut cut = half;
        while cut < self.messages.len() && role_of(&self.messages[cut]) != Some("user") {
            cut += 1;
        }
        if cut == 0 || cut >= self.messages.len() {
            return Ok(());
        }

        let older: Vec<Value> = self.messages.drain(..cut).collect();
        self.base += older.len();

        let transcript = render_for_summary(&older);
        let prior = self
            .summary
            .as_ref()
            .and_then(|s| s.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();

        let user_prompt = if prior.is_empty() {
            format!(
                "Summarize the following conversation transcript so an agent can continue without losing critical decisions, file paths, errors, identifiers, or user preferences. Be concise but specific.\n\n{}",
                transcript
            )
        } else {
            format!(
                "Continue this conversation summary, integrating the new transcript that follows. Preserve concrete identifiers verbatim.\n\nExisting summary:\n{}\n\nNew transcript:\n{}",
                prior, transcript
            )
        };

        let req = ChatRequest {
            model: ctx.model.to_string(),
            messages: vec![
                json!({
                    "role": "system",
                    "content": "You compact conversation histories. Output only the new summary; no preamble, no apologies."
                }),
                json!({"role": "user", "content": user_prompt}),
            ],
            tools: vec![],
        };

        let resp = ctx.provider.chat(req).await?;
        let summary_text = resp
            .message
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        self.summary = Some(json!({
            "role": "system",
            "content": format!("[Earlier conversation summary]\n{}", summary_text),
        }));

        Ok(())
    }
}

fn role_of(m: &Value) -> Option<&str> {
    m.get("role").and_then(|v| v.as_str())
}

fn render_for_summary(msgs: &[Value]) -> String {
    let mut out = String::new();
    for m in msgs {
        let role = role_of(m).unwrap_or("?");
        if let Some(content) = m.get("content").and_then(|v| v.as_str()) {
            if !content.is_empty() {
                out.push_str(&format!("[{}] {}\n", role, content));
                continue;
            }
        }
        if let Some(tcs) = m.get("tool_calls").and_then(|v| v.as_array()) {
            let names: Vec<&str> = tcs
                .iter()
                .filter_map(|t| {
                    t.get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                })
                .collect();
            out.push_str(&format!("[{}] tool_calls: {}\n", role, names.join(", ")));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{ChatResponse, Provider};
    use async_trait::async_trait;
    use serde_json::json;

    #[tokio::test]
    async fn record_and_snapshot_round_trip_in_order() {
        let mut m = LinearWithCompact::new();
        m.record(json!({"role":"user","content":"a"}))
            .await
            .unwrap();
        m.record(json!({"role":"assistant","content":"b"}))
            .await
            .unwrap();
        let snap = m.snapshot().await;
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0]["content"], "a");
        assert_eq!(snap[1]["content"], "b");
    }

    #[tokio::test]
    async fn pinned_appears_before_messages_in_snapshot() {
        let mut m = LinearWithCompact::new();
        m.pin(json!({"role":"system","content":"sys"}))
            .await
            .unwrap();
        m.record(json!({"role":"user","content":"u"}))
            .await
            .unwrap();
        let snap = m.snapshot().await;
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0]["role"], "system");
        assert_eq!(snap[1]["role"], "user");
    }

    #[tokio::test]
    async fn clear_drops_messages_summary_and_counter_but_not_pinned() {
        let mut m = LinearWithCompact::new();
        m.pin(json!({"role":"system","content":"sys"}))
            .await
            .unwrap();
        m.record(json!({"role":"user","content":"u"}))
            .await
            .unwrap();
        m.clear().await.unwrap();
        assert_eq!(m.len(), 0);
        let snap = m.snapshot().await;
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0]["role"], "system");
    }

    #[tokio::test]
    async fn truncate_rolls_back_recorded_messages_only() {
        let mut m = LinearWithCompact::new();
        m.pin(json!({"role":"system","content":"sys"}))
            .await
            .unwrap();
        m.record(json!({"role":"user","content":"a"}))
            .await
            .unwrap();
        m.record(json!({"role":"user","content":"b"}))
            .await
            .unwrap();
        m.record(json!({"role":"user","content":"c"}))
            .await
            .unwrap();
        assert_eq!(m.len(), 3);

        m.truncate(1).await.unwrap();
        assert_eq!(m.len(), 1);
        let snap = m.snapshot().await;
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0]["role"], "system");
        assert_eq!(snap[1]["content"], "a");
    }

    #[tokio::test]
    async fn len_excludes_pinned_messages() {
        let mut m = LinearWithCompact::new();
        m.pin(json!({"role":"system","content":"sys"}))
            .await
            .unwrap();
        m.pin(json!({"role":"system","content":"sys2"}))
            .await
            .unwrap();
        m.record(json!({"role":"user","content":"u"}))
            .await
            .unwrap();
        assert_eq!(m.len(), 1);
    }

    #[tokio::test]
    async fn maybe_compact_skips_when_within_budget() {
        struct ShouldNotCall;
        #[async_trait]
        impl Provider for ShouldNotCall {
            async fn chat(&self, _: ChatRequest) -> Result<ChatResponse> {
                unreachable!("provider should not be called when within budget")
            }
        }

        let mut m = LinearWithCompact::new();
        for i in 0..6 {
            let role = if i % 2 == 0 { "user" } else { "assistant" };
            m.record(json!({"role": role, "content": format!("msg{}", i)}))
                .await
                .unwrap();
        }
        let provider = ShouldNotCall;
        m.maybe_compact(CompactContext {
            provider: &provider,
            model: "x",
            target_tokens: 10_000,
            current_tokens: 100,
        })
        .await
        .unwrap();
        // No summary inserted; live messages all preserved.
        let snap = m.snapshot().await;
        assert_eq!(snap.len(), 6);
    }

    #[tokio::test]
    async fn maybe_compact_summarizes_when_over_budget() {
        struct StubSummary;
        #[async_trait]
        impl Provider for StubSummary {
            async fn chat(&self, _: ChatRequest) -> Result<ChatResponse> {
                Ok(ChatResponse {
                    message: json!({"role":"assistant","content":"COMPACTED"}),
                    usage: None,
                })
            }
        }

        let mut m = LinearWithCompact::new();
        for i in 0..6 {
            let role = if i % 2 == 0 { "user" } else { "assistant" };
            m.record(json!({"role": role, "content": format!("msg{}", i)}))
                .await
                .unwrap();
        }
        let provider = StubSummary;
        m.maybe_compact(CompactContext {
            provider: &provider,
            model: "x",
            target_tokens: 10,
            current_tokens: 1_000,
        })
        .await
        .unwrap();

        // record_count stays at 6 — compaction is not visible at the
        // logical-position level, only at the physical one.
        assert_eq!(m.len(), 6);

        // Snapshot now has the synthesized summary message.
        let snap = m.snapshot().await;
        let has_summary = snap.iter().any(|x| {
            x["role"] == "system"
                && x["content"]
                    .as_str()
                    .map(|s| s.contains("Earlier conversation summary"))
                    .unwrap_or(false)
        });
        assert!(has_summary, "summary should be in snapshot: {:?}", snap);
    }

    #[tokio::test]
    async fn maybe_compact_keeps_tool_call_pairs_together() {
        // Sequence: user, assistant(tool_call), tool, assistant, user, assistant.
        // Half-point lands at index 3. cut should snap forward to the next
        // user (index 4), so the assistant(tool_call) at index 1 + tool at
        // index 2 stay together — both go into the summary.
        struct StubSummary;
        #[async_trait]
        impl Provider for StubSummary {
            async fn chat(&self, _: ChatRequest) -> Result<ChatResponse> {
                Ok(ChatResponse {
                    message: json!({"role":"assistant","content":"S"}),
                    usage: None,
                })
            }
        }

        let mut m = LinearWithCompact::new();
        m.record(json!({"role":"user","content":"u1"}))
            .await
            .unwrap();
        m.record(json!({
            "role":"assistant","content":null,
            "tool_calls":[{"id":"c1","type":"function","function":{"name":"X","arguments":"{}"}}]
        }))
        .await
        .unwrap();
        m.record(json!({"role":"tool","tool_call_id":"c1","content":"r1"}))
            .await
            .unwrap();
        m.record(json!({"role":"assistant","content":"a1"}))
            .await
            .unwrap();
        m.record(json!({"role":"user","content":"u2"}))
            .await
            .unwrap();
        m.record(json!({"role":"assistant","content":"a2"}))
            .await
            .unwrap();

        let provider = StubSummary;
        m.maybe_compact(CompactContext {
            provider: &provider,
            model: "x",
            target_tokens: 10,
            current_tokens: 1_000,
        })
        .await
        .unwrap();

        // Live window should start at the second user message — the cut
        // landed there, not in the middle of the tool_call/tool pair.
        let live: Vec<&Value> = m.messages.iter().collect();
        assert_eq!(live.len(), 2);
        assert_eq!(live[0]["content"], "u2");
        assert_eq!(live[1]["content"], "a2");
    }

    #[tokio::test]
    async fn truncate_after_compaction_below_base_keeps_summary_drops_live() {
        struct StubSummary;
        #[async_trait]
        impl Provider for StubSummary {
            async fn chat(&self, _: ChatRequest) -> Result<ChatResponse> {
                Ok(ChatResponse {
                    message: json!({"role":"assistant","content":"S"}),
                    usage: None,
                })
            }
        }

        let mut m = LinearWithCompact::new();
        for i in 0..6 {
            let role = if i % 2 == 0 { "user" } else { "assistant" };
            m.record(json!({"role": role, "content": format!("msg{}", i)}))
                .await
                .unwrap();
        }
        let provider = StubSummary;
        m.maybe_compact(CompactContext {
            provider: &provider,
            model: "x",
            target_tokens: 10,
            current_tokens: 1_000,
        })
        .await
        .unwrap();
        assert_eq!(m.len(), 6);

        // Roll back to logical position 1 — predates the kept window.
        m.truncate(1).await.unwrap();
        assert_eq!(m.len(), 1);
        let snap = m.snapshot().await;
        // pinned (none) + summary + nothing = 1.
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0]["role"], "system");
        assert!(
            snap[0]["content"]
                .as_str()
                .unwrap()
                .contains("Earlier conversation summary")
        );
    }

    #[tokio::test]
    async fn default_maybe_compact_is_noop_when_target_unsatisfied() {
        // The trait default would unconditionally return Ok; verify
        // LinearWithCompact's override does the same when budget is fine,
        // without calling the provider.
        struct ShouldNotCall;
        #[async_trait]
        impl Provider for ShouldNotCall {
            async fn chat(&self, _: ChatRequest) -> Result<ChatResponse> {
                unreachable!("provider must not be called when not over budget")
            }
        }
        let mut m = LinearWithCompact::new();
        m.record(json!({"role":"user","content":"u"}))
            .await
            .unwrap();

        let provider = ShouldNotCall;
        m.maybe_compact(CompactContext {
            provider: &provider,
            model: "x",
            target_tokens: 10_000,
            current_tokens: 5,
        })
        .await
        .unwrap();
        assert_eq!(m.len(), 1);
    }
}
