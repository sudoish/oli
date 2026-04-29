# Progress

A living status doc. Update at each phase boundary so a fresh context can
pick up without spelunking through git history.

Spec lives at `specs/README.md`. This doc covers state, not goals.

## Phase ledger

| Commit  | Phase | What it shipped                                                                |
| ------- | ----- | ------------------------------------------------------------------------------ |
| 9992d31 | —     | High-level spec                                                                |
| d958c18 | —     | Plugin system added to spec                                                    |
| b10688a | 0     | Module split, `Tool`/`Provider` traits, config loader, error types, FakeProvider |
| 829b3e9 | 1a    | `Edit`/`Grep`/`Glob`, `Read` pagination, output truncation, `ToolContext`        |
| 1e33058 | 1b    | System prompt with env / git / dir listing / `CLAUDE.md` ingestion             |
| 1ac7fef | 1c    | Streaming `Provider`, stateful `Agent`, REPL with rustyline, `SlashCommand` trait + `/clear`, `/help`, `/exit` |
| 254687e | docs  | `specs/memory.md` — pluggable active-context memory trait                       |
| _next_  | 1d    | `Memory` trait + `LinearWithCompact` default, token tracking, `maybe_compact` summarization, model-capability registry, tool-call fallback parser |

Tip-of-master at last update: **Phase 1d (this commit)**.
Tests: **110 unit tests, all green** (was 77). Release build: clean, zero warnings.
Smoke-tested against `qwen2.5-coder:7b` on Ollama — tool dispatch works
end-to-end via the fallback parser.

## What works today

**CLI:** Two modes off the same binary.
- `codecrafters-claude-code -p "prompt"` — single-shot, non-streaming,
  prints final assistant content. Same scripted-friendly behavior as before.
- `codecrafters-claude-code` (no `-p`) — interactive REPL with streaming
  output, multi-turn history, `/clear` / `/help` / `/exit`, Ctrl-C cancels
  the in-flight turn (history rolls back even after compaction), Ctrl-D
  exits.
- OpenRouter via env vars (`OPENROUTER_API_KEY`, `OPENROUTER_BASE_URL`),
  model `anthropic/claude-haiku-4.5` by default.
- TOML config at `~/.config/agent/config.toml` overrides defaults if
  present. Config supports multiple named providers, all of `kind =
  "openai-compat"` for now.

**Local-model survival kit (Phase 1d):**
- `Memory` trait (`src/agent/memory/`) replaces the flat `Vec<Value>`. Default
  `LinearWithCompact` keeps today's behavior plus auto-summarization when
  `current_tokens > target_tokens`.
- Compaction summarizes the older half via `provider.chat`, snapping to a
  user-message boundary so tool-call/tool-result pairs aren't split.
- `Memory::len` is monotonic across compaction events — REPL Ctrl-C
  rollback works even after compaction has run mid-session.
- Token tracking via `stream_options.include_usage`. Captured per-call as
  `agent.last_usage`; feeds the compaction trigger.
- Model-capability registry (`src/agent/caps.rs`) — hardcoded prefix → `{
  ctx_window, supports_native_tool_calls, supports_streaming_tool_deltas
  }`. `compact_target() = 80% of ctx_window`. qwen / llama / claude /
  gpt families covered, conservative default for anything unknown.
- Tool-call fallback parser (`src/agent/tool_parse.rs`) — for models
  flagged `supports_native_tool_calls = false`, scans assistant content
  for embedded JSON tool calls (bare, fenced, `<tool_call>`-tagged) and
  splices them onto the assistant message before the agent loop dispatches.
  Synthesizes globally-unique ids so subsequent tool results match.

**Tools:** `Read` (with `offset`/`limit`), `Write`, `Edit` (with
read-first invariant), `Bash`, `Grep` (via `rg`), `Glob` (via `glob`
crate). All outputs >30 KB are truncated with a marker that includes
the original byte count.

**ToolContext:** per-session state shared across tool calls. Currently
tracks canonical paths of `Read` files so `Edit` can enforce a
read-first invariant.

**System prompt:** built once at startup, prepended as `role:system`
on every turn. Includes:
- Identity preamble (5-line guidelines).
- Environment: cwd, OS/arch.
- Git: branch, status (porcelain summary), last 3 oneline commits.
- Project files: top-level listing (skip hidden, mark dirs).
- `CLAUDE.md`: walk-up from cwd (root-first ordering) + global
  `~/.claude/CLAUDE.md`. Capped at 16 KB total.

**Provider abstraction:** `OpenAICompatProvider` covers OpenRouter, OpenAI,
Ollama, LM Studio, vLLM, and llama.cpp's server. Same binary works against
all five with a config flip. The `Provider` trait carries both `chat`
(non-streaming) and `chat_stream(req, sink)` where `sink: &mut dyn
FnMut(&str) + Send` is re-borrowed across loop iterations. Streaming
assembles tool calls across SSE deltas by `index`, so OpenRouter / Ollama
fragmented tool-call streams converge to a single `tool_calls` array.

**REPL:** `repl::run(agent)` owns the loop. Rustyline runs in
`spawn_blocking` per iteration so the runtime is free for Tokio's signal
handler during in-flight turns. `tokio::select!` races the agent's
streaming future against `tokio::signal::ctrl_c()`; on cancel, history is
truncated to its pre-turn length so the next turn doesn't see a
half-finished assistant message.

**Slash commands:** `SlashCommand` trait + `SlashRegistry` mirror
`Tool` / `Registry`. Built-ins: `/clear`, `/help`, `/exit`. `/help` is
rendered against the live registry rather than from the command's own
`run`, so adding a command only requires `register(...)`.

**Test seam:** `FakeProvider` lets the entire agent loop be unit-tested
without touching the network. `ScriptedProviderHandle` wraps an
`Arc<FakeProvider>` so tests can both feed responses and inspect the
captured request stream.

## Architecture as built

```
src/
├── main.rs              # CLI: -p one-shot or REPL
├── agent/
│   ├── mod.rs           # Stateful Agent: messages, run, run_streaming, clear
│   └── context.rs       # SystemPromptBuilder
├── providers/
│   ├── mod.rs           # Provider trait + chat / chat_stream + ContentSink
│   ├── openai_compat.rs # OpenAICompatProvider with SSE streaming
│   └── fake.rs          # FakeProvider (cfg(test)), 2-chunk streaming for tests
├── repl/
│   ├── mod.rs           # rustyline + tokio::select Ctrl-C cancel
│   └── slash.rs         # SlashCommand trait + /clear /help /exit
├── tools/
│   ├── mod.rs           # Tool trait + Registry
│   ├── context.rs       # ToolContext + SessionState
│   ├── util.rs          # truncate + DEFAULT_MAX_OUTPUT_BYTES
│   ├── read.rs / write.rs / edit.rs / bash.rs / grep.rs / glob.rs
├── config.rs            # TOML loader + env-default fallback
└── error.rs             # AgentError + ToolError via thiserror
```

Per-spec architecture sketch except: `agent/compact.rs` (Phase 1d),
`providers/anthropic.rs` (Phase 4), `policy/`, `plugins/` not yet written.

## What's NOT done

Gaps a daily-driver user would hit today:

- **No policy / permission engine.** `Bash` runs whatever the model wants.
  (Phase 2.)
- **Slash command set is minimal.** `/model`, `/provider`, `/cost`,
  `/system`, `/tools`, `/compact`, `/memory` not yet implemented. (Phase 2.)
- **No subprocess tools** (MCP-lite from Phase 2).
- **No plugin runtime** (Phase 3, including plugin-registered `Memory`
  strategies).
- **No alternative `Memory` strategies shipped.** Default
  `LinearWithCompact` is the only impl in tree; `EmbeddingRAG` /
  `GraphBacked` / `HierarchicalSummary` are sketched in `specs/memory.md`
  but not implemented.
- **No native Anthropic provider** with prompt caching (Phase 4).
- **No `NotesStore`** for cross-session memory (Phase 4).
- **No session persistence**, no `--resume` / `--continue`.

## Decisions made

- **Repo strategy:** continuing on `master` alongside CodeCrafters
  submissions. CodeCrafters tests run remotely; if a future phase
  breaks stage parity, revisit.
- **Provider coverage:** OpenAI-compat covers Ollama, OpenRouter,
  OpenAI, LM Studio, vLLM, llama.cpp server. No separate adapters.
- **Tool trait async:** `async-trait` crate (dyn-compat for `Box<dyn
  Tool>`). Stable Rust still doesn't allow `async fn` in trait objects
  natively.
- **ToolContext shape:** `Arc<Mutex<SessionState>>`. Cheap clone, interior
  mutability handles future concurrent tool calls (loop is serial today).
- **CLAUDE.md ordering:** root-first so broader context appears before
  narrower (mirrors how Claude Code itself walks up).
- **Output truncation:** 30 KB default, char-boundary safe, marker shows
  bytes-shown / total.
- **Path canonicalization:** done on insert and lookup in `ToolContext`,
  so relative and absolute spellings of the same file converge.
- **Tool error surfacing:** operational failures (file not found,
  command failed, pattern not found) come back as result strings the
  model can react to. `ToolError` is reserved for argument-shape problems.
- **Streaming sink shape:** `&mut dyn FnMut(&str) + Send`, re-borrowed
  per loop iteration. Considered `Box<dyn FnMut + Send>` (must move-out)
  and an mpsc channel (extra overhead); the borrow form lets the agent
  reuse one closure across multiple turns + tool rounds.
- **Tool-call assembly during streaming:** by `index`, with per-index
  accumulators for `id` / `function.name` / concatenated
  `function.arguments`. Matches OpenAI / OpenRouter SSE delta semantics
  and survives Ollama's tendency to spread one call across many chunks.
- **REPL Ctrl-C semantics:** in-flight cancel via
  `tokio::signal::ctrl_c()`; on cancel, history is truncated to the
  pre-turn length so the next turn doesn't carry a half-completed
  assistant message back into the context. Ctrl-D exits cleanly via
  rustyline's `ReadlineError::Eof`.
- **`/help` rendering:** introspects the live registry from the REPL
  rather than from `Help::run`, which has no peers handle. Adding a
  command stays a single `register(...)` call.

## Open decisions still on the table

- **Streaming approach:** decided — `async-openai`'s `create_stream_byot`
  with our own SSE-delta assembly. Worked first try; haven't yet hit a
  case where raw `reqwest` would be needed.
- **Plugin scripting language:** Lua via `mlua` is the spec's lead
  candidate. Final call needed before Phase 3.
- **Plugin async model:** native `mlua` async vs sync-with-host-driving.
  Affects how natural `ctx:prompt(...)` feels to plugin authors.
- **Native Anthropic provider:** only worth writing for prompt caching.
  Defer until Phase 4.
- **TS plugins via Deno:** deferred. Plugin contract designed to be
  host-language-agnostic so a second runtime can be added later without
  breaking existing Lua plugins.
- **REPL feedback during tool rounds:** today the REPL is silent while
  the model is dispatching tools (only model content streams to stdout).
  Open whether to surface a small `→ Read(...)` line per tool call. Lean
  toward yes once Phase 2 brings the policy engine — the prompts will
  need that context anyway.

## Picking up from here

Fresh-context boot sequence:

1. Read `specs/README.md` — the vision and full roadmap.
2. Read `specs/memory.md` — the active-context memory architecture.
3. Read this file — current state.
4. `git log --oneline -10` — phase boundaries with commit SHAs.
5. `cargo test` — confirm 110 tests green.
6. `cargo build --release` — confirm clean build.
7. Pick the next phase below.

### Next up

**Phase 2 — flexibility surface.** With the local-driver mission done,
the next bottleneck is daily-use UX and safety.

- **Policy engine.** `Policy::check(tool, args, cwd) -> Allow | Ask |
  Deny`. `Bash` and `Edit` should never run unguarded.
- **`/model`, `/provider`, `/cost`, `/tools`, `/compact`, `/memory`.**
  Switching mid-session is the biggest single UX gap. `/cost` is
  trivial now that `agent.last_usage` is wired. `/memory` should expose
  `snapshot` for inspection and `compact` to manually trigger.
- **Subprocess tool registration (MCP-lite).** Three config lines = new
  tool, no recompile.
- **Per-config-section overrides for `caps`.** Right now caps are
  hardcoded; users with custom Ollama models can't override
  `supports_native_tool_calls` or `ctx_window`. Add a config table.

### Phase 1d smoke-test results (2026-04-28)

- **Token streaming + REPL plumbing**: still unverified by hand against
  a real network — the live REPL with Ctrl-C/`/clear`/`/help` flow
  hasn't been driven from a TTY in this sandbox.
- **Tool-call fallback parser**: verified end-to-end against
  `qwen2.5-coder:7b` on Ollama. `-p "Read Cargo.toml and tell me the
  package name"` returns `codecrafters-claude-code` after the parser
  splices the model's text-mode JSON into `tool_calls` and dispatches
  the `Read` tool. Multi-tool prompt (`Glob` then summarize) also
  succeeds.
- **Compaction**: not yet exercised on a real long session — the unit
  tests cover the algorithm; would be worth verifying the summary
  prompt produces coherent transcripts on a real model under live
  token pressure.

### Useful commands

```sh
# Run the full test suite
cargo test

# Build the release binary (codecrafters target dir)
bash your_program.sh -p "your prompt here"

# Quick API surface check
/tmp/codecrafters-build-claude-code-rust/release/codecrafters-claude-code --help

# Format and lint
cargo fmt --all && cargo build --release
```
