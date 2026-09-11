# Current architecture doc outline

This spec defines the doc that should become `docs/current-architecture.md`.
It is intentionally an outline so the implementation doc can be filled in while
reading the code.

## Purpose

Give users and contributors a single current-state explanation of how oli works.
It should replace the need to infer architecture from historical roadmap specs.

## Audience

- future contributors
- agents modifying the repo
- users who want to extend oli
- embedders reading the library API

## Proposed sections

### 1. One-screen summary

Explain oli as:

```text
CLI/REPL → Agent → Provider + Memory + Tools + Hooks → Transcript/Ledger
```

### 2. Runtime lifecycle

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

Include which modules own each step.

### 3. Module map

Use the current source tree, not historical plans:

| Path | Responsibility |
| --- | --- |
| `src/agent/` | think/call/observe loop, memory, caps, system prompt |
| `src/providers/` | model backend adapters |
| `src/tools/` | built-in tools and tool registry |
| `src/policy/` | Optional deterministic tool-denial policy |
| `src/hooks/` | pre/post/stop hooks |
| `src/plugins/` | Lua plugin runtime |
| `src/mcp/` | MCP clients and tool adapters |
| `src/repl/` | line REPL and slash commands |
| `src/bootstrap.rs` | shared startup wiring |
| `src/bin/oli.rs` | CLI entrypoint |
| `src/ledger/` | token, cost, latency, and context accounting |
| `src/diagnostics.rs` | operational warning ring buffer |
| `src/auth/` | ChatGPT subscription auth |

### 4. Frontend boundary

Explain that the line REPL and headless CLI are bundled frontends over the same
session runtime, not owners of agent behavior. Document the intended contract:

```text
frontend command
→ session runtime
→ Agent
→ owned runtime events + snapshot
→ frontend rendering
```

Cover:

- prompt and command submission;
- streaming content and tool progress events;
- cancellation and completion;
- session/provider/model/tool/health snapshots;
- where a TUI or desktop adapter should live;
- the rule that core runtime modules do not read stdin, render widgets, or own
  a window/event loop.

### 5. Core traits

Document the extension surfaces:

- `Provider`
- `Tool`
- `Memory`
- `Policy`
- `Hook`
- `SlashCommand`
- `SubagentSpawner`

Document `McpHandle` separately as the integration handle for a connected MCP
server rather than presenting it as an extension trait.

For each, include:

- what it is for
- where it lives
- bundled implementations
- where to add another implementation

### 6. Tool execution path

Explain:

```text
model tool call
→ fallback/native parsing
→ pre-tool hooks
→ optional deterministic policy
→ registry dispatch
→ post-tool hooks
→ result truncation/cache
→ observation message
```

Mention `ShowFull` for truncated results.

### 7. Memory and context

Explain:

- pinned system prompt
- linear memory
- persisted sessions
- compaction
- RAG memory option
- read-set persistence for edit safety

### 8. Persistence

Explain files and purpose:

- sessions JSONL
- ledger JSONL
- notes
- config overlays
- plugins

Link to `/paths` and `docs/cheatsheet.md`.

### 9. Observability

Explain:

- `/diagnostics`
- `/cost`
- `/memory`
- replay
- ledger records
- transcript records

### 10. Extension golden paths

Short pointers:

- add a native Rust tool
- add a provider
- add a slash command
- add a Lua plugin
- add a subprocess tool
- add MCP server config
- add a frontend adapter

Details can live in another doc; this section should point readers correctly.

### 11. Design boundaries

State the current intentional non-goals:

- no TUI in current tree
- no hosted/multi-user mode
- no IDE integration
- no speculative memory strategy beyond observed need
- no UI framework types in the core runtime

## Done criteria

- `docs/current-architecture.md` exists.
- It reflects the current text-first repo.
- It links to `README.md`, `AGENTS.md`, `docs/cheatsheet.md`, and relevant specs.
- It does not describe removed TUI code as current architecture.
- It makes clear how a new frontend reuses the runtime without forking agent
  behavior.
