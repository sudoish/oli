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
| _next_  | 5b    | `HttpTransport` for streamable-http (POST + JSON or SSE response, `Mcp-Session-Id` capture/echo, env-expanded user headers), `/mcp restart <server>` (re-spawn + re-handshake in place; existing `Arc<Mutex<McpServer>>` tool registrations stay valid), `auto_allow_pure_reads` policy heuristic (auto-allow MCP tools whose bare name starts with `get_`/`list_`/`search_`/`fetch_`/`read_`/`describe_`/`show_`/`find_`/`query_`; default true; off via `[policy].auto_allow_pure_reads = false`) |

Tip-of-master at last update: **Phase 5b (this commit)**.
Tests: **244 unit tests, all green** (was 231). Release build: clean.
13 new tests cover the HTTP transport (unary JSON, SSE, error
envelopes, session-id capture, user-header forwarding, 202 notify
acks via an inline tokio TCP fake server), the restart lifecycle (a
second handshake against the Python fake plus a Down-failure path),
and the pure-reads policy heuristic (auto-allow on get/list/search,
no escape on save/delete/create, off-flag falls through, doesn't
trigger on non-MCP tools).

## What works today

**Phase 5b — MCP client extensions:**
- `HttpTransport` (`src/mcp/http.rs`) for streamable-http servers.
  Uses the existing `reqwest::Client` + `eventsource-stream` crates.
  Each request is a POST with `Accept: application/json,
  text/event-stream`; the client handles both response shapes (single
  JSON body for unary RPCs, SSE for streaming) and picks the response
  whose `id` matches the outgoing request out of the SSE stream
  (server-initiated notifications and unrelated requests are dropped
  silently). The transport captures the first `Mcp-Session-Id`
  response header it sees and echoes it on every subsequent call so
  servers that require session affinity work without explicit user
  config. User headers (typically `Authorization`) are forwarded with
  `${VAR}` expansion handled at config-load time.
- `/mcp restart <server>` re-runs the connect + initialize sequence
  in place. The existing `Arc<Mutex<McpServer>>` shared with every
  registered `McpTool` keeps tool entries valid across the restart;
  the user sees the new health state and tool count in the response.
  Restart of a misconfigured server leaves it `Down(reason)` rather
  than vanishing.
- `auto_allow_pure_reads` policy heuristic (default true). MCP tools
  named `<server>__<bare>` whose bare verb starts with `get_`,
  `list_`, `search_`, `fetch_`, `read_`, `describe_`, `show_`,
  `find_`, or `query_` are auto-allowed without an explicit
  allow-list entry. Save/delete/create/update verbs still drop into
  the `Ask` path. The heuristic intentionally only fires on MCP-
  shaped names (`__` separator), so a custom plugin tool named
  `get_thing` keeps the safe Ask default. Off via
  `[policy].auto_allow_pure_reads = false`.

**Phase 5a — MCP client:**
- `src/mcp/` module with `transport.rs` (trait), `stdio.rs`
  (newline-delimited JSON-RPC 2.0 over child stdio), `server.rs`
  (lifecycle + parsed metadata), `tool.rs` (registry adapter),
  `config.rs` (TOML schema + env expansion + glob-style allow/deny).
- Stdio transport spawns the configured `command` with a minimal env
  carried over from the harness (`PATH`, `HOME`, `USER`, `LANG`,
  `TMPDIR`, `TERM`) layered with the user's explicit `env` table. A
  background reader task parses each stdout line as a JSON-RPC
  message, demuxes responses by id into per-call `oneshot` waiters,
  and drops server-initiated notifications (v1 doesn't subscribe to
  resources or implement sampling). Stderr is captured into a
  ring-trimmed 64 KB buffer so a chatty server can't OOM us.
- `McpServer::connect` runs initialize → notifications/initialized →
  tools/list with per-call timeout (default 5s init, 60s call). A
  failure flips the server to `HealthState::Down(reason)` and the REPL
  proceeds — the failed server's tools simply don't register.
- `McpTool` adapts each server-side tool into the harness's `Tool`
  trait. Name is namespaced as `<server>__<tool>` (matches Claude.ai's
  convention; servers can collide on bare names). `inputSchema` passes
  through to `openai_schemas()` verbatim — no translation layer. Tool
  results are unwrapped from MCP's `content` array (text blocks
  concatenated, non-text blocks marked, `isError: true` prefixed).
- `[mcp.servers.<id>]` config tables with `kind = "stdio"` (working)
  or `"streamable-http"` (reserved — fails fast). Per-server `tools`
  filter with `allow` + `deny` glob lists. `enabled = false` lets a
  project overlay disable a globally-defined server. `${VAR}`
  expansion in `env` values resolves at spawn time; missing vars fail
  the server with a clear error naming the var.
- `/mcp` slash command: bare lists configured servers with health and
  tool counts; `/mcp tools <server>` enumerates that server's exposed
  tools (post-filter); `/mcp logs <server>` dumps captured stderr.
- Subagents inherit MCP tools through a shared `Arc<Vec<McpHandle>>`
  in `AgentSpawner` so spawning a child doesn't re-dial servers.
- Policy gating: MCP tools fall through to the default `Ask` branch
  in `ConfigPolicy` (since `<server>__<tool>` isn't in `auto_allow`),
  matching the spec's "approve once per session" guidance until the
  phase-5b `auto_allow_pure_reads` heuristic lands. Hooks fire as
  normal — `PreToolUse`/`PostToolUse` see the namespaced name.



**CLI:** Two modes off the same binary.
- `oli -p "prompt"` — single-shot, non-streaming,
  prints final assistant content. Same scripted-friendly behavior as before.
- `oli` (no `-p`) — interactive REPL with streaming
  output, multi-turn history, `/clear` / `/help` / `/exit`, Ctrl-C cancels
  the in-flight turn (history rolls back even after compaction), Ctrl-D
  exits.
- OpenRouter via env vars (`OPENROUTER_API_KEY`, `OPENROUTER_BASE_URL`),
  model `anthropic/claude-haiku-4.5` by default.
- TOML config at `~/.config/oli/config.toml` overrides defaults if
  present. Config supports multiple named providers, all of `kind =
  "openai-compat"` for now.

**Phase 4 — native Anthropic + polish:**
- Top-level agent `max_turns` (default 40) bounds the parent loop so a
  flaky fallback-parsed model can't spin forever. Configurable via
  `[agent].max_turns` and overridable per-run with `--max-turns`.
- Per-project `.oli/config.toml` overlay. `Config::load_or_default`
  walks up from cwd, finds the nearest project config, and merges it
  over `~/.config/oli/config.toml`: tables merge per-key (overlay
  scalar wins on leaves), arrays concatenate with project entries
  first (so `[[caps]]` shadows globals in lookup order). API keys stay
  in the global file; project configs stay credential-free.
- Diff preview for `Edit` and `Write` in the approval prompt. The
  `ReadlineApprover` renders tool-aware previews — old/new strings
  for `Edit`, line-truncated content + size for `Write` — so the
  user sees what's about to land before answering y/N.
- Expanded plugin host API. `ctx:tool` plus six new methods:
  `ctx:read_file`, `ctx:write_file`, `ctx:shell` (all dispatch through
  the host's tool registry), `ctx:prompt` (uses the bound
  `SubagentSpawner` to run a fresh agent loop), `ctx:get_state` /
  `ctx:set_state` (per-plugin per-session HashMap), `ctx:ask_user`
  (blocking stdin read on a tokio blocking task).
- `NotesStore` trait + `FilesystemNotesStore` default. Markdown files
  with TOML frontmatter under `~/.config/oli/notes/<id>.md`.
  Distinct from active-context `Memory` because retrieval failures
  here don't poison the live conversation. Three tools surface to the
  model: `WriteNote`, `SearchNotes` (substring + tag filter), `ListNotes`.
- Native Anthropic provider (`src/providers/anthropic.rs`). Direct
  reqwest + SSE. Bidirectional shape conversion: OpenAI-shaped
  `ChatRequest` → Anthropic `system` field + tool_use/tool_result
  blocks → OpenAI-shaped `ChatResponse`. Streaming handles
  `text_delta` and `input_json_delta` events with per-block
  accumulation. Prompt caching is the reason this provider exists:
  `cache_control: ephemeral` lands on the system prompt and on the
  last tool definition (cache breakpoint), so long sessions hit cache
  on everything before the new user message.

**Phase 3 — power features:**
- Session persistence in `src/agent/memory/persisted.rs`. `PersistedMemory`
  decorates an inner `Memory` and mirrors every `record` / `pin` /
  `clear` / `truncate` to JSONL at `~/.config/oli/sessions/<id>.jsonl`.
  On open, prior content replays into the inner memory before any new
  writes — sessions resume verbatim.
- CLI flags: `--resume <id>`, `--continue` (latest by mtime). REPL
  always persists; `-p` is ephemeral unless one of those flags is
  passed. `/sessions` slash command lists recent ids.
- Hook dispatcher (`src/hooks/`). One trait + registry shared between
  built-in hooks and Lua plugin-registered ones. Three events fire from
  the agent loop: `PreToolUse` (before policy), `PostToolUse` (after
  dispatch, with result), `Stop` (on final assistant content).
- Subagent (`Task` tool) + `SubagentSpawner` trait. Spawns a child
  agent with isolated memory and a turn cap; returns only the final
  summary. The same trait will power plugin `ctx:prompt(...)` later.
- Lua plugin runtime (`src/plugins/`) via mlua (lua54+vendored+async+
  send+serialize). Auto-discovers `~/.config/oli/plugins/*.lua` and
  `./.oli/plugins/*.lua`. Sandbox strips `os`, `io`, `dofile`,
  `loadfile`, `require`, `debug`, and `package.loadlib`/`cpath`/`path`.
  Plugins register tools, slash commands, and hooks via a `plugin`
  table return value. `ctx:tool(name, args)` async-bridges into the
  harness's tool registry; `ctx:log(level, msg)` prints to stderr.
- `/plugins` slash command lists loaded plugins with their registered
  components. (`/plugins reload` deferred — needs registry-rebuild
  semantics worked out first.)

**Phase 2 — flexibility surface:**
- Policy engine in `src/policy/`. `Policy::check(tool, args) -> Allow | Ask
  | Deny`; `Approver::approve(...)` resolves `Ask` outcomes asynchronously.
  Default `ConfigPolicy` reads `[policy]` from config (auto_allow / ask /
  bash_allowlist; baked-in defaults cover Read/Glob/Grep auto, Edit /
  Write / Bash ask, common dev shell commands on the bash allowlist).
- Three approvers ship: `AlwaysApprove` (default + scripted `-p`),
  `ReadlineApprover` (REPL — prompts y/N via stdin on a blocking task),
  `AlwaysDeny` (testing + future strict-mode flag).
- Slash command set expanded: `/cost`, `/tools`, `/system`, `/memory`
  (subcommands `stats` / `dump`), `/compact`, `/provider`, `/model`.
  `/provider <name>` swaps to a fresh `OpenAICompatProvider` from config
  and recomputes caps; `/model <id>` swaps within the active provider.
- Subprocess tools (MCP-lite). `[[tools.subprocess]]` config entries
  register external binaries that speak JSON over stdio. Args go in via
  stdin, stdout becomes the tool result, non-zero exits surface stderr
  to the model.
- Per-config caps overrides. `[[caps]]` entries with a `prefix` field
  shadow the hardcoded `caps.rs` registry, so a custom Ollama-tagged
  model whose context window or tool-call support differs from the
  family default can be made first-class without recompile.

**Local-model survival kit (Phase 1d):**
- `Memory` trait (`src/agent/memory/`) replaces the flat `Vec<Value>`. Default
  `LinearWithCompact` keeps today's behavior plus auto-summarization when
  `current_tokens > target_tokens`.
- Compaction summarizes the older half via `provider.chat`, snapping to a
  user-message boundary so tool-call/tool-result pairs aren't split.
- `Memory::len` is monotonic across compaction events — REPL Ctrl-C
  rollback works even after compaction has run mid-session.
- Token tracking via `stream_options.include_usage`. Captured per-call as
  `agent.last_usage`; feeds the compaction trigger.
- Model-capability registry (`src/agent/caps.rs`) — hardcoded prefix → `{
  ctx_window, supports_native_tool_calls, supports_streaming_tool_deltas
  }`. `compact_target() = 80% of ctx_window`. qwen / llama / claude /
  gpt families covered, conservative default for anything unknown.
- Tool-call fallback parser (`src/agent/tool_parse.rs`) — for models
  flagged `supports_native_tool_calls = false`, scans assistant content
  for embedded JSON tool calls (bare, fenced, `<tool_call>`-tagged) and
  splices them onto the assistant message before the agent loop dispatches.
  Synthesizes globally-unique ids so subsequent tool results match.

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
all five with a config flip. The `Provider` trait carries both `chat`
(non-streaming) and `chat_stream(req, sink)` where `sink: &mut dyn
FnMut(&str) + Send` is re-borrowed across loop iterations. Streaming
assembles tool calls across SSE deltas by `index`, so OpenRouter / Ollama
fragmented tool-call streams converge to a single `tool_calls` array.

**REPL:** `repl::run(agent)` owns the loop. Rustyline runs in
`spawn_blocking` per iteration so the runtime is free for Tokio's signal
handler during in-flight turns. `tokio::select!` races the agent's
streaming future against `tokio::signal::ctrl_c()`; on cancel, history is
truncated to its pre-turn length so the next turn doesn't see a
half-finished assistant message.

**Slash commands:** `SlashCommand` trait + `SlashRegistry` mirror
`Tool` / `Registry`. Built-ins: `/clear`, `/help`, `/exit`. `/help` is
rendered against the live registry rather than from the command's own
`run`, so adding a command only requires `register(...)`.

**Test seam:** `FakeProvider` lets the entire agent loop be unit-tested
without touching the network. `ScriptedProviderHandle` wraps an
`Arc<FakeProvider>` so tests can both feed responses and inspect the
captured request stream.

## Architecture as built

```
src/
├── main.rs              # CLI: -p one-shot or REPL
├── agent/
│   ├── mod.rs           # Stateful Agent: messages, run, run_streaming, clear
│   └── context.rs       # SystemPromptBuilder
├── providers/
│   ├── mod.rs           # Provider trait + chat / chat_stream + ContentSink
│   ├── openai_compat.rs # OpenAICompatProvider with SSE streaming
│   └── fake.rs          # FakeProvider (cfg(test)), 2-chunk streaming for tests
├── repl/
│   ├── mod.rs           # rustyline + tokio::select Ctrl-C cancel
│   └── slash.rs         # SlashCommand trait + /clear /help /exit
├── tools/
│   ├── mod.rs           # Tool trait + Registry
│   ├── context.rs       # ToolContext + SessionState
│   ├── util.rs          # truncate + DEFAULT_MAX_OUTPUT_BYTES
│   ├── read.rs / write.rs / edit.rs / bash.rs / grep.rs / glob.rs
├── config.rs            # TOML loader + env-default fallback
└── error.rs             # AgentError + ToolError via thiserror
```

Per-spec architecture sketch except: `agent/compact.rs` (Phase 1d),
`providers/anthropic.rs` (Phase 4), `policy/`, `plugins/` not yet written.

## What's NOT done

Gaps a daily-driver user would hit today:

- **No `--strict-mode` / config flag for `AlwaysDeny`.** Today the `-p`
  one-shot path uses `AlwaysApprove`. A user who wants scripted runs
  with strict policy (any `Ask` decision becomes `Deny`) has to swap
  approvers programmatically.
- **No alternative `Memory` strategies shipped.** Default
  `LinearWithCompact` is the only impl in tree; `EmbeddingRAG` /
  `GraphBacked` / `HierarchicalSummary` are sketched in `specs/memory.md`
  but not implemented.
- **No `/plugins reload`.** Listing works; reload was deferred from
  Phase 3 and Phase 4 — needs a registry-rebuild refactor (the slash
  registry isn't on Agent yet, so reload from inside a slash dispatch
  can't swap it out cleanly).
- **Hooks are observe-only.** `PreToolUse` cannot veto a tool call;
  policy is the only gating path.
- **Native Anthropic provider live-call not exercised.** Shape
  conversion is unit-tested. The HTTP / SSE path needs a real
  ANTHROPIC_API_KEY to verify end-to-end; the user can flip
  `kind = "anthropic"` in config and try.
- **Diff preview is JSON-style, not unified diff.** It shows old/new
  strings rather than a context-aware unified diff. Adequate for the
  small Edit calls the model typically makes; would benefit from a
  proper diff lib for large changes.

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

## Open decisions still on the table

- **Streaming approach:** decided — `async-openai`'s `create_stream_byot`
  with our own SSE-delta assembly. Worked first try; haven't yet hit a
  case where raw `reqwest` would be needed.
- **Plugin scripting language:** Lua via `mlua` is the spec's lead
  candidate. Final call needed before Phase 3.
- **Plugin async model:** native `mlua` async vs sync-with-host-driving.
  Affects how natural `ctx:prompt(...)` feels to plugin authors.
- **Native Anthropic provider:** only worth writing for prompt caching.
  Defer until Phase 4.
- **TS plugins via Deno:** deferred. Plugin contract designed to be
  host-language-agnostic so a second runtime can be added later without
  breaking existing Lua plugins.
- **REPL feedback during tool rounds:** today the REPL is silent while
  the model is dispatching tools (only model content streams to stdout).
  Open whether to surface a small `→ Read(...)` line per tool call. Lean
  toward yes once Phase 2 brings the policy engine — the prompts will
  need that context anyway.

## Picking up from here

Fresh-context boot sequence:

1. Read `specs/README.md` — the vision and full roadmap.
2. Read `specs/memory.md` — the active-context memory architecture.
3. Read this file — current state.
4. `git log --oneline -10` — phase boundaries with commit SHAs.
5. `cargo test` — confirm 202 tests green.
6. `cargo build --release` — confirm clean build.
7. Pick the next phase below.

### Next up

**Beyond Phase 4 — open follow-ups.** The original roadmap is shipped.
Anything below is opportunistic polish, not a roadmap commitment.

- **`/plugins reload`.** Will require the slash registry to live on
  Agent (or a rebuild path that returns to the REPL boundary).
- **Strict-mode flag for `-p`.** Switch to `AlwaysDeny` on `Ask`
  decisions for fully-automated runs that shouldn't auto-approve.
- **An alternative `Memory` strategy.** `EmbeddingRAG` is the most
  obvious next impl — `nomic-embed-text` runs on the same Ollama
  instance; would let us measure whether retrieval-mediated context
  beats linear+compact on long sessions.
- **Hook short-circuit.** Let `PreToolUse` return a synthetic result
  to skip the actual tool, mirroring Claude Code's hook semantics.
- **Diff preview via `similar` crate.** Replace the inline old/new
  rendering with a unified diff for Edit calls that span many lines.
- **Per-project `.oli/notes/`.** Today notes live globally; project-
  scoped notes would let a repo carry its own knowledge alongside
  `.oli/config.toml` and `.oli/plugins/`.

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

# Build and run the release binary
cargo run --release -- -p "your prompt here"

# Quick API surface check
./target/release/oli --help

# Format and lint
cargo fmt --all && cargo build --release
```
