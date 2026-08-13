//! Memory trait and bundled implementations. See `specs/memory.md` for the
//! full design rationale.
//!
//! Phase 1d ships the trait + the `LinearWithCompact` default. Future work
//! adds alternative strategies (embedding-RAG, graph-backed, hierarchical
//! summarization) as drop-in implementations of the same trait.

use async_trait::async_trait;
use serde_json::Value;

use crate::error::Result;
use crate::providers::Provider;

pub mod linear;
pub mod persisted;
pub mod rag;

pub use linear::LinearWithCompact;
pub use persisted::{PersistedMemory, SessionEntry, list_sessions, new_session_id};
#[allow(unused_imports)]
pub use rag::{Embedder, EmbeddingRagMemory, OllamaEmbedder};

#[async_trait]
pub trait Memory: Send + Sync {
    /// Append a message to the active conversation. Called once per user
    /// input, once per assistant response, and once per tool result.
    async fn record(&mut self, message: Value) -> Result<()>;

    /// Materialize the message list to ship in the next chat request.
    /// Pinned messages always come first.
    async fn snapshot(&self) -> Vec<Value>;

    /// The same messages `snapshot` returns, in the same order, labelled
    /// by where they came from. Callers that need to know whether a
    /// system message is the rolling summary or a live record use this
    /// rather than guessing from roles.
    ///
    /// The default splits off the pinned prefix and calls the remainder
    /// recent — correct for any strategy that doesn't synthesize
    /// content of its own. Strategies that do must override.
    async fn snapshot_parts(&self) -> ContextParts {
        let pinned = self.pinned().await;
        let mut snapshot = self.snapshot().await;
        let recent = snapshot.split_off(pinned.len().min(snapshot.len()));
        ContextParts {
            pinned,
            summary: Vec::new(),
            recent,
        }
    }

    /// Pin a message so it survives every snapshot regardless of
    /// compaction. Used for the system prompt today; later for sticky
    /// instructions.
    async fn pin(&mut self, message: Value) -> Result<()>;

    /// Return the pinned messages, in pin order. Default is empty for
    /// strategies that don't separate pinned from regular content; the
    /// `LinearWithCompact` default override returns its `pinned` vec so
    /// `/system` can render what's anchored without reaching into
    /// strategy internals.
    async fn pinned(&self) -> Vec<Value> {
        Vec::new()
    }

    /// Number of raw records since the last `clear()`. Counts entries the
    /// caller passed to `record`, not pinned messages and not internally
    /// managed summary state. Used by the REPL for Ctrl-C rollback.
    fn len(&self) -> usize;

    /// Roll back to a prior `len()`. For `LinearWithCompact` this is a
    /// `Vec::truncate`; for graph-backed implementations it rolls back
    /// recent writes to whatever state matches that count.
    async fn truncate(&mut self, n: usize) -> Result<()>;

    /// Drop all session-local state. Pinned content is preserved.
    async fn clear(&mut self) -> Result<()>;

    /// Optional hook invoked by the agent before each chat request.
    /// Strategies that don't compact return immediately. Strategies that
    /// do may run an LLM call against `ctx.provider` to summarize older
    /// turns.
    async fn maybe_compact(&mut self, _ctx: CompactContext<'_>) -> Result<()> {
        Ok(())
    }
}

/// A snapshot broken down by provenance. Concatenating the fields in
/// declaration order reproduces `Memory::snapshot` exactly.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContextParts {
    /// System prompt and anything else pinned past compaction.
    pub pinned: Vec<Value>,
    /// Strategy-synthesized stand-in for records it no longer holds
    /// verbatim — today, `LinearWithCompact`'s rolling summary.
    pub summary: Vec<Value>,
    /// Records still held verbatim, in insertion order.
    pub recent: Vec<Value>,
}

impl ContextParts {
    pub fn flatten(&self) -> Vec<Value> {
        let mut out =
            Vec::with_capacity(self.pinned.len() + self.summary.len() + self.recent.len());
        out.extend(self.pinned.iter().cloned());
        out.extend(self.summary.iter().cloned());
        out.extend(self.recent.iter().cloned());
        out
    }
}

/// Context handed to `Memory::maybe_compact` so a strategy can decide
/// whether to run a summarization pass.
pub struct CompactContext<'a> {
    pub provider: &'a dyn Provider,
    pub model: &'a str,
    /// Soft target. Strategies should aim to keep `current_tokens` under
    /// this when they decide to compact.
    pub target_tokens: usize,
    /// Live token count of the most recent snapshot, supplied by the
    /// agent's token tracker.
    pub current_tokens: usize,
}
