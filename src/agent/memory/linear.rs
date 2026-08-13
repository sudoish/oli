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

use std::time::Instant;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::error::{AgentError, Result};
use crate::ledger::{ContextEstimate, Latency, as_ms, estimate::estimate_messages, now_ms};
use crate::providers::ChatRequest;

use super::{CompactContext, CompactionReport, ContextParts, Memory};

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

    async fn snapshot_parts(&self) -> ContextParts {
        ContextParts {
            pinned: self.pinned.clone(),
            summary: self.summary.iter().cloned().collect(),
            recent: self.messages.clone(),
        }
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

    async fn maybe_compact(&mut self, ctx: CompactContext<'_>) -> Result<Option<CompactionReport>> {
        if ctx.next_request_tokens <= ctx.target_tokens {
            return Ok(None);
        }
        if self.messages.len() < MIN_MESSAGES_TO_COMPACT {
            return Ok(None);
        }

        // Aim to drain roughly the older half, but snap forward to the
        // next user-message boundary so a tool_call assistant message and
        // its tool result never get split across the cut.
        let Some(cut) = compaction_cut(&self.messages) else {
            return Ok(None);
        };

        // Prepare from clones. The live state is not changed until the
        // provider succeeds and the candidate summary validates, so an
        // error or cancellation leaves the exact original history intact.
        let older = self.messages[..cut].to_vec();

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

        let prompt_tokens = estimate_messages(&req.messages);
        if prompt_tokens > ctx.hard_limit_tokens as u64 {
            return Err(AgentError::Config(format!(
                "compaction request estimate {prompt_tokens} exceeds hard context limit {}",
                ctx.hard_limit_tokens
            )));
        }
        let estimated = ContextEstimate {
            recent: prompt_tokens,
            total: prompt_tokens,
            ..ContextEstimate::default()
        };
        let started_at_ms = now_ms();
        let model_started = Instant::now();
        let resp = ctx.provider.chat(req).await?;
        let model_ms = as_ms(model_started.elapsed());
        let usage = resp.usage;
        let summary_text = resp
            .message
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if summary_text.is_empty() {
            return Err(AgentError::Provider(
                "compaction returned an empty summary".into(),
            ));
        }

        let candidate_summary = json!({
            "role": "system",
            "content": format!("[Earlier conversation summary]\n{}", summary_text),
        });
        let replaced_tokens = estimate_messages(&older)
            + self
                .summary
                .as_ref()
                .map(|summary| estimate_messages(std::slice::from_ref(summary)))
                .unwrap_or(0);
        let candidate_tokens = estimate_messages(std::slice::from_ref(&candidate_summary));
        if candidate_tokens >= replaced_tokens {
            return Err(AgentError::Provider(format!(
                "compaction summary did not reduce context ({candidate_tokens} >= {replaced_tokens} tokens)"
            )));
        }

        self.messages.drain(..cut);
        self.base += cut;
        self.summary = Some(candidate_summary);

        Ok(Some(CompactionReport {
            started_at_ms,
            estimated,
            usage,
            latency: Latency {
                model_ms,
                ..Latency::default()
            },
        }))
    }
}

fn role_of(m: &Value) -> Option<&str> {
    m.get("role").and_then(|v| v.as_str())
}

fn compaction_cut(messages: &[Value]) -> Option<usize> {
    let half = messages.len() / 2;
    (half..messages.len()).find(|cut| {
        *cut > 0
            && *cut < messages.len()
            && role_of(&messages[*cut]) == Some("user")
            && tool_pairs_are_complete(&messages[..*cut])
            && tool_pairs_are_complete(&messages[*cut..])
    })
}

fn tool_pairs_are_complete(messages: &[Value]) -> bool {
    use std::collections::HashSet;

    let mut pending = HashSet::new();
    for message in messages {
        if role_of(message) == Some("user") && !pending.is_empty() {
            return false;
        }
        if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                let Some(id) = call.get("id").and_then(Value::as_str) else {
                    return false;
                };
                if id.is_empty() || !pending.insert(id) {
                    return false;
                }
            }
        }
        if role_of(message) == Some("tool") {
            let Some(id) = message.get("tool_call_id").and_then(Value::as_str) else {
                return false;
            };
            if !pending.remove(id) {
                return false;
            }
        }
    }
    pending.is_empty()
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
    async fn maybe_compact_skips_at_the_exact_budget_boundary() {
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
            target_tokens: 100,
            hard_limit_tokens: 20_000,
            next_request_tokens: 100,
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
        let report = m
            .maybe_compact(CompactContext {
                provider: &provider,
                model: "x",
                target_tokens: 10,
                hard_limit_tokens: 2_000,
                next_request_tokens: 1_000,
            })
            .await
            .unwrap()
            .expect("a provider-backed compaction must report its accounting");
        assert!(report.estimated.total > 0);
        assert_eq!(report.estimated.total, report.estimated.recent);

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
    async fn failed_compaction_leaves_the_original_history_untouched() {
        struct FailingSummary;
        #[async_trait]
        impl Provider for FailingSummary {
            async fn chat(&self, _: ChatRequest) -> Result<ChatResponse> {
                Err(crate::error::AgentError::Provider("summary failed".into()))
            }
        }

        let mut m = LinearWithCompact::new();
        m.pin(json!({"role":"system","content":"pinned constraint"}))
            .await
            .unwrap();
        for i in 0..6 {
            let role = if i % 2 == 0 { "user" } else { "assistant" };
            m.record(json!({"role": role, "content": format!("msg{i}")}))
                .await
                .unwrap();
        }
        let before = m.snapshot().await;
        let before_len = m.len();

        let error = m
            .maybe_compact(CompactContext {
                provider: &FailingSummary,
                model: "x",
                target_tokens: 10,
                hard_limit_tokens: 2_000,
                next_request_tokens: 1_000,
            })
            .await
            .unwrap_err();

        assert!(error.to_string().contains("summary failed"));
        assert_eq!(m.snapshot().await, before);
        assert_eq!(m.len(), before_len);
    }

    #[tokio::test]
    async fn empty_compaction_output_is_rejected_without_mutating_history() {
        struct EmptySummary;
        #[async_trait]
        impl Provider for EmptySummary {
            async fn chat(&self, _: ChatRequest) -> Result<ChatResponse> {
                Ok(ChatResponse {
                    message: json!({"role":"assistant","content":"   "}),
                    usage: None,
                })
            }
        }

        let mut m = LinearWithCompact::new();
        for i in 0..6 {
            let role = if i % 2 == 0 { "user" } else { "assistant" };
            m.record(json!({"role": role, "content": format!("msg{i}")}))
                .await
                .unwrap();
        }
        let before = m.snapshot().await;

        m.maybe_compact(CompactContext {
            provider: &EmptySummary,
            model: "x",
            target_tokens: 10,
            hard_limit_tokens: 2_000,
            next_request_tokens: 1_000,
        })
        .await
        .unwrap_err();

        assert_eq!(m.snapshot().await, before);
    }

    #[tokio::test]
    async fn a_summary_larger_than_the_history_it_replaces_is_rejected() {
        struct BloatedSummary;
        #[async_trait]
        impl Provider for BloatedSummary {
            async fn chat(&self, _: ChatRequest) -> Result<ChatResponse> {
                Ok(ChatResponse {
                    message: json!({"role":"assistant","content":"x".repeat(4_000)}),
                    usage: None,
                })
            }
        }

        let mut m = LinearWithCompact::new();
        for i in 0..6 {
            let role = if i % 2 == 0 { "user" } else { "assistant" };
            m.record(json!({"role": role, "content": format!("msg{i}")}))
                .await
                .unwrap();
        }
        let before = m.snapshot().await;
        let error = m
            .maybe_compact(CompactContext {
                provider: &BloatedSummary,
                model: "x",
                target_tokens: 10,
                hard_limit_tokens: 10_000,
                next_request_tokens: 1_000,
            })
            .await
            .unwrap_err();

        assert!(error.to_string().contains("did not reduce context"));
        assert_eq!(m.snapshot().await, before);
    }

    #[tokio::test]
    async fn cancelling_compaction_before_the_provider_returns_keeps_history_intact() {
        struct NeverReturns;
        #[async_trait]
        impl Provider for NeverReturns {
            async fn chat(&self, _: ChatRequest) -> Result<ChatResponse> {
                std::future::pending().await
            }
        }

        let mut m = LinearWithCompact::new();
        for i in 0..6 {
            let role = if i % 2 == 0 { "user" } else { "assistant" };
            m.record(json!({"role": role, "content": format!("msg{i}")}))
                .await
                .unwrap();
        }
        let before = m.snapshot().await;
        let provider = NeverReturns;
        {
            let future = m.maybe_compact(CompactContext {
                provider: &provider,
                model: "x",
                target_tokens: 10,
                hard_limit_tokens: 2_000,
                next_request_tokens: 1_000,
            });
            tokio::pin!(future);
            tokio::select! {
                _ = &mut future => panic!("provider unexpectedly completed"),
                _ = tokio::task::yield_now() => {}
            }
        }

        assert_eq!(m.snapshot().await, before);
    }

    #[tokio::test]
    async fn an_incomplete_tool_cycle_is_never_selected_for_compaction() {
        struct ShouldNotCall;
        #[async_trait]
        impl Provider for ShouldNotCall {
            async fn chat(&self, _: ChatRequest) -> Result<ChatResponse> {
                panic!("malformed tool history must not be summarized")
            }
        }

        let mut m = LinearWithCompact::new();
        m.record(json!({"role":"user","content":"u1"}))
            .await
            .unwrap();
        m.record(json!({
            "role":"assistant",
            "tool_calls":[{"id":"missing","type":"function","function":{"name":"X","arguments":"{}"}}]
        }))
        .await
        .unwrap();
        m.record(json!({"role":"assistant","content":"continued"}))
            .await
            .unwrap();
        m.record(json!({"role":"user","content":"u2"}))
            .await
            .unwrap();
        m.record(json!({"role":"assistant","content":"a2"}))
            .await
            .unwrap();

        let result = m
            .maybe_compact(CompactContext {
                provider: &ShouldNotCall,
                model: "x",
                target_tokens: 10,
                hard_limit_tokens: 2_000,
                next_request_tokens: 1_000,
            })
            .await
            .unwrap();
        assert!(result.is_none());
        assert_eq!(m.messages.len(), 5);
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
            hard_limit_tokens: 2_000,
            next_request_tokens: 1_000,
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
            hard_limit_tokens: 2_000,
            next_request_tokens: 1_000,
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
    async fn snapshot_parts_before_compaction_has_no_summary() {
        let mut m = LinearWithCompact::new();
        m.pin(json!({"role":"system","content":"sys"}))
            .await
            .unwrap();
        m.record(json!({"role":"user","content":"a"}))
            .await
            .unwrap();
        m.record(json!({"role":"assistant","content":"b"}))
            .await
            .unwrap();

        let parts = m.snapshot_parts().await;
        assert_eq!(parts.pinned.len(), 1);
        assert!(parts.summary.is_empty());
        assert_eq!(parts.recent.len(), 2);
        assert_eq!(parts.flatten(), m.snapshot().await);
    }

    #[tokio::test]
    async fn snapshot_parts_separates_the_summary_from_live_records_after_compaction() {
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
        m.pin(json!({"role":"system","content":"sys"}))
            .await
            .unwrap();
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
            hard_limit_tokens: 2_000,
            next_request_tokens: 1_000,
        })
        .await
        .unwrap();

        let parts = m.snapshot_parts().await;
        assert_eq!(parts.pinned.len(), 1);
        assert_eq!(parts.summary.len(), 1);
        assert!(
            parts.summary[0]["content"]
                .as_str()
                .unwrap()
                .contains("Earlier conversation summary")
        );
        assert!(!parts.recent.is_empty());
        assert_eq!(parts.flatten(), m.snapshot().await);
    }

    #[tokio::test]
    async fn a_live_system_record_is_not_mistaken_for_the_rolling_summary() {
        // The agent loop used to approximate "is this the summary?" by
        // checking the first non-pinned message's role. A system-role
        // record recorded by the caller defeats that.
        let mut m = LinearWithCompact::new();
        m.pin(json!({"role":"system","content":"sys"}))
            .await
            .unwrap();
        m.record(json!({"role":"system","content":"a live system note"}))
            .await
            .unwrap();
        m.record(json!({"role":"user","content":"u"}))
            .await
            .unwrap();

        let parts = m.snapshot_parts().await;
        assert!(parts.summary.is_empty());
        assert_eq!(parts.recent.len(), 2);
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
            hard_limit_tokens: 20_000,
            next_request_tokens: 5,
        })
        .await
        .unwrap();
        assert_eq!(m.len(), 1);
    }
}
