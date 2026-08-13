# AGENTS.md

oli — a minimal, hackable, scriptable coding-agent runtime. Rust 2024,
MSRV 1.95. This file is auto-loaded as project context by oli (and other
agent harnesses that read AGENTS.md), so it doubles as the project's
self-description: when oli is asked about itself in this repo, this is
what it knows.

User-facing build / config docs live in `README.md`. Architectural specs
live under `specs/`. This file is for agents *modifying the codebase*.

## Build & test

- `cargo build` — the single text-first binary.
- `cargo test --bin oli` — CLI parsing and headless result-contract tests.
- `cargo test --lib` — full test suite (~500 tests, ~2s wall clock).
- `cargo test --lib <module::path>` — single test or module.
- No CI lint beyond compiler warnings; match the surrounding style.

## Discipline

- **TDD.** Write the failing test first. The test loop is fast enough
  to keep this tight — exploit it.
- Comments only when the *why* is non-obvious. No multi-paragraph
  docstrings. Don't restate what well-named code already says, don't
  reference the current task or call sites (those rot).
- Don't add backwards-compat shims, feature flags for hypothetical
  futures, or speculative extension points. Change the code.
- Don't add error handling for cases that can't happen.
- Bug fixes don't get surrounding cleanup. New features don't get
  refactors of adjacent code. One concern per change.
- Match existing patterns. If something already does what you need,
  reuse it; don't fork.

## Module map (`src/`)

| Path | What lives there |
|---|---|
| `agent/` | think→call→observe loop, `Memory` trait, system-prompt builder (`agent/context.rs`), capability table (`agent/caps.rs`) |
| `bin/oli.rs` | CLI entry point; wires startup, registers tools and hooks |
| `bootstrap.rs` | shared startup and persisted-session wiring |
| `config.rs` | layered TOML loader (global `~/.config/oli/config.toml` + project `.oli/config.toml` walked up from cwd) |
| `diagnostics.rs` | operational warning ring buffer (surfaced via `/diagnostics`) |
| `hooks/` | `PreToolUse` / `PostToolUse` / `Stop` event dispatch |
| `ledger/` | per-request accounting: provider-neutral preflight estimate, context attribution, latency, dated `[[pricing]]`; writes `<session>.ledger.jsonl` beside the transcript |
| `mcp/` | Model Context Protocol clients (stdio + SSE) |
| `notes/` | cross-session note store (filesystem, TOML frontmatter) |
| `plugins/` | Lua runtime (`mlua`), discovery dirs, hot-reload |
| `policy/` | `auto_allow` / `ask` / `bash_allowlist` gating + persisted allow-list |
| `providers/` | `Provider` trait + `anthropic`, `openai_compat` (covers Ollama / OpenRouter / OpenAI / LM Studio / vLLM / llama.cpp), `fake` (tests) |
| `repl/` | line-mode REPL + `SlashRegistry` + built-in slash commands |
| `tools/` | built-in tools: `read`, `write`, `edit`, `bash`, `grep`, `glob`, `task` (subagent), `notes`, `subprocess` (config-defined external binaries) |
| `wizard_init.rs` | first-run config wizard |

## Where to add what

| Goal | Where |
|---|---|
| New tool | `src/tools/<name>.rs` impl `tools::Tool` (trait at `src/tools/mod.rs:47`); register in `src/bin/oli.rs` startup. |
| New provider | `src/providers/<name>.rs` impl `Provider` (trait at `src/providers/mod.rs:125`); wire into `providers::build()` (`src/providers/mod.rs:36`). |
| New slash command | `src/repl/slash.rs`: struct + `impl SlashCommand`; register in `SlashRegistry::default_set_with_reloader`. |
| New hook event | `src/hooks/`. Existing dispatcher fires `PreToolUse` / `PostToolUse` / `Stop`. |
| Capability override for a model | `[[caps]]` block keyed by model-id `prefix` in user config, layered over built-in defaults in `src/agent/caps.rs`. |
| Plugin (no rebuild) | drop `.lua` into `~/.config/oli/plugins/` (global) or `<project>/.oli/plugins/`. Plugins can register tools, slashes, and hooks. Sandbox strips `os` / `io` / `package.loadlib`; filesystem and shell go through the policy gate. |
| Project-scoped agent instructions | `AGENTS.md` and/or `CLAUDE.md` at any directory between cwd and filesystem root. Both auto-load into the system prompt. User-level overlays: `~/.codex/AGENTS.md`, `~/.claude/CLAUDE.md`. |

## Testing patterns

- Tests live in `#[cfg(test)] mod tests {}` at the bottom of each
  module. Editing the existing test next to the change beats adding a
  parallel test file.
- Use `tempfile::tempdir()` for filesystem state. Never write into the
  user's real `~/.config/oli` from a test.
- Async tests: `#[tokio::test]`.
- Provider tests: `crate::providers::fake::FakeProvider` replays a
  queued list of responses.
- Test names are full sentences describing behavior
  (`config_reload_picks_up_default_provider_change`,
  not `test_reload`).

## Runtime introspection — use these before guessing

- `/paths` — resolved on-disk locations for config, plugins, sessions,
  notes, policy allow-list. Source-of-truth, computed at runtime from
  the same code that loads each file.
- `/tools` — registered tools (built-ins + plugin + MCP).
- `/plugins` / `/plugins reload` — list loaded plugins; `reload`
  re-scans dirs and swaps tools/hooks/slashes atomically.
- `/config reload` — re-read config and apply provider/model/policy
  changes in place without losing memory.
- `/system` — show the pinned system prompt (env, git, dir listing,
  AGENTS.md/CLAUDE.md content).
- `/sessions` — list saved sessions; resume with `oli run --conversation <id> -p "..."`.
- `/diagnostics` — operational warnings (plugin load failures, MCP
  errors, provider quirks).

## Gotchas

- The system prompt is pinned via `Agent::pin_system_prompt`; it
  survives `/clear` and compaction. Don't re-pin per turn.
- `Memory::pinned()` returns the pinned slice — readers must not
  treat it as a full snapshot.
- `SlashOutcome::Rebuild` is the only way to swap registered slashes
  after startup (used by `/plugins reload`).
- Policy fingerprints are `tool-name + canonical-JSON args`. Reordering
  map keys in args is fine; renaming a tool invalidates the entry.
- The line REPL builds its registry via `default_set_with_reloader`.
- Don't bypass the policy gate for "trusted" callers. If something
  should be auto-approved, that's an `auto_allow` config entry, not a
  code-path special case.
- The system prompt's project-context loader walks **up** from cwd, not
  down — `AGENTS.md` at the repo root is found from any subdirectory.
