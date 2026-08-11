## Why

Oli already has a reusable agent loop, persisted conversations, and a one-shot prompt path, but the default product surface is a large ratatui subsystem whose rendering concerns dominate maintenance. Making persisted headless runs the primary interface lets Oli behave as a scriptable agent runtime while removing the TUI entirely.

## What Changes

- Add a first-class `oli run` command that accepts a prompt, always persists a new or resumed conversation, prints the final response, and returns a stable conversation identifier.
- Allow a later `oli run --conversation <id>` invocation to append to the same conversation with the same provider, tools, policy, hooks, MCP integrations, and project context used by other Oli runs.
- Add text and machine-readable JSON output contracts with strict stdout/stderr separation and non-interactive exit behavior.
- Retain the line-mode REPL as the optional interactive surface, sharing the same runtime and conversation store.
- **BREAKING**: Remove the ratatui TUI, its CLI flags, feature flags, dependencies, rendering configuration, TUI-only tests, and TUI-only documentation. Invoking Oli interactively uses the line REPL rather than a fullscreen or inline UI.
- **BREAKING**: Replace the global `-p/--prompt` one-shot interface with the explicit `oli run` command and persist fresh runs by default so their conversation IDs can be resumed.

## Capabilities

### New Capabilities

- `headless-agent-cli`: Scriptable, persisted agent runs; conversation continuation; output contracts; non-interactive policy behavior; and the removal of the graphical terminal UI.

### Modified Capabilities

- None.

## Impact

The change affects CLI parsing and dispatch, session creation, memory wiring, output serialization, approval handling, startup hooks, Cargo features and dependencies, configuration, README/AGENTS guidance, and tests. The entire `src/tui/` tree and its obsolete design documents are removed. The agent loop, tools, provider implementations, policy gate, MCP clients, plugins, hooks, notes, persisted-memory format, and line REPL remain the shared runtime.
