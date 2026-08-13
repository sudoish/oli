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
| 21f7bc5 | 1d    | `Memory` trait + `LinearWithCompact` default, token tracking, `maybe_compact` summarization, model-capability registry, tool-call fallback parser |
| 19f849f | 2     | Policy engine, slash command set (`/cost` `/tools` `/system` `/memory` `/compact` `/provider` `/model`), subprocess tool registration, per-config caps overrides |
| b3892dd | 3     | Session persistence (`--resume`/`--continue`/`/sessions`), hook dispatcher (`PreToolUse`/`PostToolUse`/`Stop`), subagent (`Task` tool) + `SubagentSpawner`, Lua plugin runtime via mlua + `/plugins` |
| 0472f3b | 4     | Top-level `max_turns` (config + CLI), per-project `.oli/config.toml` overlay, diff preview for `Edit`/`Write`, expanded plugin host API (`ctx:prompt`/`shell`/`read_file`/`write_file`/`get_state`/`set_state`/`ask_user`), `NotesStore` trait + filesystem default + `WriteNote`/`SearchNotes`/`ListNotes` tools, native Anthropic provider with prompt caching |
| 59c07c0 | 5a    | MCP client: `McpTransport` trait + `StdioTransport` (newline-delimited JSON-RPC 2.0 over child stdio), `McpServer` lifecycle (initialize → notifications/initialized → tools/list), `McpTool` namespaced adapter (`<server>__<tool>`), `[mcp.servers.*]` config + env-var expansion + allow/deny filter + per-server timeouts + `enabled` overlay, parallel best-effort startup with `HealthState::Down` for failed servers, `/mcp` slash (list / `tools <s>` / `logs <s>`) |
| e83c9fb | 5b    | `HttpTransport` for streamable-http (POST + JSON or SSE response, `Mcp-Session-Id` capture/echo, env-expanded user headers), `/mcp restart <server>` in place, `auto_allow_pure_reads` policy heuristic (default on; only fires on `<server>__<verb>`-shaped names) |
| 8ef275f | A     | Daily-driver safety: Bash timeout + per-call cwd, `--strict` flag, persisted-reads round-trip, REPL `→ Tool(...)` progress hook |
| 18f27f7 | B     | Hook short-circuit (`PreToolUse` can return synthetic result), result-mutation hook outcome |
| e1af9ce | C     | `/plugins reload` (registry rebuild via shared `Arc<Mutex<Registry>>`), `session-cost` in `/cost`, subagent result cap |
| 2fbecfb | D     | Caching/parity polish: OpenRouter cache control, Anthropic model list, unified-diff preview via `similar`, stale-`Edit` detection (mtime check) |
| 0193495 | E1    | Plugin instruction-count budget via mlua thread hooks (`max_instructions` config; deterministic timeout for runaway loops) |
| 19b32f8 | E2    | `EmbeddingRagMemory` (retrieval-mediated snapshots; `Embedder` trait + `OllamaEmbedder` default; configurable via `[memory] kind = "rag"`) |
| 110d8ee | E3    | MCP `tools/list_changed` live refresh (per-turn diff against per-server `Arc<AtomicBool>`; agent loop swaps registry entries atomically) |
| c23da45 | F     | TUI skeleton + alt-screen lifecycle + `--plain` fallback                                              |
| a5db293 | G     | TUI agent integration: streaming, mode indicator, slash dispatch, Ctrl-C cancel, rebuild on swap        |
| 5d0ca6e | H     | TUI tool-call cards: live spinner, timing, result summaries                                           |
| f9fb277 | I     | TUI approval modal: single-key dispatch, scrollable diff preview, session-scoped allow/deny           |
| 1515a42 | J     | TUI markdown rendering + syntect-highlighted code fences                                              |
| d0f4d4e | K     | TUI input ergonomics: multi-line, completion popups, persistent history                               |
| 020da39 | K4    | TUI Ctrl-R history search overlay                                                                     |
| 895fdf8 | L     | TUI scrollable transcript + stick-to-bottom + `/copy N` via OSC52                                     |
| d21567e | M     | TUI status bar: identity strip + token gauge + live mode + width-aware collapse                       |
| 7ea160e | N     | TUI discoverability overlays: `/sessions` picker, `/help` browser, `/<cmd> ?` cards, fading hints     |
| 3e1c69b | N4    | TUI first-run wizard + Bash process-group kill (real grandchild termination via `setpgid` + `killpg`) |
| cec3113 | O     | TUI recoverability: `/undo`, `Ctrl+E` edit-and-rerun, verified Bash cancel kills the whole process group |
| 2b2e571 | docs  | `specs/review-2.md` — deep review #2 (rated 9/10)                                                     |
| 43da094 | docs  | `specs/polish.md` — 9/10 → 10/10 polish roadmap                                                       |
| c243257 | P1    | Zero build warnings: drop real dead code, annotate intentional public surface with `#[allow(dead_code)]` + comment |
| fe1fed2 | P2    | Overlay sum type: replace App's six `Option<*State>` overlay fields with one `Overlay` enum; single-match keypress router |
| 1687232 | P3    | Split `tui/app.rs` (1729 LOC) → `app/{mod,overlay,transcript,tests}.rs`; split `tui/ui.rs` (1358 LOC) → `ui/{mod,overlays,transcript}.rs` |
| 735fb45 | Q1+Q2 | Extract `src/lib.rs` with public re-exports, move binary to `src/bin/oli.rs`, factor reusable wiring into `src/bootstrap.rs` |
| 09876fd | Q3    | Module-level `//!` docs on every top-level module; `cargo doc --no-deps` produces a clean module index |
| fa718c4 | R     | `/diagnostics` ring buffer (8 KB cap, FIFO eviction) + `log_*!` shim replacing `eprintln!` in mcp/plugins/providers/repl; `RUST_LOG` threshold (info default) |
| ba630c8 | S1    | Persisted approval allow-list at `~/.config/oli/policy-allow.json`; capital `[A]` writes through, lowercase `a` stays session-only |
| 7e2e67f | S2    | Subagent inherits parent's `ToolContext`: `SubagentSpawner::spawn` takes `Option<ToolContext>`; child gets parent's read-set + sticky cwd |
| d5e5961 | S3    | `ShowFull(id, offset, limit)` tool + per-session result cache (32-entry FIFO); truncation marker embeds the cache id |
| 9ab0d70 | T1    | `oli init` headless CLI subcommand (`--provider`, `--api-key`, `--force`); `wizard_init` module shared with TUI wizard |
| 343462c | T2    | `/config reload` slash command: re-parse config.toml, swap provider/model/policy/caps; memory + transcript + system prompt survive |
| 0fe85b6 | T3    | `tui` and `syntax-highlight` Cargo features; `cargo build --no-default-features` produces an 8.5 MB line-mode-only binary (was 11 MB) |
| 2582b5f | T4+T5 | `docs/cheatsheet.md` (full keybind/slash/path/env/feature reference, linked from `oli --help`); `specs/README.md` TOC pointing at every spec doc |

The ledger is history, not current state. Phases F–O built a ratatui
front-end that has since been removed along with the `tui` and
`syntax-highlight` Cargo features; oli is text-first with a line REPL.
For what exists today, read `AGENTS.md` (module map) and `README.md`.

## What's NOT done

The 17-item polish plan (`specs/polish.md`) closed every gap from the
post-TUI deep review. Remaining open items are forward-looking, not
regressions:

- **`tracing`-style structured logging.** The diagnostics ring is a
  flat string store. Embedders who want per-event spans / kv pairs
  would need a `tracing-subscriber` adapter. Not blocking — the
  ring buffer covers the "where did that error come from?" case.
- **GraphBacked / HierarchicalSummary memory strategies.** Sketched
  in `specs/memory.md`; only `LinearWithCompact` and
  `EmbeddingRagMemory` are implemented. Adding a third is a clean
  drop-in via the `Memory` trait.
- **Hosted/multi-user mode.** Out of scope per the spec; flagged
  here so a future contributor sees it's an explicit non-goal.
- **`oli init --reset`.** Today `--force` overwrites; a `--reset`
  flag that deletes + recreates is on the polish plan's open
  decisions list. Not landed.

## Decisions made

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

## Picking up from here

Fresh-context boot sequence:

1. Read `AGENTS.md` — module map, discipline, where to add what.
2. Read `docs/cheatsheet.md` — every slash command, file path, and
   env var in one page.
3. Read this file — phase ledger with SHAs, for history.
4. `git log --oneline -20` — recent boundaries.
5. `cargo test` — confirm the suite is green.
6. `cargo build --release` — confirm a clean release build.
7. `cargo doc --no-deps --lib --open` — render the public API
   index (every top-level module has a `//!` overview).

### Next up

The original roadmap (Phases 0–5) and the post-MCP roadmap (A–E,
F–O, P–T) are all shipped. Anything below is opportunistic polish
or research, not a roadmap commitment.

- **`tracing` adapter.** Optional feature that bridges
  `crate::log_*!` macros into `tracing` events for embedders who
  want structured logging.
- **`oli init --reset`.** `--force` overwrites; `--reset` would
  delete + recreate. Flagged in `specs/polish.md` open decisions.
- **GraphBacked or HierarchicalSummary memory.** Third drop-in for
  the `Memory` trait once a real workload makes the choice obvious.
- **Per-project `.oli/notes/`.** Notes are globally-scoped today;
  project-scoped would let a repo carry its own knowledge.
- **Provider-side Anthropic prompt caching coverage.** The native
  provider sets cache_control on system + last tool; OpenRouter
  cache hooks on the same surfaces (Phase D). A real measurement
  pass on long sessions would tell us whether the cache cutoffs
  are placed where they actually pay off.

### Phase 1d smoke-test results (2026-04-28)

- **Token streaming + REPL plumbing**: still unverified by hand against
  a real network — the live REPL with Ctrl-C/`/clear`/`/help` flow
  hasn't been driven from a TTY in this sandbox.
- **Tool-call fallback parser**: verified end-to-end against
  `qwen2.5-coder:7b` on Ollama. `-p "Read Cargo.toml and tell me the
  package name"` returns the package name after the parser splices
  the model's text-mode JSON into `tool_calls` and dispatches the
  `Read` tool. Multi-tool prompt (`Glob` then summarize) also
  succeeds.
- **Compaction**: not yet exercised on a real long session — the unit
  tests cover the algorithm; would be worth verifying the summary
  prompt produces coherent transcripts on a real model under live
  token pressure.

### Phase 4 smoke-test results (2026-04-29)

- **Compilation + unit tests**: 202 green, release build clean.
- **NotesStore (filesystem)**: round-trip tested — write, list,
  search by query+tag, delete, reload-on-startup all pass.
- **Native Anthropic provider**: shape conversion verified end-to-end
  via 9 unit tests (system extraction, tool_use/tool_result blocks,
  cache_control on system + last tool, response → OpenAI-shape
  conversion). The HTTP path against api.anthropic.com is untested
  in this sandbox — the user can flip a config provider entry to
  `kind = "anthropic"` and a real ANTHROPIC_API_KEY to verify.
- **Per-project `.oli/config.toml` merge**: covered by 4 unit
  tests (overlay scalar wins, table merge per-key, array concat with
  overlay first, walk-up parent dirs).
- **Diff preview**: unit-tested Edit/Write rendering paths;
  end-to-end TTY behavior unverified (live REPL approval flow
  requires a stdin-bound session).
- **`max_turns`**: previously-observed qwen runaway should now stop
  at 40 turns by default. Not yet exercised on a real long session
  to confirm the cutoff behavior reads cleanly to the user.
- **Plugin host API expansion**: ctx:prompt / ctx:shell / ctx:read_file
  / ctx:write_file / ctx:get_state / ctx:set_state / ctx:ask_user
  compile and the existing plugin tests still pass; no new live
  smoke against a multi-method plugin.

### Phase 3 smoke-test results (2026-04-29)

- **Lua plugin runtime**: verified live. Drop a `hello.lua` into
  `~/.config/oli/plugins/`, ask the agent to call `Greet`, and the
  Lua function fires (`[plugin:hello] info Greet invoked` lines on
  stderr) and returns `"hello, plugins!"` to the model. Sandbox blocks
  `io` access (covered by unit test).
- **Caveat surfaced**: qwen2.5-coder:7b's fallback-parsed tool calls
  can loop — the model re-emits the same JSON tool call on each turn
  even after seeing the result. Not a runtime bug; a known weakness of
  the model with text-mode tools. Top-level `max_turns` config (Phase
  4) would short-circuit this.
- **Session persistence + hook dispatcher + Task subagent**: unit-tested
  end-to-end (replay, hook firing, spawner forwarding) but not yet
  exercised in a live multi-turn REPL. Worth a manual session that
  does `/sessions` → exit → `--continue` → continue working.

### Phase 2 smoke-test results (2026-04-28)

- **Policy through dispatch**: verified against `qwen2.5-coder:7b` on
  Ollama. `-p "Use the Read tool ... package name"` returns the
  package name — Read is auto-allowed by the default policy, dispatch
  flows through the policy gate, fallback parser still bridges
  JSON-as-content. Plain chat without tools also works.
- **Slash commands `/cost`, `/tools`, `/system`, `/memory`, `/compact`,
  `/provider`, `/model`**: unit-tested end-to-end (132+ tests covering
  the listing/swap/inspection paths). Live REPL behavior under user
  input still needs a TTY-driven session.
- **Approval prompt**: `ReadlineApprover` reads y/N via stdin on a
  blocking task; not exercised live yet because the smoke test went
  through `-p` (`AlwaysApprove`). Needs a REPL session that triggers an
  `Edit` or unknown `Bash` to verify the prompt rendering.
- **Subprocess tools**: round-trip exercised via `cat` / `wc -c` /
  `false` in unit tests; no live model has been steered into calling a
  config-registered subprocess tool yet.
- **Caps overrides**: unit-tested in `caps.rs`. Live Ollama usage with
  a `[[caps]]` config block to flip `supports_native_tool_calls=true`
  on qwen would let us measure whether qwen's structured output is
  reliable enough to skip the fallback parser.

### Useful commands

```sh
# Run the full test suite
cargo test

# Library tests only
cargo test --lib

# Build the binary
cargo build --release

# Run against a real model
cargo run --release -- -p "your prompt here"

# Headless first-run config bootstrap
./target/release/oli init --provider ollama

# Quick API surface check
./target/release/oli --help

# Render the public-API docs
cargo doc --no-deps --lib --open
```
