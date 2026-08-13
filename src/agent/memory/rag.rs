// `EmbeddingRagMemory` is a public extension point. The default
// binary wires `LinearWithCompact`; tests and downstream embedders
// swap this in via `Agent::with_memory`. Silence dead-code warnings
// for the unused-by-default surface.
#![allow(dead_code)]

//! `EmbeddingRagMemory` — embedding-retrieval memory strategy.
//!
//! Where `LinearWithCompact` summarizes older turns under token
//! pressure, this strategy keeps the full transcript indexed by
//! embedding and serves the model only what's relevant to the active
//! query plus a small recent window. Snapshots are bounded by
//! construction (pinned + top-k retrieved + recent N), so 200-turn
//! sessions don't drift the request size up over time.
//!
//! ## What gets embedded
//!
//! Only messages with natural standalone text:
//! - `user` with a string `content`.
//! - `assistant` with a string `content` and no `tool_calls`.
//!
//! Assistant messages that are pure tool dispatches and `tool` result
//! messages aren't embedded — pulling one without its pair would
//! produce an incoherent transcript. The recent-window suffix is
//! always emitted verbatim, so the most recent tool pairs do reach
//! the model intact.
//!
//! ## Snapshot order
//!
//! 1. Pinned (system prompt).
//! 2. Top-k retrieved messages, sorted by original insertion index so
//!    the retrieved bits appear in chronological order.
//! 3. Recent window, verbatim, in insertion order.
//!
//! ## Retrieval query
//!
//! The query is the most recent embeddable message (typically the
//! latest user turn). If no message in the transcript has an
//! embedding, retrieval is skipped entirely.

use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

use crate::error::{AgentError, Result};

use super::{CompactContext, Memory};

const DEFAULT_RECENT_N: usize = 4;
const DEFAULT_TOP_K: usize = 8;

/// Pluggable embedding backend. The strategy invokes `embed` once per
/// recorded message and once per snapshot for the query. An impl
/// against Ollama's `/api/embeddings` is shipped alongside; tests
/// inject a deterministic stub.
#[async_trait]
pub trait Embedder: Send + Sync {
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;
}

struct RecordedMessage {
    msg: Value,
    /// `None` when the message has no embeddable text (assistant tool
    /// dispatches, tool results) or when the embedder errored at
    /// record time. Such messages are still recorded — they're not
    /// retrieval candidates, but they remain in the recent window.
    embedding: Option<Vec<f32>>,
}

pub struct EmbeddingRagMemory {
    embedder: Arc<dyn Embedder>,
    pinned: Vec<Value>,
    messages: Vec<RecordedMessage>,
    recent_n: usize,
    top_k: usize,
}

impl EmbeddingRagMemory {
    pub fn new(embedder: Arc<dyn Embedder>) -> Self {
        Self {
            embedder,
            pinned: Vec::new(),
            messages: Vec::new(),
            recent_n: DEFAULT_RECENT_N,
            top_k: DEFAULT_TOP_K,
        }
    }

    /// Override the recent-window size. Smaller = more aggressive
    /// retrieval-only context; larger = more verbatim tail. Default 4.
    pub fn with_recent_window(mut self, n: usize) -> Self {
        self.recent_n = n;
        self
    }

    /// Override how many similarity-retrieved messages join the
    /// snapshot. Default 8.
    pub fn with_top_k(mut self, k: usize) -> Self {
        self.top_k = k;
        self
    }
}

#[async_trait]
impl Memory for EmbeddingRagMemory {
    async fn record(&mut self, message: Value) -> Result<()> {
        let embedding = match extract_embedding_text(&message) {
            Some(text) => self.embedder.embed(&text).await.ok(),
            None => None,
        };
        self.messages.push(RecordedMessage {
            msg: message,
            embedding,
        });
        Ok(())
    }

    async fn snapshot(&self) -> Vec<Value> {
        let mut out: Vec<Value> = self.pinned.clone();

        let total = self.messages.len();
        let recent_start = total.saturating_sub(self.recent_n);

        // Pick a query: the most recent embeddable message (typically
        // the latest user turn). If nothing is embeddable, skip
        // retrieval and fall through to the recent-window-only view.
        let query_text = self
            .messages
            .iter()
            .rev()
            .find_map(|m| extract_embedding_text(&m.msg));

        if let Some(query) = query_text {
            if let Ok(query_emb) = self.embedder.embed(&query).await {
                let mut scored: Vec<(usize, f32)> = self
                    .messages
                    .iter()
                    .take(recent_start)
                    .enumerate()
                    .filter_map(|(i, m)| {
                        m.embedding
                            .as_ref()
                            .map(|e| (i, cosine_similarity(e, &query_emb)))
                    })
                    .collect();
                scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                scored.truncate(self.top_k);
                // Restore chronological order so the model reads the
                // retrieved snippets in the order they happened.
                scored.sort_by_key(|(i, _)| *i);
                for (i, _) in scored {
                    out.push(self.messages[i].msg.clone());
                }
            }
        }

        for m in &self.messages[recent_start..] {
            out.push(m.msg.clone());
        }
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
        self.messages.len()
    }

    async fn truncate(&mut self, n: usize) -> Result<()> {
        self.messages.truncate(n);
        Ok(())
    }

    async fn clear(&mut self) -> Result<()> {
        self.messages.clear();
        Ok(())
    }

    /// RAG keeps the full transcript indexed; growth is bounded at
    /// snapshot time, not via summarization. This is a no-op.
    async fn maybe_compact(
        &mut self,
        _ctx: CompactContext<'_>,
    ) -> Result<Option<super::CompactionReport>> {
        Ok(None)
    }
}

/// Pull the embedding-eligible text out of a message. Returns `None`
/// for messages we deliberately skip (assistant tool dispatches, tool
/// results) and for messages whose `content` isn't a plain string.
fn extract_embedding_text(message: &Value) -> Option<String> {
    let role = message.get("role")?.as_str()?;
    match role {
        "user" => message
            .get("content")
            .and_then(|c| c.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from),
        "assistant" => {
            // Skip pure tool-dispatch turns; their text is just JSON
            // shape and the paired tool result lives elsewhere.
            if message.get("tool_calls").is_some() {
                return None;
            }
            message
                .get("content")
                .and_then(|c| c.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from)
        }
        _ => None,
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    let denom = (na.sqrt()) * (nb.sqrt());
    if denom == 0.0 { 0.0 } else { dot / denom }
}

/// Ollama embedding backend. Hits the local Ollama daemon's
/// `/api/embeddings` endpoint with the configured model. Default
/// model is `nomic-embed-text`, which runs on the same daemon the
/// agent talks to for chat — no extra service to host.
pub struct OllamaEmbedder {
    client: reqwest::Client,
    base_url: String,
    model: String,
}

impl OllamaEmbedder {
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            model: model.into(),
        }
    }

    /// Construct against the conventional local Ollama port with
    /// `nomic-embed-text` as the model.
    #[allow(dead_code)]
    pub fn local_default() -> Self {
        Self::new("http://localhost:11434", "nomic-embed-text")
    }
}

#[async_trait]
impl Embedder for OllamaEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let payload = json!({ "model": self.model, "prompt": text });
        let resp = self
            .client
            .post(format!("{}/api/embeddings", self.base_url))
            .json(&payload)
            .send()
            .await
            .map_err(|e| AgentError::Provider(format!("ollama embed request: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(AgentError::Provider(format!(
                "ollama embed {}: {}",
                status, text
            )));
        }

        let v: Value = resp
            .json()
            .await
            .map_err(|e| AgentError::Provider(format!("ollama embed parse: {}", e)))?;
        parse_embedding_response(&v)
    }
}

fn parse_embedding_response(v: &Value) -> Result<Vec<f32>> {
    let arr = v
        .get("embedding")
        .and_then(|x| x.as_array())
        .ok_or_else(|| {
            AgentError::Provider("embedding response missing `embedding` array".into())
        })?;
    Ok(arr
        .iter()
        .filter_map(|x| x.as_f64().map(|f| f as f32))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stub embedder: returns a 4-dim vector counting occurrences of a
    /// fixed keyword set. Enough determinism that retrieval against a
    /// known-keyword query is predictable in tests.
    struct KeywordEmbedder;

    #[async_trait]
    impl Embedder for KeywordEmbedder {
        async fn embed(&self, text: &str) -> Result<Vec<f32>> {
            let count = |needle: &str| text.matches(needle).count() as f32;
            Ok(vec![
                count("alpha"),
                count("beta"),
                count("gamma"),
                count("delta"),
            ])
        }
    }

    #[test]
    fn cosine_similarity_basics() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        let c = vec![0.0, 1.0, 0.0];
        // Same direction → 1.0.
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 1e-6);
        // Orthogonal → 0.0.
        assert!(cosine_similarity(&a, &c).abs() < 1e-6);
        // Empty → 0.
        assert_eq!(cosine_similarity(&[], &a), 0.0);
        // Mismatched length → 0.
        assert_eq!(cosine_similarity(&a, &[1.0, 2.0]), 0.0);
    }

    #[test]
    fn extract_embedding_text_skips_tool_calls_and_tool_messages() {
        let user = json!({"role":"user","content":"hi"});
        assert_eq!(extract_embedding_text(&user).unwrap(), "hi");

        let asst_text = json!({"role":"assistant","content":"sure"});
        assert_eq!(extract_embedding_text(&asst_text).unwrap(), "sure");

        let asst_tool = json!({
            "role":"assistant",
            "content": null,
            "tool_calls":[{"id":"c1","type":"function","function":{"name":"X","arguments":"{}"}}]
        });
        assert!(extract_embedding_text(&asst_tool).is_none());

        let tool_result = json!({"role":"tool","tool_call_id":"c1","content":"output"});
        assert!(extract_embedding_text(&tool_result).is_none());

        let empty_user = json!({"role":"user","content":""});
        assert!(extract_embedding_text(&empty_user).is_none());
    }

    #[test]
    fn parse_embedding_response_extracts_vector() {
        let v = json!({"embedding": [0.1, 0.2, 0.3]});
        let out = parse_embedding_response(&v).unwrap();
        assert_eq!(out.len(), 3);
        assert!((out[0] - 0.1).abs() < 1e-5);
    }

    #[test]
    fn parse_embedding_response_errors_when_missing() {
        let v = json!({"oops": []});
        assert!(parse_embedding_response(&v).is_err());
    }

    #[tokio::test]
    async fn snapshot_includes_pinned_then_recent_when_no_retrieval_candidates() {
        let mut m = EmbeddingRagMemory::new(Arc::new(KeywordEmbedder)).with_recent_window(2);
        m.pin(json!({"role":"system","content":"sys"}))
            .await
            .unwrap();
        m.record(json!({"role":"user","content":"a"}))
            .await
            .unwrap();
        m.record(json!({"role":"user","content":"b"}))
            .await
            .unwrap();

        let snap = m.snapshot().await;
        // pinned (1) + retrieved-from-prefix (none, recent_start=0) + recent (2) = 3.
        // recent_start = max(0, 2-2) = 0 → no retrieval candidates,
        // straight to recent.
        assert_eq!(snap.len(), 3);
        assert_eq!(snap[0]["role"], "system");
        assert_eq!(snap[1]["content"], "a");
        assert_eq!(snap[2]["content"], "b");
    }

    #[tokio::test]
    async fn snapshot_retrieves_message_relevant_to_latest_query() {
        // 20 user messages: some about alpha, some about beta, with the
        // latest asking about alpha. Recent window = 1 (just the
        // query); retrieval should pull alpha-keyed earlier messages
        // even though they're far in the past.
        let mut m = EmbeddingRagMemory::new(Arc::new(KeywordEmbedder))
            .with_recent_window(1)
            .with_top_k(2);
        for i in 0..20 {
            let body = if i % 5 == 0 {
                format!("turn {}: alpha facts about widgets", i)
            } else if i % 5 == 1 {
                format!("turn {}: beta facts about gizmos", i)
            } else {
                format!("turn {}: filler text", i)
            };
            m.record(json!({"role":"user","content": body}))
                .await
                .unwrap();
        }
        // Latest message is the query.
        m.record(json!({"role":"user","content":"what was the alpha fact?"}))
            .await
            .unwrap();

        let snap = m.snapshot().await;
        // Should contain at least one message mentioning "alpha facts"
        // — the retrieval pulled it out of the prefix.
        let combined: String = snap
            .iter()
            .filter_map(|v| v.get("content").and_then(|c| c.as_str()).map(String::from))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            combined.contains("alpha facts"),
            "expected retrieved alpha message in snapshot:\n{}",
            combined
        );
        // And the query itself is present (recent window).
        assert!(combined.contains("what was the alpha fact"));
        // We bounded the snapshot: pinned(0) + top_k(2) + recent(1) ≤ 3.
        assert!(snap.len() <= 3, "snapshot too large: {}", snap.len());
    }

    #[tokio::test]
    async fn snapshot_size_stays_bounded_across_a_long_session() {
        // The headline RAG promise: the request size doesn't grow with
        // session length. Record 200 messages with a recent window of
        // 4 and top_k of 8; snapshot should stay ≤ 12 messages.
        let mut m = EmbeddingRagMemory::new(Arc::new(KeywordEmbedder))
            .with_recent_window(4)
            .with_top_k(8);
        m.pin(json!({"role":"system","content":"sys"}))
            .await
            .unwrap();
        for i in 0..200 {
            let body = format!("turn {}: alpha keyword for retrieval", i);
            m.record(json!({"role":"user","content": body}))
                .await
                .unwrap();
        }
        let snap = m.snapshot().await;
        // pinned(1) + top_k(8) + recent(4) = 13 max.
        assert!(
            snap.len() <= 13,
            "snapshot grew unbounded: {} messages after 200 turns",
            snap.len()
        );
    }

    #[tokio::test]
    async fn retrieved_messages_appear_in_chronological_order() {
        let mut m = EmbeddingRagMemory::new(Arc::new(KeywordEmbedder))
            .with_recent_window(1)
            .with_top_k(3);
        m.record(json!({"role":"user","content":"turn 0 alpha"}))
            .await
            .unwrap();
        m.record(json!({"role":"user","content":"turn 1 alpha"}))
            .await
            .unwrap();
        m.record(json!({"role":"user","content":"turn 2 filler"}))
            .await
            .unwrap();
        m.record(json!({"role":"user","content":"turn 3 alpha"}))
            .await
            .unwrap();
        m.record(json!({"role":"user","content":"query about alpha"}))
            .await
            .unwrap();

        let snap = m.snapshot().await;
        let alpha_indices: Vec<usize> = snap
            .iter()
            .filter_map(|v| v.get("content").and_then(|c| c.as_str()))
            .enumerate()
            .filter(|(_, c)| c.starts_with("turn") && c.contains("alpha"))
            .map(|(i, _)| i)
            .collect();
        // The three retrieved alpha messages should be in turn order
        // (0 < 1 < 3) — i.e. their position in the snapshot is
        // increasing in original-record order.
        assert_eq!(
            alpha_indices.len(),
            3,
            "expected three alpha turns retrieved, got snap: {:?}",
            snap
        );
        let order: Vec<&str> = snap
            .iter()
            .filter_map(|v| v.get("content").and_then(|c| c.as_str()))
            .filter(|c| c.starts_with("turn") && c.contains("alpha"))
            .collect();
        assert_eq!(order, vec!["turn 0 alpha", "turn 1 alpha", "turn 3 alpha"]);
    }

    #[tokio::test]
    async fn maybe_compact_is_noop() {
        // RAG memory deliberately doesn't compact — the whole point
        // is to bound by retrieval, not summarization. The provider
        // must not be called.
        struct ShouldNotCall;
        #[async_trait]
        impl crate::providers::Provider for ShouldNotCall {
            async fn chat(
                &self,
                _: crate::providers::ChatRequest,
            ) -> Result<crate::providers::ChatResponse> {
                unreachable!("RAG memory must not call provider for compaction")
            }
        }
        let mut m = EmbeddingRagMemory::new(Arc::new(KeywordEmbedder));
        m.record(json!({"role":"user","content":"a"}))
            .await
            .unwrap();
        let provider = ShouldNotCall;
        m.maybe_compact(CompactContext {
            provider: &provider,
            model: "x",
            target_tokens: 10,
            hard_limit_tokens: 2_000_000,
            next_request_tokens: 1_000_000,
        })
        .await
        .unwrap();
        assert_eq!(m.len(), 1);
    }

    #[tokio::test]
    async fn truncate_drops_messages_above_n() {
        let mut m = EmbeddingRagMemory::new(Arc::new(KeywordEmbedder));
        for i in 0..5 {
            m.record(json!({"role":"user","content":format!("msg{}", i)}))
                .await
                .unwrap();
        }
        assert_eq!(m.len(), 5);
        m.truncate(2).await.unwrap();
        assert_eq!(m.len(), 2);
    }
}
