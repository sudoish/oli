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

Tip-of-master at last update: **`1e33058`**.
Tests: **68 unit tests, all green.** Release build: clean, zero warnings.

## What works today

**CLI:** `codecrafters-claude-code -p "prompt"`. Single-shot.
- OpenRouter via env vars (`OPENROUTER_API_KEY`, `OPENROUTER_BASE_URL`),
  model `anthropic/claude-haiku-4.5` by default.
- TOML config at `~/.config/agent/config.toml` overrides defaults if
  present. Config supports multiple named providers, all of `kind =
  "openai-compat"` for now.

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
all five with a config flip.

**Test seam:** `FakeProvider` lets the entire agent loop be unit-tested
without touching the network. `ScriptedProviderHandle` wraps an
`Arc<FakeProvider>` so tests can both feed responses and inspect the
captured request stream.

## Architecture as built

```
src/
├── main.rs              # 56 LOC, just wiring
├── agent/
│   ├── mod.rs           # Agent loop, system_prompt prepend
│   └── context.rs       # SystemPromptBuilder
├── providers/
│   ├── mod.rs           # Provider trait + ChatRequest/ChatResponse
│   ├── openai_compat.rs # OpenAICompatProvider
│   └── fake.rs          # FakeProvider (cfg(test))
├── tools/
│   ├── mod.rs           # Tool trait + Registry
│   ├── context.rs       # ToolContext + SessionState
│   ├── util.rs          # truncate + DEFAULT_MAX_OUTPUT_BYTES
│   ├── read.rs / write.rs / edit.rs / bash.rs / grep.rs / glob.rs
├── config.rs            # TOML loader + env-default fallback
└── error.rs             # AgentError + ToolError via thiserror
```

Per-spec architecture sketch except: `agent/compact.rs` (Phase 1d),
`providers/anthropic.rs` (Phase 4), `policy/`, `plugins/`, `repl/` not yet
written.

## What's NOT done

Gaps a daily-driver user would hit today:

- **Single-shot only.** No interactive mode, no multi-turn conversation.
- **No streaming.** Waits for full response per turn.
- **No token tracking, no auto-compact.** Long sessions will blow up the
  model's context window.
- **No tool-call fallback parser.** Local models that emit tool calls as
  text rather than structured `tool_calls` will fail.
- **No model-capability registry.** No way to know if a model needs the
  fallback parser or its real context window size.
- **No policy / permission engine.** `Bash` runs whatever the model wants.
- **No slash commands** (`/clear`, `/model`, `/provider`, `/cost`, etc.).
- **No subprocess tools** (MCP-lite from Phase 2).
- **No plugin runtime** (Phase 3).
- **No native Anthropic provider** with prompt caching (Phase 4).
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

## Open decisions still on the table

- **Streaming approach:** `async-openai`'s stream API vs raw `reqwest`
  for finer control. Decide before Phase 1c.
- **Plugin scripting language:** Lua via `mlua` is the spec's lead
  candidate. Final call needed before Phase 3.
- **Plugin async model:** native `mlua` async vs sync-with-host-driving.
  Affects how natural `ctx:prompt(...)` feels to plugin authors.
- **Native Anthropic provider:** only worth writing for prompt caching.
  Defer until Phase 4.
- **TS plugins via Deno:** deferred. Plugin contract designed to be
  host-language-agnostic so a second runtime can be added later without
  breaking existing Lua plugins.

## Picking up from here

Fresh-context boot sequence:

1. Read `specs/README.md` — the vision and full roadmap.
2. Read this file — current state.
3. `git log --oneline -10` — phase boundaries with commit SHAs.
4. `cargo test` — confirm 68 tests green.
5. `cargo build --release` — confirm clean build.
6. Pick the next phase below.

### Next up

**Phase 1c — REPL + streaming.** Biggest UX shift remaining in Phase 1.
- `rustyline` for interactive input.
- Switch to streaming chat completions; print assistant tokens as they
  arrive.
- Multi-turn message history retained across user inputs.
- Ctrl-C handling that cancels the current turn without exiting.
- First slash command(s): `/clear`, `/help`. Lays the groundwork for a
  `SlashCommand` trait + registry mirroring `Tool` / `Provider`.

Estimated ~400 LOC. After 1c, this stops being a one-shot script.

**Phase 1d — local-model survival kit.** Smaller. Ship after 1c.
- Token tracking from `usage` field on responses.
- Auto-compact: when nearing the model's context window, summarize older
  turns into a single message.
- Model-capability registry: hardcoded prefix → `{ ctx_window,
  supports_native_tool_calls, supports_streaming_tool_deltas }` map.
- Tool-call fallback parser: if `tool_calls` is empty but content looks
  like a tool call (`<tool_call>{...}</tool_call>`, fenced JSON), parse
  and dispatch.

After 1c+1d, the harness is a credible daily driver against
`qwen2.5-coder:7b` on Ollama or Claude via OpenRouter.

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
