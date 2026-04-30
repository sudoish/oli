# Roadmap — 8 → 10

Follow-up plan derived from the 2026-04-29 deep review. Sequenced by
impact-per-day. Each item lists *what*, *files*, and *done-when*. Phase
boundaries are natural ship points; you can stop at any one.

Cross-reference: `specs/progress.md` covers state through Phase 5b.
This doc covers everything after.

## Phase A — Daily-driver safety (1–2 days)

### A1. Bash timeout + cwd
- Add `timeout_ms` (default 120s) and `cwd` (default repo root) args.
  Track active cwd in `ToolContext` so it persists across Bash calls
  within a session.
- Wrap `Command::output` in `tokio::time::timeout`. On timeout, kill
  the child process group and return
  `"command timed out after Xs"` so the model can react and retry.
- Files: `src/tools/bash.rs`, `src/tools/context.rs`.
- Done when: a `sleep 999` returns a timeout result in ≤2s; a
  `cd src && pwd` round-trips correctly across two sequential Bash
  calls.

### A2. Strict-mode flag
- `--strict` flag flips `-p` from `AlwaysApprove` to `AlwaysDeny`.
  Document in `--help`. `Decision::Ask` outcomes surface as
  `user declined` tool results; the model can recover.
- Files: `src/main.rs`.
- Done when: `oli --strict -p "edit something"` returns the model's
  recovery path with `user declined …` and exits 0.

### A3. Persist Edit read-set across `--resume`
- Mirror `ToolContext::mark_read` writes to the session JSONL with a
  new op `read`. Replay restores the read-set so resumed sessions
  don't lose the read-first invariant.
- Files: `src/tools/context.rs`, `src/agent/memory/persisted.rs`.
- Done when: read a file, exit, `--continue`, attempt an Edit on it —
  succeeds without re-reading.

### A4. REPL progress indicator during tool rounds
- Introduce a thin `ToolEventSink` trait analogous to `ContentSink`.
  Agent fires `Started{name, args_preview}` / `Finished{name, ok}`
  around dispatch; REPL prints a single-line `→ Read(file=…)`
  (clipped to 80 cols) with a clear-line on completion.
- Files: `src/agent/mod.rs`, `src/repl/mod.rs`,
  `src/providers/mod.rs` (event sink lives next to `ContentSink`).
- Done when: a multi-tool turn shows live `→ Read … → Grep … → Edit …`
  lines that erase before the model's final content streams.

## Phase B — Hooks become useful (1 day)

### B1. Hook short-circuit + result mutation
- Change `Hook::handle` to return
  `HookOutcome::{Continue, Skip(result_string), Replace(args_or_result)}`.
  - `PreToolUse` honors `Skip` (synthetic result, no dispatch) and
    `Replace(args)` (modified args go to dispatch).
  - `PostToolUse` honors `Replace(result)` for redaction.
- Files: `src/hooks/mod.rs`, `src/agent/mod.rs`, plugin bridge in
  `src/plugins/mod.rs`.
- Done when: a Lua hook that returns `{ skip = "blocked by policy" }`
  on `pre_tool_use` for `Bash` short-circuits the call; a hook that
  returns `{ replace = "[redacted]" }` on `post_tool_use` redacts the
  result the model sees. Tests cover both paths plus the `Continue`
  (no-op) default.

## Phase C — Plugin DX & cost visibility (1 day)

### C1. `/plugins reload`
- Move the `SlashRegistry` onto `Agent` (or a `RwLock<Arc<…>>` shared
  with the REPL). On reload: rebuild plugin tools / slashes / hooks,
  swap the three sub-registries atomically, re-emit manifest.
- Files: `src/agent/mod.rs`, `src/plugins/mod.rs`, `src/repl/mod.rs`.
- Done when: edit a plugin file, `/plugins reload` in the live REPL,
  and the new tool is callable on the next turn without restart.

### C2. Session-level cost in `/cost`
- Accumulate `session_usage: Usage` on every chat round; render as
  `total: N prompt + M completion (since session start)` alongside
  last-call.
- Files: `src/agent/mod.rs`, `src/repl/slash.rs`.
- Done when: `/cost` after three turns shows totals ≈3× a single
  turn.

### C3. Subagent result cap
- Add `max_result_bytes` to `SubagentSpawner::spawn` (default 8 KB).
  Truncate with marker, identical to tool output truncation.
- Files: `src/tools/task.rs`, `src/main.rs` (`AgentSpawner`).
- Done when: a subagent that produces 50 KB returns ≤8 KB + truncation
  marker; the `max_result_bytes` arg overrides the default.

## Phase D — Caching, parity, polish (2 days)

### D1. OpenRouter prompt-cache headers
- When `kind = "openai-compat"` and the active model routes through
  Anthropic via OpenRouter, attach the same `cache_control` blocks
  the native Anthropic provider uses (system + last tool).
  Detection via `base_url.contains("openrouter")` plus an explicit
  `cache = "anthropic-style"` flag in `[providers.<name>]` for
  forward compatibility.
- Files: `src/providers/openai_compat.rs`, `src/config.rs`.
- Done when: a long OpenRouter session against
  `anthropic/claude-haiku-4.5` reports cache reads in `usage`
  (provider returns `cache_read_input_tokens`).

### D2. Anthropic `list_models`
- Hit `GET /v1/models`; parse and return the ids so `/model` works
  against the native Anthropic provider.
- Files: `src/providers/anthropic.rs`.
- Done when: `/model` against an Anthropic provider lists
  Opus / Sonnet / Haiku families.

### D3. Unified-diff preview for Edit/Write
- Pull in `similar = "2"`. Render unified-diff with 3-line context for
  Edit; for Write against an existing file, diff old vs new content;
  for Write of a new file, keep the current "+content" rendering.
- Files: `src/policy/mod.rs`, `Cargo.toml`.
- Done when: a multi-line Edit shows a
  `--- file / +++ file / @@ -10,3 +10,3 @@` block instead of two
  flat strings.

### D4. File-watch invalidation for Edit
- On Edit success, store the file's mtime in `ToolContext`. On the
  *next* Edit attempt for the same path, if mtime has advanced beyond
  the recorded value (external mutation), require a fresh Read.
  Return
  `Error: file modified externally since last read; re-read it.`
- Files: `src/tools/context.rs`, `src/tools/edit.rs`.
- Done when: read → external `echo > file` → Edit returns the
  re-read prompt instead of clobbering.

## Phase E — Stretch toward 10 (open-ended)

### E1. Plugin resource caps via `mlua` instruction hook
- Set a Lua debug hook firing every N instructions; abort if a plugin
  entry-point exceeds a budget (~1M instructions per call).
- Files: `src/plugins/mod.rs`.
- Done when: a `while true do end` plugin tool returns a
  `plugin exceeded execution budget` error within ~50 ms.

### E2. EmbeddingRAG memory strategy (proof of pluggability)
- Implement `EmbeddingRAGMemory` against an Ollama embed model
  (`nomic-embed-text`). Snapshot returns pinned + summary + top-k
  retrieved + recent N. Live-window kept small.
- Files: `src/agent/memory/rag.rs` (new), `specs/memory.md` (mark as
  shipped).
- Done when: a 200-turn synthetic session keeps the request under
  target tokens without summarization, and answers questions about
  turn 5 from turn 200.

### E3. MCP `notifications/tools/list_changed`
- Subscribe to the notification on the stdio reader; on receipt,
  re-run `tools/list` and atomically swap that server's tool
  registrations.
- Files: `src/mcp/server.rs`, `src/mcp/stdio.rs`.
- Done when: a server that adds a tool mid-session has it appear in
  `/mcp tools <s>` without restart.

## Suggested ordering & milestones

- **Week 1 ship:** A1–A4 + B1. Moves the harness from "useful daily
  driver" to "no obvious surprises." Most user-facing pain
  disappears here.
- **Week 2 ship:** C1–C3 + D1. Unlocks the plugin iteration loop and
  brings caching parity.
- **Backlog:** D2–D4 in any order; E1–E3 when there's appetite for
  stretch work.

## Acceptance criteria for "10"

A user can:
- run a 4-hour Ollama session with no manual babysitting
  (A1, A4, compaction, C2 visible);
- write a Lua plugin, `/plugins reload`, see it work, no restart
  (C1, B1);
- run scripted CI tasks under `--strict` without rubber-stamping
  (A2);
- resume any session tomorrow and continue editing without re-reading
  every file (A3);
- pay ~10% of the OpenRouter bill they pay now on long sessions
  (D1);
- never silently overwrite a file modified outside the agent (D4).

## Status tracker

Update as items land. Mirror commit SHAs into `specs/progress.md` at
each phase boundary.

| ID | Item                                              | Status |
| -- | ------------------------------------------------- | ------ |
| A1 | Bash timeout + cwd                                | DONE   |
| A2 | `--strict` flag for `-p`                          | DONE   |
| A3 | Persist Edit read-set across `--resume`           | DONE   |
| A4 | REPL progress indicator during tool rounds        | DONE   |
| B1 | Hook short-circuit + result mutation              | DONE   |
| C1 | `/plugins reload`                                 | TODO   |
| C2 | Session-level cost in `/cost`                     | TODO   |
| C3 | Subagent result cap                               | TODO   |
| D1 | OpenRouter prompt-cache headers                   | TODO   |
| D2 | Anthropic `list_models`                           | TODO   |
| D3 | Unified-diff preview for Edit/Write               | TODO   |
| D4 | File-watch invalidation for Edit                  | TODO   |
| E1 | Plugin resource caps via mlua instruction hook    | TODO   |
| E2 | EmbeddingRAG memory strategy                      | TODO   |
| E3 | MCP `notifications/tools/list_changed`            | TODO   |
