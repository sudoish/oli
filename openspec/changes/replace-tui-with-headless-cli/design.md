## Context

See `proposal.md` for motivation. Today `src/bin/oli.rs` supports three paths: ephemeral `-p` execution, a line-mode rustyline REPL, and a default ratatui TUI. `bootstrap::resolve_session_id` deliberately gives one-shot prompts no persisted session, while interactive runs create or resume one. Both interactive clients share the same agent construction, but the TUI adds a large driver/render/input/approval subsystem and several optional dependency stacks.

The headless path can already call `Agent::run`; the missing product contract is persisted-by-default execution, stable conversation targeting, machine-safe output, and explicit CLI structure. Existing JSONL transcripts and `PersistedMemory` already provide the required continuation mechanism.

## Goals / Non-Goals

**Goals:**

- Make one-command/one-result execution the primary automation interface.
- Persist every successful fresh run under a stable conversation ID and resume it explicitly later.
- Keep the agent runtime identical across headless runs and the line REPL.
- Remove ratatui and all TUI-specific product, configuration, dependency, and documentation surface.
- Preserve predictable behavior in pipes, CI, benchmarks, and Aperture-routed experiments.

**Non-Goals:**

- Generalizing Oli for a hypothetical second workload in this change.
- Adding a daemon, server, workflow language, or remote API.
- Replacing the JSONL session format or migrating existing sessions.
- Removing the line REPL, slash-command registry, plugin slashes, or readline approvals.
- Adding streaming JSON events; the initial JSON contract contains the completed run result.

## Decisions

### Add `run` as an explicit subcommand

Use `oli run --prompt <text>` for a fresh run and `oli run --conversation <id> --prompt <text>` to continue one. If `--prompt` is absent and stdin is not a terminal, read the complete prompt from stdin. Reject missing, empty, or conflicting prompt sources without opening an interactive reader.

An explicit subcommand separates automation from `init`, `login`, `logout`, and the line REPL. Keeping global `-p` as an alias was rejected because the repository explicitly avoids compatibility shims and this is the point to establish the durable CLI.

### Persist fresh headless runs by default

Change session resolution so a fresh `run` allocates a new session ID before memory construction. Existing `--conversation` IDs open through `PersistedMemory`; `--continue` resolves the newest transcript. The stored JSONL format and replayed read-set remain unchanged.

Ephemeral-by-default was rejected because it makes the requested follow-up command impossible. An opt-out ephemeral flag is excluded until a real privacy or performance use case requires it.

### Give stdout a strict result contract

`--output text` writes only the final assistant response to stdout and writes `conversation: <id>` to stderr after a successful run. `--output json` writes one JSON object containing `conversation_id`, `response`, provider/model identity, and reported usage fields to stdout. Diagnostics and errors always use stderr; JSON mode never mixes progress text into stdout.

The conversation ID is intentionally present in JSON rather than encoded into the text response. Streaming events are deferred because a single completed object is easier to compose and test.

### Make headless approval behavior non-blocking

Headless runs use the existing policy and tool gate but never install `ReadlineApprover`. Auto-allowed and persisted-allowed operations proceed normally. Any operation that still requires a human decision is denied through the non-interactive approver; `--strict` continues to force ask mode so every approval-required operation is denied. The agent may recover from a denied tool within its normal loop.

Bypassing the policy gate was rejected. Reading approval decisions from stdin was also rejected because stdin is a prompt source and automation must never unexpectedly block.

### Retain the line REPL and delete the TUI

The line REPL remains Oli's optional interactive client because it already exercises the same agent, persisted memory, slash registry, plugin commands, progress hooks, and `ReadlineApprover` without owning terminal rendering. Interactive `oli` starts this REPL and prints the session ID.

Delete `src/tui/`; remove `tui`, `syntax-highlight`, and `images` Cargo features and their optional dependencies; remove `--plain`, `--inline`, and `--fullscreen`; remove TUI viewport/theme/mouse/OSC52 configuration; and remove or rewrite documentation that describes TUI-only behavior. The first-run flow uses the existing headless `oli init` path rather than the TUI wizard.

Removing both interactive clients was rejected because it would also remove useful slash commands and approval workflows without being necessary for the headless-runtime goal. Keeping the TUI behind a feature flag was rejected because it preserves the maintenance surface the change is intended to eliminate.

### Keep runtime assembly shared

Extract only enough binary wiring to make the headless and REPL dispatch paths testable without duplicating agent construction. Provider selection, tools, Task subagents, plugins, MCP handles, hooks, policy, system prompt, memory, and read logging are built once and passed to the selected client.

This change does not introduce a general framework abstraction. The coding runtime is the proven workload; a second real workload must justify any later capability-profile abstraction.

## Risks / Trade-offs

- [Existing users rely on fullscreen/inline UI behavior] → Treat this as an explicit breaking release, document the line-REPL and `run` replacements, and remove misleading TUI docs in the same change.
- [Conversation IDs on stderr are awkward for scripts] → Provide the JSON output contract as the supported machine interface.
- [Headless ask-mode tools silently fail to execute] → Emit bounded diagnostics on stderr, preserve distinct failure context, and document `auto_allow`/persisted policy as the automation mechanism.
- [Fresh persisted runs create many session files] → Keep the existing session listing and storage model; cleanup policy is a separate concern.
- [TUI code has concurrent uncommitted work at implementation time] → Resolve ownership before deleting the tree; do not overwrite or discard unrelated local changes while applying this plan.
- [Removing rendering dependencies breaks non-UI shared helpers] → Identify any genuinely reusable logic first and move it under a neutral module with tests before deleting the TUI tree.

## Migration Plan

1. Add and test the persisted `run` command and output contracts while the current clients still compile.
2. Route interactive invocation exclusively to the line REPL and remove TUI CLI/config selection.
3. Remove the TUI module, feature/dependency graph, and obsolete tests; move only proven shared helpers.
4. Update user and agent documentation, examples, package metadata, and release notes for the breaking interface.
5. Verify new and resumed text/JSON runs, the line REPL, all authentication modes, MCP/plugins, default and minimal builds, and one real Aperture-routed continuation.

Rollback is a source revert of the breaking release. Persisted sessions require no data rollback because the JSONL format is unchanged.
