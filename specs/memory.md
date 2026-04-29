# Memory — Spec

A pluggable memory layer for the agent. Replaces today's flat
`Agent.messages: Vec<Value>` with a trait so different strategies — linear
with compaction, embedding-RAG, graph-backed, hierarchical summarization —
become drop-in implementations.

This doc is a feature spec; it lives alongside `specs/README.md`. Status of
the work is tracked in `specs/progress.md`.

## Mission

Make context-window management an extension axis instead of a hardcoded
behavior. Default stays simple and predictable (linear + compact);
experiments earn their slot by outperforming on real workloads.

A second, complementary trait — `NotesStore` — covers the cross-session
"long-term" surface (preferences, project facts, things the model writes
to remember next time). Active-context memory and cross-session notes are
distinct products with different failure modes; they get distinct traits.

## Why two traits, not one

| Concern | Active context (`Memory`) | Cross-session (`NotesStore`) |
| --- | --- | --- |
| What it holds | This conversation's messages | Persistent facts written across sessions |
| Failure mode of bad retrieval | **Poisons the live turn** — model acts on wrong context with no signal | Misses a hint — conversation is not corrupted |
| Update frequency | Every turn, every tool result | Rare, model-driven |
| Lifetime | Bounded by session | Bounded by user's preference (forever, until pruned) |
| Default ergonomics | Must be predictable, debuggable | Must be discoverable, editable |

These don't share an interface. Folding them into one trait would force
implementations to compromise on both axes.

## Principles

1. **Default beats clever.** `LinearWithCompact` is the spec's default
   forever unless data says otherwise. New strategies prove themselves on
   real workloads before promotion.
2. **Pure interface.** `Memory` does not reach for the network. If an
   implementation needs an LLM call (compaction, summary), the agent
   invokes it explicitly through `maybe_compact`.
3. **Cancellation survives the abstraction.** Ctrl-C rollback must work
   without REPL knowing the backend.
4. **Local-first.** Default has zero new dependencies. Anything heavier
   (vector store, graph DB) is an opt-in feature flag on a non-default
   impl.
5. **Plugin-friendly.** A user with 50 lines of Lua should be able to
   register a custom `Memory` strategy in Phase 3.

## The `Memory` trait

```rust
#[async_trait]
pub trait Memory: Send + Sync {
    /// Append a message to the active conversation. Called once per
    /// user input, once per assistant response, and once per tool result.
    async fn record(&mut self, message: Value);

    /// Materialize the message list to ship in the next chat request.
    /// Implementations are free to reorder, summarize, drop older turns,
    /// or interleave retrieved content. Pinned messages always come first.
    async fn snapshot(&self) -> Vec<Value>;

    /// Pin a message so it survives every snapshot regardless of
    /// compaction. Used for the system prompt; later for sticky
    /// instructions.
    async fn pin(&mut self, message: Value);

    /// Number of raw records since `clear()`. Used by the REPL for
    /// Ctrl-C rollback. The unit is "things `record` was called with."
    fn len(&self) -> usize;

    /// Roll back to a prior `len()`. Equivalent to undoing the last K
    /// recorded items. For `LinearWithCompact` this is a `Vec::truncate`;
    /// for graph-backed impls it rolls back recent writes to whatever
    /// state matches that count.
    async fn truncate(&mut self, n: usize);

    /// Drop all session-local state. Pinned content is preserved.
    async fn clear(&mut self);

    /// Optional hook the agent calls before each chat request. Strategies
    /// that don't compact return immediately. Strategies that do may run
    /// an LLM call against `ctx.provider` to summarize older turns.
    async fn maybe_compact(&mut self, _ctx: CompactContext<'_>) -> Result<()> {
        Ok(())
    }
}

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
```

### Why these methods

- `record` + `snapshot` is the minimum split that lets a strategy diverge
  from "store and replay" without restructuring the agent loop.
- `len` + `truncate` is the cancellation contract. Phrased in terms of
  "records," not "tokens" or "turns" — those are strategy-specific.
- `pin` keeps the system prompt anchored without leaking that detail
  into every snapshot implementation.
- `maybe_compact` is async because compaction may need a network call;
  it's separate from `snapshot` so reading state stays cheap.

## Default implementation: `LinearWithCompact`

Today's behavior, behind the trait.

- Internal state: `Vec<Value>` of raw messages + `Vec<Value>` of pinned.
- `snapshot()` returns `pinned ++ messages` (clone).
- `maybe_compact()` triggers when `current_tokens > target_tokens`:
  collapses the oldest non-pinned span into one summary message via
  `ctx.provider`.
- Zero new dependencies. Ships in Phase 1d.

## Alternative implementations — sketches, not commitments

### `EmbeddingRAG`

- Each `record()` vectorizes content via a configurable embedding model
  (default: `nomic-embed-text` running on the same Ollama).
- `snapshot()` = pinned + recent N + top-K nearest to the latest user
  message.
- Cheap on a local box; fails gracefully (degrades to recent-N if
  retrieval is empty).

### `GraphBacked`

- Nodes: entities (files, functions, identifiers, errors). Edges:
  "mentioned-in," "depends-on," "modified-by."
- `snapshot()` = pinned + recent N + entity-relevant context for the
  current focus.
- Entity extraction via tree-sitter + heuristics first; LLM extraction
  only if heuristics underperform.

### `HierarchicalSummary`

- Three tiers: raw recent, mid-summarized, deep summary.
- Aging messages get re-summarized into the next tier. No retrieval —
  pure stratified compaction. Smallest dependency footprint of the
  alternatives.

## Cross-session: `NotesStore` (Phase 4)

Separate trait, separate concern.

```rust
#[async_trait]
pub trait NotesStore: Send + Sync {
    async fn write(&self, note: Note) -> Result<()>;
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<Note>>;
    async fn list(&self, scope: NoteScope) -> Result<Vec<Note>>;
    async fn delete(&self, id: &str) -> Result<()>;
}
```

- Default impl: markdown files under `~/.config/agent/notes/` —
  human-readable, model-writable, version-controllable.
- Surfaced to the model as built-in tools (`WriteNote`, `SearchNotes`,
  `ListNotes`) — they appear in the tool registry alongside `Read` /
  `Edit`. The model decides what's worth remembering and looks it up
  when relevant.
- Graph/embedding indexing is genuinely valuable here because retrieval
  failures don't poison the live conversation — you just miss a hint.
- Detailed spec: `specs/notes.md` (to be written when Phase 4 starts).

## Migration from today's `Vec<Value>`

```rust
pub struct Agent {
    pub provider: Box<dyn Provider>,
    pub tools: Registry,
    pub model: String,
    pub memory: Box<dyn Memory>,   // was: messages: Vec<Value>, system_prompt: Option<String>
    ctx: ToolContext,
}
```

`run_streaming` shape after migration:

```rust
async fn run_streaming<F>(&mut self, prompt: &str, sink: &mut F) -> Result<String>
where F: FnMut(&str) + Send,
{
    self.memory
        .record(json!({"role": "user", "content": prompt}))
        .await;

    loop {
        self.memory.maybe_compact(self.compact_ctx()).await?;
        let req = ChatRequest {
            model: self.model.clone(),
            messages: self.memory.snapshot().await,
            tools: self.tools.openai_schemas(),
        };
        let resp = self.provider.chat_stream(req, sink).await?;
        self.memory.record(resp.message.clone()).await;
        // ...tool dispatch unchanged; tool results recorded via memory.record...
    }
}
```

REPL Ctrl-C rollback:

```rust
let saved = agent.memory.len();
// ...tokio::select cancel...
agent.memory.truncate(saved).await;
```

System prompt setup moves from `with_system_prompt` to:

```rust
agent.memory.pin(json!({"role": "system", "content": sys})).await;
```

## Phasing

| Phase | What lands |
| --- | --- |
| 1d | `Memory` trait, `LinearWithCompact` default, token tracking, `maybe_compact` wired into the agent loop, `Agent` migrated off `Vec<Value>` |
| 1d (stretch) | One alternative impl behind a config flag for measurement (probably `EmbeddingRAG`) |
| 2  | `/memory` slash command (`/memory snapshot`, `/memory stats`) for inspection |
| 3  | Plugin host API exposes `Memory` registration so Lua plugins can ship strategies |
| 4  | `NotesStore` trait + filesystem default + `WriteNote`/`SearchNotes`/`ListNotes` tools |

## Open questions

1. **Sync vs async `record`.** Linear doesn't need async; graph/embedding
   strategies do. Going async upfront avoids a breaking change later.
2. **Token counting ownership.** Probably lives outside `Memory` —
   token tracker reads `snapshot()` and supplies `current_tokens` to
   `maybe_compact`. Avoids strategy-specific tokenizer assumptions
   leaking into the trait.
3. **`truncate` granularity.** Raw record count is what we have today and
   matches the REPL's snapshot-len pattern. A `/undo`-style command
   would want turn granularity (user + assistant + tool results = 1
   turn). Defer until Phase 2 introduces `/undo` if it ever does.
4. **Pin semantics.** Right now `pin` is a separate API; pinned messages
   never roll back. Open whether pinned messages should be re-pinnable
   (overwrite) or append-only.
5. **Compaction reversibility.** Once `LinearWithCompact` collapses old
   turns into a summary, the originals are gone. Worth keeping a
   write-only log on disk so a `/uncompact` is theoretically possible?
   Lean toward no — adds storage, rarely useful, complicates Ctrl-C.

## Non-goals

- **Not a real graph database.** `GraphBacked` would be backed by the
  simplest store that works (probably SQLite + a tiny schema), not Neo4j.
- **Not auto-summarization on every turn.** Compaction triggers on
  token pressure, not on a timer.
- **Not unifying `Memory` and `NotesStore`.** They look similar; they
  aren't. See "Why two traits, not one" above.
- **Not committing to an alternative as default.** `LinearWithCompact`
  stays default until measured outcomes say otherwise.
