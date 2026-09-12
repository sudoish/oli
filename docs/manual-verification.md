# Manual verification

Use this checklist before a release or after architectural changes. Unit tests
remain the first gate; these checks cover real providers, terminals, credentials,
processes, and persistence that hermetic tests cannot fully prove.

Do not put API keys in the repository. Supply them through the documented
environment variables or owner-only user configuration.

Run the entire checklist with an isolated configuration so destructive setup
commands and test credentials cannot affect the user's real configuration:

```sh
OLI_VERIFICATION_CONFIG="$(mktemp -d)"
export XDG_CONFIG_HOME="$OLI_VERIFICATION_CONFIG"
trap 'rm -rf -- "$OLI_VERIFICATION_CONFIG"' EXIT
```

## Baseline

```sh
cargo test
cargo build --release
cargo doc --no-deps --lib
./target/release/oli --help
./target/release/oli run --output json -p "say hello" --max-turns 3
```

Confirm builds are warning-free, tests pass, help matches the README, JSON mode
prints exactly one object to stdout, and failures exit non-zero.

## Headless contract

```sh
./target/release/oli run -p "say hello" --max-turns 3
./target/release/oli run --output json -p "say hello" --max-turns 3
printf 'say hello' | ./target/release/oli run --max-turns 3
```

- Text mode writes the final answer to stdout and the conversation id to stderr.
- JSON mode keeps stdout machine-clean.
- The `--prompt` argument and piped stdin remain mutually exclusive.
- Resume reuses the requested conversation id.
- Max-turn exhaustion is a typed non-success result and remains resumable.

## Provider smoke tests

### Ollama

Prerequisite: Ollama is running and the configured model is pulled.

```sh
./target/release/oli init --provider ollama --force
./target/release/oli run --output json \
  -p "Read Cargo.toml and tell me the package name" --max-turns 8
```

Confirm the model can use `Read`, completion is coherent, and provider/model
errors identify the failing endpoint.

### OpenRouter

Prerequisite: `OPENROUTER_API_KEY` is set and `default_provider = "openrouter"`
is selected in the active config.

```sh
./target/release/oli run --output json -p "say hello" --max-turns 3
```

Confirm usage and cost are populated when the provider reports usage.

### Anthropic

Prerequisite: `ANTHROPIC_API_KEY`, an Anthropic provider entry, and
`default_provider = "anthropic"` in the active config.

```sh
./target/release/oli run --output json -p "say hello" --max-turns 3
```

Confirm native streaming and tool-call shapes work.

### ChatGPT subscription

Prerequisite: complete `oli login`, which provisions and selects the ChatGPT
provider.

```sh
./target/release/oli login --check
./target/release/oli run --output json -p "say hello" --max-turns 3
```

Confirm model discovery works and an expired access token refreshes without
another interactive login.

## Line REPL

Start `./target/release/oli`, then run:

```text
/help
/tools
/cost
/memory
/paths
/diagnostics
/config reload
/plugins reload
/sessions
/exit
```

Confirm commands do not panic, runtime state survives config reload, and
recoverable failures appear in `/diagnostics`.

## Automatic and strict tool execution

In the line REPL and a normal headless run, ask Oli to read a file and execute a
harmless shell command. Confirm neither path asks for permission.

Then run:

```sh
./target/release/oli run --strict --output json \
  -p "Use Bash to create /tmp/oli-strict-must-not-exist" --max-turns 3
```

Confirm the tool result is a deterministic policy denial, no prompt is shown,
and the file was not created.

## Edit safety

### Resume

1. Ask Oli to read a tracked file.
2. Exit and resume that conversation.
3. Ask Oli to edit the same unchanged file.

Confirm the restored read set permits the edit.

### External mutation

1. Ask Oli to read a file.
2. Modify it outside Oli.
3. Ask Oli to edit it.

Confirm Oli refuses the stale edit and requires a fresh read.

## Plugins

1. Add a small Lua plugin under `~/.config/oli/plugins/`.
2. Start the REPL and inspect `/plugins`.
3. Change the plugin and run `/plugins reload`.
4. Call the changed tool.
5. Introduce a Lua error, reload, and inspect `/diagnostics`.

Confirm reload swaps tools, hooks, and slashes atomically; a broken replacement
does not crash the process or partially replace the working set.

## MCP

### OAuth HTTP

```sh
./target/release/oli mcp add linear https://mcp.linear.app/mcp --read-only
./target/release/oli mcp status linear
```

Complete browser authorization, then confirm status reports a healthy
connection and tools. Start a new Oli session and call a read-only Linear tool.
Use `--paste` when the browser cannot return directly to the machine running
Oli.

### Runtime lifecycle

For one stdio and one streamable-HTTP server:

- server health and tool counts appear in `/mcp`;
- tools appear in `/tools` and can be called;
- `/mcp restart <name>` recovers a failed server;
- `tools/list_changed` updates the registry on a later turn;
- one failed server does not prevent the REPL or other servers from working.

Confirm `/diagnostics` contains enough server and transport context to act on a
failure without exposing credentials.

## Compaction

Run a session long enough to approach its configured context target.

- `/memory` shows increasing pressure.
- Compaction runs before an authoritative hard limit is exceeded.
- The compacted conversation remains coherent.
- The pinned system prompt appears exactly once.
- The ledger records before/after estimates, summarizer usage, and latency.
- A failed compaction retains the unchanged request context.

## Persistence and replay

1. Run a multi-turn session and note its id.
2. Inspect the transcript and adjacent ledger path shown by `/paths`.
3. Resume the session and complete another turn.
4. Run a representative replay fixture:

```sh
./target/release/oli replay --fixture <fixture.json>
```

Confirm records append rather than restart, replay makes no provider call,
fixtures remain unchanged, and comparison output is deterministic.

## Frontend contract

Run `cargo test --lib runtime::` for the non-rendering contract and verify:

- prompts produce ordered content, tool, completion, and error events;
- events own payloads and cross a channel safely;
- cancellation preserves the line REPL's memory rollback behavior;
- snapshots expose session, provider, model, tools, MCP health, provider-reported
  usage, and bounded ledger cost without inventing missing values;
- line and headless frontends do not duplicate lifecycle rules.

A TUI or desktop test adapter should not import rustyline, capture terminal
output, or access private `Agent` fields.

In the real line REPL, start a response and press Ctrl-C. Confirm `(cancelled)`
appears once and the next prompt does not include the cancelled user message.
Cancellation is local and cooperative: a remote provider may still finish work
server-side after its client future is dropped.

## Release gate

- [ ] `cargo test`
- [ ] `cargo build --release`
- [ ] `cargo doc --no-deps --lib`
- [ ] headless text and JSON runs
- [ ] one real-provider smoke test
- [ ] automatic and strict execution when tool dispatch changed
- [ ] edit safety when memory, replay, or tools changed
- [ ] plugin reload when plugins changed
- [ ] stdio and HTTP MCP smoke tests when MCP changed
- [ ] replay fixture when memory, ledger, or provider shaping changed
- [ ] frontend contract when runtime boundaries changed
- [ ] user-facing documentation updated
