# Manual verification plan

Unit tests cover the core. This plan captures the live checks worth running
before releases or after large architectural moves.

## Baseline commands

Run these first:

```sh
OLI_VERIFICATION_CONFIG="$(mktemp -d)"
export XDG_CONFIG_HOME="$OLI_VERIFICATION_CONFIG"
trap 'rm -rf -- "$OLI_VERIFICATION_CONFIG"' EXIT
cargo test
cargo build --release
cargo doc --no-deps --lib
./target/release/oli --help
./target/release/oli init --provider ollama --force
./target/release/oli run --output json -p "say hello" --max-turns 3
```

The temporary `XDG_CONFIG_HOME` keeps `init --force`, credentials, sessions,
and other verification state out of the user's real configuration.

## Headless CLI contract

Verify:

```sh
./target/release/oli run -p "say hello" --max-turns 3
./target/release/oli run --output json -p "say hello" --max-turns 3
printf 'say hello' | ./target/release/oli run --max-turns 3
```

Expected:

- text mode prints final response to stdout
- text mode prints conversation id to stderr
- JSON mode emits one JSON object
- failures are non-zero and machine-readable in JSON mode

## Provider smoke tests

### Ollama

Prereq: Ollama running with a model pulled.

```sh
./target/release/oli init --provider ollama --force
./target/release/oli run --output json -p "Read Cargo.toml and tell me the package name" --max-turns 8
```

Expected: model uses `Read` or answers correctly; run completes or fails with a
clear provider/model error.

### OpenRouter / OpenAI-compatible

Prereq: API key env var configured.

```sh
OPENROUTER_API_KEY=... ./target/release/oli run --output json -p "say hello" --max-turns 3
```

Expected: request succeeds and usage/cost fields are populated when provider
reports them.

### Anthropic native

Prereq: `ANTHROPIC_API_KEY` and config provider entry.

```sh
./target/release/oli run --output json -p "say hello" --provider anthropic --max-turns 3
```

Expected: streaming/tool shapes work; `/model` in REPL lists native models when
available.

### ChatGPT subscription auth

Prereq: successful `oli login`.

```sh
./target/release/oli login --check
./target/release/oli run --output json -p "say hello" --provider chatgpt --max-turns 3
```

Expected: token refresh works, models are discovered, fallback messaging names
API-key alternatives on auth failure.

## REPL checks

Start:

```sh
./target/release/oli
```

Verify slash commands:

- `/help`
- `/tools`
- `/cost`
- `/memory`
- `/paths`
- `/diagnostics`
- `/config reload`
- `/plugins reload`
- `/sessions`
- `/exit`

Expected: no panic; errors appear in diagnostics where appropriate.

## Frontend contract

Run the shared runtime contract tests with a non-rendering test frontend.

Verify:

- prompts produce ordered content, tool, and completion events;
- events own their payloads and can cross a channel or process bridge;
- cancellation preserves the same memory rollback invariant as the line REPL;
- session, provider, model, tool, MCP-health, and cost state are available as
  snapshots rather than parsed terminal output;
- line and headless frontends do not duplicate agent lifecycle rules.

Expected: a future TUI or desktop adapter can drive a session without importing
rustyline, capturing stdout/stderr, or accessing private `Agent` internals.

## Automatic tool execution

Verify that the line REPL and ordinary headless runs execute read, edit, and
shell tools without pausing for permission input. Then run a tool-using prompt
with `oli run --strict`.

Expected: ordinary runs remain fully automatic; strict mode returns
deterministic policy-denied tool results and never executes a tool or waits for
input.

## Edit safety

### Resume read-set

1. Ask oli to read a file.
2. Exit.
3. Resume the conversation.
4. Ask oli to edit the same file.

Expected: edit is allowed without forcing a fresh read solely because of resume.

### External mutation

1. Ask oli to read a file.
2. Modify the file externally.
3. Ask oli to edit the file.

Expected: oli refuses and asks for a re-read instead of clobbering.

## Plugin reload

1. Add a simple Lua plugin under `~/.config/oli/plugins/`.
2. Start REPL.
3. Run `/plugins`.
4. Edit the plugin.
5. Run `/plugins reload`.
6. Ask the model to use the changed tool.
7. Break the plugin intentionally.
8. Run `/plugins reload` and `/diagnostics`.

Expected: reload is atomic; broken plugin does not crash the process; failure is
visible in diagnostics.

## MCP checks

For one stdio and one HTTP MCP server when available:

- startup health appears in `/mcp`
- tools appear in `/tools`
- a tool can be called
- failed server can be restarted with `/mcp restart <name>`
- `tools/list_changed` refresh is reflected without process restart

Expected: MCP failures degrade gracefully and diagnostics capture useful context.

## Compaction / long-session check

Run a long enough session to approach the configured budget.

Verify:

- `/memory` shows pressure increasing
- compaction happens before overflow
- summary remains coherent
- ledger records compaction metrics
- no pinned system prompt duplication

## Replay checks

Run available fixtures:

```sh
./target/release/oli replay --fixture <fixture.json>
```

Expected:

- fixture is not modified
- comparison reports cost/latency/context differences
- output is deterministic enough for regression review

## Release gate

Before tagging a release, complete:

- [ ] `cargo test`
- [ ] `cargo build --release`
- [ ] `cargo doc --no-deps --lib`
- [ ] headless text run
- [ ] headless JSON run
- [ ] one real provider smoke
- [ ] automatic and strict tool execution if policy changed
- [ ] plugin reload if plugins changed
- [ ] MCP smoke if MCP changed
- [ ] replay fixture if memory/ledger/provider shaping changed
- [ ] docs updated for user-facing behavior
