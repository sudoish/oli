# Current architecture

Oli is a text-first coding-agent runtime exposed as both a binary and a Rust
library. This document describes the code that exists now. Product rationale
and historical plans live under [`specs/`](../specs/README.md).

## System at a glance

```mermaid
flowchart TD
    F["Line REPL or headless CLI"] --> B["Startup and session wiring"]
    B --> R["Session runtime"]
    R --> A["Agent coordinator"]
    A --> P["Provider"]
    A --> M["Memory"]
    A --> T["Tool registry"]
    A --> H["Hooks and optional policy"]
    A --> O["Transcript, ledger, and diagnostics"]
    T --> X["Built-ins, plugins, subprocess, MCP"]
```

[`src/bin/oli.rs`](../src/bin/oli.rs) parses and dispatches commands.
[`src/cli/`](../src/cli/) implements those commands and assembles top-level runs.
[`src/bootstrap.rs`](../src/bootstrap.rs) provides reusable constructors for
tools, sessions, memory, accounting, and subagents. [`Agent`](../src/agent/mod.rs)
owns conversation state and exposes the stable run entrypoints. Its internal
[`run_loop`](../src/agent/run_loop.rs) coordinates think → call → observe while
[`compaction`](../src/agent/compaction.rs) and
[`streaming`](../src/agent/streaming.rs) own request preflight and provider
response assembly respectively. [`SessionRuntime`](../src/runtime/mod.rs) owns
the top-level agent and translates that low-level API into commands, owned
events, cancellation, and snapshots for frontends.

## Runtime lifecycle

For each submitted prompt, [`Agent::run_streaming`](../src/agent/mod.rs):

1. Records the user message in `Memory`.
2. Refreshes tools for MCP servers that emitted `tools/list_changed`.
3. Materializes the pinned and recent context plus tool schemas.
4. Estimates the next request and asks memory to compact while over budget.
5. Sends a `ChatRequest` through the active `Provider`.
6. Streams provider events to the caller and records usage and latency.
7. Records the assistant message and parses fallback textual tool calls when
   the selected model does not support native calls.
8. If tools were requested, executes them and records observations, then starts
   another model turn.
9. Otherwise dispatches stop hooks and returns `RunOutcome::Completed`.

`max_turns` produces the distinct `RunOutcome::MaxTurnsExhausted` state. It
cannot be converted into a successful completion by presentation code.

## Module map

| Path | Responsibility |
| --- | --- |
| `src/agent/` | Agent state/coordinator, think-call-observe loop, request compaction, provider stream assembly, typed outcomes, tool execution, model capabilities, system context, fallback parsing, and memory strategies. |
| `src/cli/` | Reusable command handlers, headless output contracts, session selection, and top-level agent startup assembly. |
| `src/providers/` | `Provider` implementations for Anthropic, ChatGPT subscription, and OpenAI-compatible APIs. |
| `src/tools/` | `Tool`, registry, built-in tools, subprocess tools, output bounds, and subagent support. |
| `src/hooks/` | Pre-tool, post-tool, and stop hook composition. |
| `src/policy/` | Optional deterministic hard-deny policy. Normal runs use `AllowAll`; `--strict` uses `DenyAll`. |
| `src/mcp/` | stdio and streamable-HTTP MCP clients, OAuth, server lifecycle, and MCP-to-Tool adapters. |
| `src/plugins/` | Sandboxed Lua discovery, loading, host APIs, tools, hooks, and slash commands. |
| `src/repl/` | Rustyline frontend and stream rendering; `repl/slash/` contains the slash registry and responsibility-grouped built-in commands. |
| `src/runtime/` | Frontend-neutral session ownership, prompt commands, owned run/tool events, cancellation rollback, and session snapshots. |
| `src/bootstrap.rs` | Reusable startup constructors for tools, sessions, memory, ledger, and subagents. |
| `src/bin/oli.rs` | Clap-only syntax, parsing, and thin top-level dispatch. |
| `src/config.rs` | Global/project TOML loading and deterministic overlay rules. |
| `src/ledger/` | Request estimates, context attribution, usage, cost, and latency records. |
| `src/auth/` | ChatGPT subscription OAuth and credential refresh. |
| `src/notes/` | Cross-session filesystem note store. |
| `src/diagnostics.rs` | Bounded operational warning ring buffer. |
| `src/replay.rs` | Provider-free transcript replay and comparison. |

## Core extension surfaces

| Surface | Purpose | Bundled implementations / registration |
| --- | --- | --- |
| `Provider` | Turn messages and tool schemas into streamed assistant responses. | `providers/anthropic.rs`, `providers/openai_compat.rs`, and the test fake; add construction in `providers::build`. |
| `Tool` | Describe and execute a model-callable capability. | Built-ins under `tools/`; register top-level tools in binary startup or reusable defaults in `bootstrap::build_default_tools`. |
| `Memory` | Own active conversation context and compaction. | `LinearWithCompact`, `PersistedMemory`, and `EmbeddingRagMemory` under `agent/memory/`. |
| `Hook` | Observe, replace, or skip lifecycle values. | Built-in and Lua hooks share `HookRegistry`. |
| `Policy` | Deterministically allow or hard-deny a model tool call. | `AllowAll` and `DenyAll`; embedders can provide another implementation with `Agent::with_policy`. |
| `SlashCommand` | Handle line-frontend commands without sending them to a model. | `SlashRegistry` lives in `repl/slash/registry.rs`; bundled implementations are grouped by responsibility beside it. |
| `SubagentSpawner` | Construct isolated child loops for the `Task` tool. | `DefaultAgentSpawner` in `bootstrap.rs`. |
| `McpHandle` | Retain a live MCP server and refresh its tools. | Built by `mcp::connect_all`; exposed tools enter the normal registry. |

The public re-exports in [`src/lib.rs`](../src/lib.rs) are the supported starting
point for embedders.

## Tool execution

```mermaid
flowchart TD
    C["Native or fallback tool call"] --> Pre["PreToolUse hooks"]
    Pre -->|"replace args"| Pre
    Pre -->|"skip"| Post["PostToolUse hooks"]
    Pre --> Pol["Policy"]
    Pol -->|"deny"| Post
    Pol -->|"allow"| R["Registry dispatch"]
    R --> Post
    Post --> Mem["Tool observation in Memory"]
```

Normal runs never pause for per-call permission. Hooks can replace arguments or
short-circuit a call with a synthetic result. The optional policy can only allow
or deny; `--strict` installs deny-all. Post hooks see real, skipped, and denied
results before the observation is recorded.

Tools use `ToolContext` for the current working directory, bounded full-result
cache, and the read set that protects edits from stale external changes.
Oversized results carry a cache identifier that the `ShowFull` tool can page.

Plugin host calls are different from model tool calls: plugins are
user-installed code and their explicit `ctx:*` calls dispatch directly through
the registry. Lua has no raw `os`, `io`, or dynamic-library access.

## Memory and persistence

The system prompt is pinned once with `Agent::pin_system_prompt`; clearing or
compacting recent memory does not remove it. `agent::compaction` materializes
`Memory::snapshot_parts`, keeps pinned, compacted, recent, and tool-schema
attribution separate for accounting, and rejects an over-limit authoritative
request only after bounded transactional compaction attempts.

Top-level runs wrap memory in `PersistedMemory`. Session transcripts are JSONL,
and replay restores both conversation messages and the read set needed by the
edit safety invariant. Compaction replaces older active context with a summary
transactionally; failures retain the unchanged request context.

The main persisted locations are:

- global config: `$XDG_CONFIG_HOME/oli/config.toml`, normally
  `~/.config/oli/config.toml`;
- project config: the nearest ancestor `.oli/config.toml`;
- sessions and adjacent `*.ledger.jsonl` files: the runtime sessions directory;
- notes: the filesystem notes directory;
- plugins: global and project plugin directories;
- MCP OAuth credentials: the owner-only MCP auth directory.

Use `/paths` instead of guessing resolved locations. See
[`docs/cheatsheet.md`](cheatsheet.md) for commands, files, and environment
variables.

## Configuration and startup

`Config` starts from environment defaults, merges global TOML, then merges the
nearest project overlay. Tables merge by key, scalar leaves are replaced, and
project array entries precede global entries where lookup order matters.

The `cli::run` startup path then:

1. resolves provider and model overrides;
2. opens notes, plugins, MCP servers, transcript, and ledger;
3. builds the default registry and registers `Task`, plugin, and MCP tools;
4. constructs and pins the agent;
5. wraps it in `SessionRuntime` and hands that to the headless path or line REPL.

MCP connection failures and plugin load failures are reported through
diagnostics without preventing unrelated capabilities from starting.

## Observability

- `/diagnostics` shows operational warnings from plugins, MCP, providers, and
  other recoverable startup/runtime failures.
- `/cost` shows last-call and session token/cost totals.
- `/memory` explains active context pressure and memory composition.
- Ledger JSONL records estimates, attributed context, provider usage,
  compaction, latency, and configured pricing per request.
- Transcript JSONL is the durable conversation and replay source.
- `oli replay` compares stable fixtures without making provider calls.

## Frontend boundary

The boundary is deliberately smaller than a `Ui` trait:

```text
RuntimeCommand::Prompt → SessionRuntime → Agent::run_streaming
borrowed StreamEvent → owned RuntimeEvent → frontend renderer
CancellationToken → drop in-flight run + memory rollback
SessionRuntime::snapshot → frontend
```

`RuntimeEvent` owns every payload and is serializable. It reports content,
tool-call deltas and starts, then completion, turn exhaustion, cancellation, or
an error. The controller drains queued progress before emitting that outcome. A
rollback failure is reported as an error immediately before the cancellation
event, matching the line frontend's existing best-effort rollback behavior.
`SessionSnapshot` reports the session id, last run state, configured provider
when known, model, registered tools, MCP health/tool counts, provider-reported
usage (including missing-report counts), and the bounded ledger summary. It does
not infer unavailable values.

Both the line and headless paths execute prompts through `SessionRuntime`.
Headless ignores incremental events and preserves its final text/JSON contract;
the line frontend renders content and tool progress. Rustyline, signals, stdout,
stderr, and JSON output shaping stay outside `runtime`.

Cancellation is an externally triggerable, one-shot token. When observed, the
controller drops the in-flight agent future and truncates memory to its exact
pre-command length, preserving the former line-REPL transaction. Cancellation
cannot guarantee that a remote provider stops server-side work, and—as with any
cooperative async cancellation—it cannot interrupt code that blocks its
executor thread without yielding.

Slash commands still own line-frontend concerns and receive `&mut Agent`.
`SessionRuntime::agent_mut` is the narrow transitional escape hatch for that
existing API; prompt lifecycle behavior no longer uses it.

The controller runs in its caller's task rather than hiding the agent behind a
background actor. This keeps ownership and event ordering direct, preserves the
slash-command bridge, and makes the cancellation token the only cross-task
handle. Snapshots are therefore requested between commands; a concurrent
command queue or live snapshot service is not part of this boundary.

A future review mechanism belongs at this boundary as a typed run checkpoint:
the runtime stops and returns control. It is not a per-tool approval prompt.

## Extension paths

- Native tool: implement `Tool` in `src/tools/` and register it in startup.
- Provider: implement `Provider` and add it to `providers::build`.
- Slash command: implement `SlashCommand` and register it in the default set.
- Lua plugin: add one file to a plugin discovery directory.
- Subprocess tool: add `[[tools.subprocess]]` configuration.
- MCP server: use `oli mcp add` for OAuth HTTP servers or add stdio/header
  configuration under `[mcp.servers.<name>]`.
- Frontend: wait for or contribute the session command/event/snapshot boundary;
  do not fork the agent loop into presentation code.

## Intentional boundaries

- Oli currently ships no TUI, desktop, IDE, hosted, or multi-user frontend.
- The core remains text-independent even though the bundled binary is
  text-first.
- Tools execute automatically; deterministic policy denial and hooks are the
  available interception mechanisms.
- New abstractions should follow demonstrated consumers rather than speculative
  framework design.

For modification conventions and exact test commands, read
[`AGENTS.md`](../AGENTS.md). For the active structural plan, read
[`specs/architecture-polish-plan.md`](../specs/architecture-polish-plan.md).
