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

pub use linear::LinearWithCompact;
pub use persisted::{PersistedMemory, SessionEntry, list_sessions, new_session_id, sessions_dir};

#[async_trait]
pub trait Memory: Send + Sync {
    /// Append a message to the active conversation. Called once per user
    /// input, once per assistant response, and once per tool result.
    async fn record(&mut self, message: Value);

    /// Materialize the message list to ship in the next chat request.
    /// Pinned messages always come first.
    async fn snapshot(&self) -> Vec<Value>;

    /// Pin a message so it survives every snapshot regardless of
    /// compaction. Used for the system prompt today; later for sticky
    /// instructions.
    async fn pin(&mut self, message: Value);

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
    async fn truncate(&mut self, n: usize);

    /// Drop all session-local state. Pinned content is preserved.
    async fn clear(&mut self);

    /// Optional hook invoked by the agent before each chat request.
    /// Strategies that don't compact return immediately. Strategies that
    /// do may run an LLM call against `ctx.provider` to summarize older
    /// turns.
    async fn maybe_compact(&mut self, _ctx: CompactContext<'_>) -> Result<()> {
        Ok(())
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
