# Architecture polish plan

Goal: make oli feel simple again without removing capability, and make its
runtime reusable by line, headless, TUI, or desktop frontends. This is an
organization and documentation plan, not a feature roadmap. Preserve the
current trait taxonomy and behavior; improve the shape around it.

## Current assessment

The runtime is healthy: tests pass, the core extension traits are coherent, and
observability exists through transcripts, ledgers, diagnostics, replay, and slash
commands. The main issue is navigability. Several files have become too large,
and some historical specs still describe TUI-era architecture that no longer
exists.

## Progress

- Phase 1 is complete: historical reviews are archived, and the current
  architecture and manual verification guides live under `docs/`.
- The `Agent` split has started with typed outcomes in `agent/outcome.rs`.
- The tool execution boundary is extracted into `agent/tool_exec.rs` and can be
  tested without driving a provider loop.

## Principles

1. No behavior change during structural moves.
2. Keep `Agent` as coordinator, not a dumping ground.
3. Prefer mechanical splits before new abstractions.
4. Keep public extension surfaces stable.
5. Current-state docs should be easy to find; historical docs should be clearly
   marked as historical.
6. Keep the harness UI-agnostic; line, TUI, desktop, and headless clients should
   drive the same runtime contract.

## Autonomy and review decision

Normal runs execute tools automatically and never pause for per-call approval.
`Policy` remains a deterministic embedder extension for hard denials, while
`--strict` installs a deny-all policy. A future review mechanism should be a
typed run checkpoint where the agent stops and returns control to its frontend,
not another tool-permission prompt.

## Phase 1 — Establish current truth

### 1. Archive stale specs

Move historical TUI-era docs to an archive path, or add an explicit header to
each saying it is historical.

Archived:

- `specs/archive/review-2.md`
- `specs/archive/polish.md`

Done when a new reader can start from `specs/README.md` without being confused
about whether oli currently has a TUI.

### 2. Add a current architecture doc

Create `docs/current-architecture.md` with:

- module map
- agent loop diagram
- request/tool lifecycle
- extension surfaces
- persistence flow
- diagnostics/ledger flow
- where to add tools/providers/slash commands/hooks

Done when this doc is the best first read after `README.md` and `AGENTS.md`.

### 3. Add a manual verification doc

Create `docs/manual-verification.md` covering real runtime checks that unit tests
cannot fully prove:

- Ollama local run
- OpenRouter run
- Anthropic native run
- ChatGPT subscription auth
- automatic tool execution and `--strict`
- resume/edit invariant
- external edit invalidation
- plugin reload
- MCP startup/refresh
- compaction on a long session
- replay fixture check

Done when release confidence does not depend on ad hoc memory.

## Phase 2 — Improve navigability

### 4. Split `src/repl/slash.rs`

Current issue: one file owns the slash registry and every command.

Target shape:

```text
src/repl/slash/
  mod.rs
  registry.rs
  clear.rs
  help.rs
  cost.rs
  tools.rs
  system.rs
  memory.rs
  provider.rs
  model.rs
  sessions.rs
  plugins.rs
  mcp.rs
  config.rs
  diagnostics.rs
  paths.rs
```

Done when each slash command is easy to open independently and adding a new
command does not require editing a 2000-line file.

### 5. Split CLI command handling out of `src/bin/oli.rs`

Target shape:

```text
src/cli/
  mod.rs
  run.rs
  init.rs
  login.rs
  replay.rs
  sessions.rs
```

`src/bin/oli.rs` should remain a thin clap entrypoint plus subcommand dispatch.

Done when CLI behavior is organized by user command rather than startup history.

### 6. Split `src/agent/mod.rs` mechanically

Target shape:

```text
src/agent/
  mod.rs          # Agent struct, builders, public run entrypoints
  outcome.rs      # RunOutcome and related result helpers
  run_loop.rs     # think/call/observe loop
  tool_exec.rs    # policy + hooks + registry dispatch
  streaming.rs    # stream event and tool-call assembly
  compaction.rs   # preflight budget + compaction transaction
```

Done when the core loop is easier to read and no module has to understand every
agent concern at once.

## Phase 3 — Sharpen boundaries

### 7. Establish a frontend boundary

The line REPL currently combines input, output, cancellation, progress
rendering, and slash-command dispatch. Preserve it as the bundled
text frontend, but move the shared session behavior behind a library-side
controller that another frontend can drive without copying the REPL.

Target shape:

```text
src/runtime/
  mod.rs          # session controller and frontend-facing commands
  event.rs        # owned events: content, tools, completion, errors
  snapshot.rs     # current session/provider/model/tool/health state

src/frontends/
  line.rs         # rustyline + stdout/stderr adapter
  headless.rs     # one-shot text/JSON adapter
```

A future `tui` module or desktop binary should only need to:

- submit prompts and slash/runtime commands;
- consume typed, owned events;
- request cancellation;
- render snapshots and events in its own way.

Do not add a broad `Ui` trait. The stable boundary is a small command API plus
events and snapshots. Framework-specific state, widgets, windows, and event
loops stay in the frontend.

Done when a test frontend can run and cancel a turn, observe tool progress,
and inspect session state without reading stdin, writing stdout/stderr, or
reaching into `Agent` fields.

### 8. Document the lifecycle in code and docs

Canonical lifecycle:

```text
prompt
→ memory snapshot
→ budget estimate
→ optional compaction
→ provider request
→ stream/tool-call assembly
→ policy + hooks
→ tool dispatch
→ observation
→ ledger + transcript
→ stop/continue
```

Done when the same lifecycle appears in `docs/current-architecture.md` and is
reflected by module boundaries.

### 9. Extract a tool execution boundary

Move policy/hook/dispatch details behind a small internal API, roughly:

```rust
ToolExecutor::execute(call, ctx, policy, hooks, registry).await
```

Exact naming can differ. The point is that the main loop should ask for a tool
call to be executed, not inline hook/policy/dispatch details.

Done when testing tool execution behavior does not require driving the whole
agent loop.

### 10. Keep `Agent` as coordinator

`Agent` should own state and order the loop. Specialized modules should do the
work:

- streaming assembly
- compaction transaction
- tool execution
- ledger recording
- MCP refresh

Done when `Agent` still reads as the runtime coordinator rather than the place
where every subsystem is implemented.

## Phase 4 — Improve operational confidence

### 11. Add real-provider smoke commands

Document repeatable commands for:

- Ollama
- OpenRouter
- Anthropic
- ChatGPT subscription
- MCP stdio
- MCP HTTP

Done when each supported provider path has a short smoke test.

### 12. Add representative replay fixtures

Keep a small set of stable fixtures for:

- simple read/edit
- multi-tool task
- compaction path
- provider error path
- max-turn exhaustion

Done when replay can catch context-shaping regressions before manual testing.

### 13. Add a release gate

A release checklist should include:

```sh
cargo test
cargo build --release
cargo doc --no-deps --lib
./target/release/oli --help
./target/release/oli run --output json -p "say hello" --max-turns 3
```

Plus the manual provider/plugin/MCP checks that are practical for the machine.

## Phase 5 — Polish user-facing coherence

### 14. Unify documentation ownership

Suggested ownership:

- `README.md`: user-facing overview and setup
- `AGENTS.md`: contributor/agent operating guide
- `docs/cheatsheet.md`: daily-use reference
- `docs/current-architecture.md`: current implementation overview
- `docs/manual-verification.md`: release and smoke checks
- `specs/`: historical design and future plans

Done when every doc has one obvious job.

### 15. Review naming consistency

Audit names across docs, config, slash commands, and code for:

- provider/model
- memory/context
- transcript/session/conversation
- policy/tool execution/review
- diagnostics/logging

Done when user-facing language is consistent.

### 16. Add an extension golden path

Add or refresh a doc showing how to:

- add a Rust tool
- add a Lua plugin
- add a subprocess tool
- add an MCP server
- add a provider
- add a slash command

Done when a new contributor can extend oli without spelunking.

## Optional later work

- Move memory to top-level `src/memory/` if it keeps growing.
- Add a feature-gated `tracing` adapter for structured diagnostics.
- Add graph or hierarchical memory only when real workloads require it.

## Recommended order

1. Archive/update stale specs.
2. Add `docs/current-architecture.md`.
3. Add `docs/manual-verification.md`.
4. Split `src/repl/slash.rs`.
5. Split `src/bin/oli.rs`.
6. Split `src/agent/mod.rs`.
7. Establish and test the frontend boundary.
8. Extract tool execution boundary.
9. Add smoke/release checklist.
10. Polish naming and extension docs.
