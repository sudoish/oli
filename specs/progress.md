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
| e83c9fb | 5b    | `HttpTransport` for streamable-http (POST + JSON or SSE response, `Mcp-Session-Id` capture/echo, env-expanded user headers), `/mcp restart <server>` in place, `auto_allow_pure_reads` policy heuristic (default on; only fires on `<server>__<verb>`-shaped names) |
| 8ef275f | A     | Daily-driver safety: Bash timeout + per-call cwd, `--strict` flag, persisted-reads round-trip, REPL `→ Tool(...)` progress hook |
| 18f27f7 | B     | Hook short-circuit (`PreToolUse` can return synthetic result), result-mutation hook outcome |
| e1af9ce | C     | `/plugins reload` (registry rebuild via shared `Arc<Mutex<Registry>>`), `session-cost` in `/cost`, subagent result cap |
| 2fbecfb | D     | Caching/parity polish: OpenRouter cache control, Anthropic model list, unified-diff preview via `similar`, stale-`Edit` detection (mtime check) |
| 0193495 | E1    | Plugin instruction-count budget via mlua thread hooks (`max_instructions` config; deterministic timeout for runaway loops) |
| 19b32f8 | E2    | `EmbeddingRagMemory` (retrieval-mediated snapshots; `Embedder` trait + `OllamaEmbedder` default; configurable via `[memory] kind = "rag"`) |
| 110d8ee | E3    | MCP `tools/list_changed` live refresh (per-turn diff against per-server `Arc<AtomicBool>`; agent loop swaps registry entries atomically) |
| c23da45 | F     | TUI skeleton + alt-screen lifecycle + `--plain` fallback                                              |
| a5db293 | G     | TUI agent integration: streaming, mode indicator, slash dispatch, Ctrl-C cancel, rebuild on swap        |
| 5d0ca6e | H     | TUI tool-call cards: live spinner, timing, result summaries                                           |
| f9fb277 | I     | TUI approval modal: single-key dispatch, scrollable diff preview, session-scoped allow/deny           |
| 1515a42 | J     | TUI markdown rendering + syntect-highlighted code fences                                              |
| d0f4d4e | K     | TUI input ergonomics: multi-line, completion popups, persistent history                               |
| 020da39 | K4    | TUI Ctrl-R history search overlay                                                                     |
| 895fdf8 | L     | TUI scrollable transcript + stick-to-bottom + `/copy N` via OSC52                                     |
| d21567e | M     | TUI status bar: identity strip + token gauge + live mode + width-aware collapse                       |
| 7ea160e | N     | TUI discoverability overlays: `/sessions` picker, `/help` browser, `/<cmd> ?` cards, fading hints     |
| 3e1c69b | N4    | TUI first-run wizard + Bash process-group kill (real grandchild termination via `setpgid` + `killpg`) |
| cec3113 | O     | TUI recoverability: `/undo`, `Ctrl+E` edit-and-rerun, verified Bash cancel kills the whole process group |
| 2b2e571 | docs  | `specs/review-2.md` — deep review #2 (rated 9/10)                                                     |
| 43da094 | docs  | `specs/polish.md` — 9/10 → 10/10 polish roadmap                                                       |
| c243257 | P1    | Zero build warnings: drop real dead code, annotate intentional public surface with `#[allow(dead_code)]` + comment |
| fe1fed2 | P2    | Overlay sum type: replace App's six `Option<*State>` overlay fields with one `Overlay` enum; single-match keypress router |
| 1687232 | P3    | Split `tui/app.rs` (1729 LOC) → `app/{mod,overlay,transcript,tests}.rs`; split `tui/ui.rs` (1358 LOC) → `ui/{mod,overlays,transcript}.rs` |
| 735fb45 | Q1+Q2 | Extract `src/lib.rs` with public re-exports, move binary to `src/bin/oli.rs`, factor reusable wiring into `src/bootstrap.rs` |
| 09876fd | Q3    | Module-level `//!` docs on every top-level module; `cargo doc --no-deps` produces a clean module index |
| fa718c4 | R     | `/diagnostics` ring buffer (8 KB cap, FIFO eviction) + `log_*!` shim replacing `eprintln!` in mcp/plugins/providers/repl; `RUST_LOG` threshold (info default) |
| ba630c8 | S1    | Persisted approval allow-list at `~/.config/oli/policy-allow.json`; capital `[A]` writes through, lowercase `a` stays session-only |
| 7e2e67f | S2    | Subagent inherits parent's `ToolContext`: `SubagentSpawner::spawn` takes `Option<ToolContext>`; child gets parent's read-set + sticky cwd |
| d5e5961 | S3    | `ShowFull(id, offset, limit)` tool + per-session result cache (32-entry FIFO); truncation marker embeds the cache id |
| 9ab0d70 | T1    | `oli init` headless CLI subcommand (`--provider`, `--api-key`, `--force`); `wizard_init` module shared with TUI wizard |
| 343462c | T2    | `/config reload` slash command: re-parse config.toml, swap provider/model/policy/caps; memory + transcript + system prompt survive |
| 0fe85b6 | T3    | `tui` and `syntax-highlight` Cargo features; `cargo build --no-default-features` produces an 8.5 MB line-mode-only binary (was 11 MB) |
| 2582b5f | T4+T5 | `docs/cheatsheet.md` (full keybind/slash/path/env/feature reference, linked from `oli --help`); `specs/README.md` TOC pointing at every spec doc |

Tip-of-master at last update: **Phase T (polish complete: 17/17)**.
Tests: **470 unit tests, all green** (was 244 at end of phase 5b — +226
from phases A–E, F–O, P–T, plus the deferred N4/K4/O work).
Release build: clean across all three feature configs (`default`,
`--no-default-features`, `--no-default-features --features tui`).

## What works today

**Phase T — onboarding & packaging (latest):**
- `oli init` subcommand (`src/bin/oli.rs`) — headless mirror of the
  TUI's first-run wizard. Flags `--provider {ollama,openrouter,
  anthropic}`, `--api-key <key>`, `--force`. Without `--provider`,
  prompts on stdin with a numbered menu. Refuses to clobber an
  existing config without `--force`. Output mirrors the TUI's
  confirmation card (provider label, default model, "Run `oli` to
  start").
- `wizard_init` module (`src/wizard_init.rs`) — data layer shared
  with the TUI wizard. `WizardProvider` enum (with `from_name` for
  case-insensitive CLI parsing), `render_toml(provider, api_key)`,
  `save(path, body, force)`, `config_path()`. The TUI's
  `tui::wizard::WizardState::render_toml` is now a one-line
  delegation; both surfaces produce byte-identical config files.
- `/config reload` slash command — re-parses `config.toml` (global +
  project-local overlay), rebuilds the active provider via
  `crate::providers::build`, recomputes caps from the new model id,
  swaps `[policy]`. Memory, transcript, system prompt, and session
  totals survive. Reload errors don't touch live state, so a
  fat-fingered edit is recoverable.
- Cargo feature gates. `default = ["tui", "syntax-highlight"]`;
  `tui` covers ratatui + crossterm + tui-textarea-2 + pulldown-cmark;
  `syntax-highlight` adds syntect on top. `--no-default-features`
  produces an 8.5 MB line-mode-only binary (was 11 MB), useful for
  piped CI usage. Code fences fall back to a plain cyan-gutter card
  when syntect is off; the TUI is excluded entirely without the
  `tui` feature.
- `docs/cheatsheet.md` — every keybind, slash command, file path,
  env var, and feature flag in one page. Linked from
  `oli --help` via clap's `long_about`.
- `specs/README.md` TOC — "Where to start reading" table at the top
  pointing at every spec doc plus the cheatsheet. Existing
  high-level spec content kept verbatim below.

**Phase S — persistent user state:**
- Persisted approval allow-list at
  `~/.config/oli/policy-allow.json`. Capital `[A]` on the approval
  modal writes the (tool, args-canonical-json) fingerprint through
  to disk; lowercase `[a]` stays session-only. Versioned JSON
  envelope (`{"version": 1, "fingerprints": [...]}`); malformed,
  missing, or version-mismatched files yield an empty list (a
  corrupt cache never denies tools the user already approved).
- Subagent inherits parent's `ToolContext`. `SubagentSpawner::spawn`
  takes `Option<ToolContext>`; `Task::run` passes the parent's;
  `bootstrap::DefaultAgentSpawner` snapshots the parent's read-set
  + sticky cwd into the child's context after building the agent.
  One-way clone — child reads stay local. `read_logger` is
  intentionally not propagated.
- `ShowFull(id, offset, limit)` tool + per-session result cache.
  Tool calls that hit the 30 KB byte cap stash their full body in
  the cache (32-entry FIFO ring on `ToolContext`) and embed the id
  in the truncation marker. The model can paginate deeper without
  blanket-loading every oversized result. Bash, Grep, Subprocess,
  Notes (Search/List), Task all migrated; Edit/Write/Read keep their
  own truncation strategies.

**Phase R — operational visibility:**
- `src/diagnostics.rs` — process-wide
  `Mutex<VecDeque<DiagnosticEntry>>` capped at 8 KB (FIFO
  eviction). `push()` stashes everything regardless of level;
  stderr printing gates on `RUST_LOG` (info default,
  trace/debug/info/warn/error). Whole module under 200 LOC; no
  `tracing`/`log` dep.
- `crate::log_warn!` / `log_info!` / `log_error!` / `log_debug!`
  macros replace the operational `eprintln!` sites in `mcp/`,
  `plugins/`, `providers/openai_compat.rs`, and `repl/`. The Lua
  `ctx:log` binding routes through diagnostics with level mapping.
- `/diagnostics` slash command renders the most-recent 50 entries
  as `[level] body`; `/diagnostics clear` wipes the ring. Picked up
  automatically by both the line-mode REPL and the TUI through
  `default_set_with_reloader`.

**Phase Q — library split:**
- `src/lib.rs` exposes the public API for embedders. Re-exports
  `Agent`, `Provider`, `Tool`, `Memory`, `LinearWithCompact`,
  `PersistedMemory`, `EmbeddingRagMemory`, `OllamaEmbedder`, `Hook`,
  `Policy`, `Approver`, `SlashCommand`, `Config`, `McpHandle`,
  `SubagentSpawner`, `AgentError`, `Result`. Crate-level `//!` doc
  with the trait taxonomy.
- `src/bin/oli.rs` — binary moved out of `src/main.rs`. Uses
  `oli::*` for substance instead of redeclaring modules. CLI
  parsing + orchestration only; reusable wiring (`build_default_tools`,
  `resolve_session_id`, `build_memory`, `DefaultAgentSpawner`)
  factored into `src/bootstrap.rs` so embedders can build their
  own oli-flavored agent without copying main.
- Module-level `//!` docs on every top-level module:
  `agent`, `bootstrap`, `config`, `diagnostics`, `error`, `hooks`,
  `mcp`, `notes`, `plugins`, `policy`, `providers`, `repl`, `tools`,
  `tui`, `wizard_init`. `cargo doc --no-deps --lib` renders a
  module index where every entry has a meaningful one-paragraph
  summary; zero rustdoc warnings.

**Phase P — cleanup:**
- Zero build warnings on `cargo build` and `cargo build --tests`.
  Real dead code removed (`WizardStep::Cancelled`, `MULTI_LINE`
  hint, `set_slash_names`, `has_overlay`, `default_set` for
  prod-only callers, `CompletionMenu.query`, `ApprovalState.id`,
  `ToolCard.id`, `UiEvent::ApprovalRequested.id` +
  `TuiApprover.next_id`); intentional public API kept for the
  contract annotated with `#[allow(dead_code)]` + a one-line
  comment.
- App's six `Option<*State>` overlay fields (approval,
  sessions_picker, help_browser, inline_help, history_search,
  wizard) collapsed into one `pub overlay: Option<Overlay>` enum.
  Keypress router and render dispatcher are now single-match.
  Completion stays separate (it's an in-input affordance, not modal).
- `tui/app.rs` (1729 LOC) split into `app/{mod, overlay, transcript,
  tests}.rs`; `tui/ui.rs` (1358 LOC) split into
  `ui/{mod, overlays, transcript}.rs`. Largest remaining file is
  624 LOC.

**Phase O — TUI recoverability:**
- `/undo` slash command pops the last user turn from memory and
  the transcript, returning the prompt body so the user can edit
  it. Active assistant + tool indices reset.
- `Ctrl+E` edit-and-rerun: equivalent to `/undo` but loads the
  popped body straight into the input buffer.
- Bash cancel verified to kill the entire process group, not just
  the immediate `sh` child. `setpgid(0, 0)` in `pre_exec` puts
  every grandchild in the same session; `ProcessGroupKillGuard`
  uses `libc::killpg` on drop. Tested end-to-end against
  `sh -c 'sleep 60 & wait'`.

**Phase N — TUI discoverability:**
- First-run setup wizard. Welcome → PickProvider → (EnterApiKey if
  applicable) → Confirm → Saved. Esc skips at any point. Triggered
  when `~/.config/oli/config.toml` doesn't exist at TUI startup.
- `/sessions` interactive picker overlay with arrow-key navigation;
  Enter copies a `oli --resume <id>` command to the clipboard via
  OSC52.
- `/help` interactive command browser (two-pane: list + full
  description). Arrow keys cycle, Esc / Enter closes.
- `/<cmd> ?` one-shot help cards for any registered slash command.
- Fading onboarding hints persisted in
  `~/.config/oli/tui-hints.json` so tips fade once the user has
  used the feature.

**Phase M — TUI status bar:**
- Identity strip on the left: model | session | branch | ctx
  window. Width-aware collapse drops fields right-to-left when the
  terminal narrows.
- Token gauge: green under 60%, amber 60–85%, red above 85%. Reads
  the live `last_usage` from the agent.
- Mode indicator on the right: idle / thinking (with elapsed
  spinner) / streaming / awaiting-approval (yellow override when
  the modal is up).

**Phase L — TUI scroll + clipboard:**
- Scrollable transcript with stick-to-bottom default.
  PgUp/PgDn/Ctrl+Home/Ctrl+End + mouse wheel detach into a manual
  offset; reattach when reaching bottom; `↓ N new` badge surfaces
  unread lines while detached.
- `/copy N` slash command copies the Nth-most-recent assistant
  message to the OS clipboard via OSC52 (works through SSH).

**Phase K — TUI input ergonomics:**
- Multi-line input via `tui-textarea-2`. Shift+Enter / Alt+Enter
  inserts a newline; Enter submits. Up/Down walks the persistent
  history (single-line buffers only); Ctrl+R opens a substring
  history-search overlay (newest-first, arrow-key navigate, Enter
  loads the picked entry).
- Slash and `@path` completion popups. Tab accepts; Shift+Tab
  cycles backwards. Path completion fires at any word boundary
  starting with `@`.
- Persistent history at `~/.config/oli/tui-history.jsonl`,
  rotated when it grows past 1000 entries.

**Phase J — TUI markdown rendering:**
- pulldown-cmark renders headings, bold/italic/strikethrough,
  inline code, code fences, lists, links, paragraphs into ratatui
  `Vec<Line>`. Streaming-safe — re-parsed each frame; mid-stream
  un-closed tokens render as literal text.
- Code fences route through syntect for syntax highlighting (when
  the `syntax-highlight` feature is on; default). Lazy-loaded
  `SyntaxSet` + `ThemeSet`. Plain dim-text fallback when the
  feature is off.

**Phase F–I — TUI foundations:**
- ratatui-driven alt-screen TUI with `--plain` line-mode REPL
  fallback (auto-engaged on non-TTY stdin/stdout for piped usage).
- Streaming agent integration; mode indicator switches between
  idle/thinking/streaming; Ctrl+C cancels the current turn.
- Per-tool-call cards in the transcript with a live spinner,
  elapsed time, and result summary on completion.
- Approval modal with scrollable diff preview, single-key dispatch
  (`y`/`n`/`a`/`A`/`d`/`Esc`), session-scoped allow/deny.

**Phase E — late-Phase-5 follow-ups:**
- Plugin instruction-count budget via mlua thread hooks. Per-call
  `max_instructions` (default 10M) prevents a runaway Lua loop
  from blocking the agent indefinitely.
- `EmbeddingRagMemory` (`src/agent/memory/rag.rs`). Retrieval-
  mediated snapshots: embed every recorded message, keep recent
  K turns verbatim, retrieve top-N similar older turns by cosine
  similarity. `Embedder` trait + `OllamaEmbedder` default; opt-in
  via `[memory] kind = "rag"`.
- MCP `tools/list_changed` live refresh. Each turn drains a per-
  server `Arc<AtomicBool>`; servers that pushed the notification
  have their tool list refetched and the agent's registry swapped
  in place. New tools become callable on the next model turn.

**Phase D — caching, parity, polish:**
- OpenRouter cache control on the system prompt + last tool
  definition (mirrors the native Anthropic provider's
  `cache_control: ephemeral`).
- Anthropic provider's `list_models` enumerates real model ids via
  the `/v1/models` endpoint.
- Unified-diff preview via the `similar` crate for `Edit` calls
  (replaces the JSON old/new rendering).
- Stale-`Edit` detection: `ToolContext` captures mtime at
  `mark_read` time; `Edit` refuses if the on-disk mtime has
  advanced since the last `Read`.

**Phase C — polish:**
- `/plugins reload` re-scans `~/.config/oli/plugins/` and
  `<project>/.oli/plugins/` without restarting. Registry is rebuilt
  via a shared `Arc<Mutex<Registry>>` between the agent and the
  reloader.
- `/cost` now reports session totals in addition to last-call.
- Subagent (`Task`) result is capped at 8 KB by default
  (configurable via `max_result_bytes`) so a chatty child can't
  blow up the parent's context.

**Phase B — hooks:**
- `PreToolUse` hooks can short-circuit by returning a synthetic
  `HookOutcome::Replace(result)`, skipping the actual tool call.
- `PostToolUse` hooks can mutate the tool's result before it
  reaches the model (`HookOutcome::ReplaceResult`).

**Phase A — daily-driver safety:**
- Bash timeout (default 120s, max 600s) + per-call `cwd` argument
  with sticky behavior across calls (the last explicit cwd carries
  forward).
- `--strict` flag for `-p` runs flips every `Ask` policy decision
  to `Deny` (suitable for unattended scripted runs that must not
  rubber-stamp Edit/Write/unknown-Bash).
- Persisted-reads round-trip. `PersistedMemory` writes `read` ops
  to the JSONL transcript; `--resume` replays them back into the
  `ToolContext` so `Edit`'s read-first invariant survives across
  sessions.
- REPL `→ Tool(...)` progress hook surfaces tool calls inline as
  they fire (interactive only; scripted `-p` stays quiet).

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
├── lib.rs                          # Public API surface (Agent, Tool, Provider, ...)
├── bin/oli.rs                      # CLI entry: clap + dispatch to TUI / REPL / init
├── bootstrap.rs                    # Reusable wiring (build_default_tools, ...)
├── config.rs                       # Layered TOML config (global + project overlay)
├── diagnostics.rs                  # Ring buffer + log_*! shim
├── error.rs                        # AgentError / ToolError / Result alias
├── wizard_init.rs                  # Headless config bootstrap (shared with TUI wizard)
├── agent/
│   ├── mod.rs                      # Agent loop + builder
│   ├── caps.rs                     # Model-capability registry
│   ├── context.rs                  # SystemPromptBuilder
│   ├── tool_parse.rs               # Tool-call fallback parser
│   └── memory/
│       ├── mod.rs                  # Memory trait + LinearWithCompact default
│       ├── linear.rs               # LinearWithCompact impl
│       ├── persisted.rs            # JSONL session persistence
│       └── rag.rs                  # EmbeddingRagMemory + Embedder + OllamaEmbedder
├── hooks/mod.rs                    # Hook trait + HookRegistry
├── mcp/                            # MCP client (stdio + http transports)
│   ├── mod.rs / config.rs / server.rs / stdio.rs / http.rs / transport.rs / tool.rs
├── notes/mod.rs                    # NotesStore trait + filesystem default
├── plugins/mod.rs                  # Lua plugin runtime via mlua
├── policy/
│   ├── mod.rs                      # Policy / Approver traits + ConfigPolicy
│   └── persisted_allow.rs          # ~/.config/oli/policy-allow.json
├── providers/
│   ├── mod.rs                      # Provider trait + build factory
│   ├── anthropic.rs                # Native Anthropic + prompt caching
│   ├── openai_compat.rs            # OpenAI / OpenRouter / Ollama / vLLM / ...
│   └── fake.rs                     # FakeProvider (cfg(test))
├── repl/
│   ├── mod.rs                      # Line-mode REPL (rustyline + tokio::select)
│   └── slash.rs                    # SlashCommand trait + 15 built-ins
├── tools/
│   ├── mod.rs                      # Tool trait + Registry
│   ├── context.rs                  # ToolContext (read-set, cwd, result cache)
│   ├── util.rs                     # truncate / truncate_with_cache
│   ├── show_full.rs                # ShowFull tool (paginate truncated results)
│   ├── task.rs                     # Task subagent + SubagentSpawner trait
│   └── bash / edit / glob / grep / notes / read / subprocess / write
└── tui/                            # ratatui front-end (gated behind `tui` feature)
    ├── mod.rs / driver.rs / event.rs / hook.rs / approver.rs / completion.rs
    ├── history.rs / hints.rs / markdown.rs / terminal.rs / wizard.rs
    ├── app/{mod,overlay,transcript,tests}.rs
    └── ui/{mod,overlays,transcript}.rs
```

12 traits anchor the extension surface: `Tool`, `Provider`, `Memory`,
`Hook`, `Policy`, `Approver`, `SlashCommand`, `NotesStore`,
`McpTransport`, `SubagentSpawner`, `ReadLogger`, `Embedder`. Each has
at least one bundled impl plus public visibility for embedders to
plug in their own.

## What's NOT done

The 17-item polish plan (`specs/polish.md`) closed every gap from the
post-TUI deep review. Remaining open items are forward-looking, not
regressions:

- **`tracing`-style structured logging.** The diagnostics ring is a
  flat string store. Embedders who want per-event spans / kv pairs
  would need a `tracing-subscriber` adapter. Not blocking — the
  ring buffer covers the "where did that error come from?" case.
- **GraphBacked / HierarchicalSummary memory strategies.** Sketched
  in `specs/memory.md`; only `LinearWithCompact` and
  `EmbeddingRagMemory` are implemented. Adding a third is a clean
  drop-in via the `Memory` trait.
- **Hosted/multi-user mode.** Out of scope per the spec; flagged
  here so a future contributor sees it's an explicit non-goal.
- **`oli init --reset`.** Today `--force` overwrites; a `--reset`
  flag that deletes + recreates is on the polish plan's open
  decisions list. Not landed.

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

1. Read `specs/README.md` — vision + roadmap + the new TOC at the
   top pointing at every other spec doc.
2. Read `docs/cheatsheet.md` — every keybind / slash / file path /
   env var / feature flag in one page.
3. Read this file — current state, phase ledger with SHAs.
4. `git log --oneline -20` — phase boundaries.
5. `cargo test` — confirm 470 tests green.
6. `cargo build --release` and
   `cargo build --release --no-default-features` — confirm both
   feature configs build clean.
7. `cargo doc --no-deps --lib --open` — render the public API
   index (every top-level module has a `//!` overview).

### Next up

The original roadmap (Phases 0–5) and the post-MCP roadmap (A–E,
F–O, P–T) are all shipped. Anything below is opportunistic polish
or research, not a roadmap commitment.

- **`tracing` adapter.** Optional feature that bridges
  `crate::log_*!` macros into `tracing` events for embedders who
  want structured logging.
- **`oli init --reset`.** `--force` overwrites; `--reset` would
  delete + recreate. Flagged in `specs/polish.md` open decisions.
- **GraphBacked or HierarchicalSummary memory.** Third drop-in for
  the `Memory` trait once a real workload makes the choice obvious.
- **Per-project `.oli/notes/`.** Notes are globally-scoped today;
  project-scoped would let a repo carry its own knowledge.
- **Provider-side Anthropic prompt caching coverage.** The native
  provider sets cache_control on system + last tool; OpenRouter
  cache hooks on the same surfaces (Phase D). A real measurement
  pass on long sessions would tell us whether the cache cutoffs
  are placed where they actually pay off.

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
# Run the full test suite (470 tests)
cargo test

# Run lib-only tests for each feature config
cargo test --lib                                          # default (470)
cargo test --lib --no-default-features --features tui     # tui without syntect (470)
cargo test --lib --no-default-features                    # plain only (353; TUI gated out)

# Build the binary
cargo build --release                                     # 11 MB, full TUI
cargo build --release --no-default-features               # 8.5 MB, line-mode only

# Run against a real model
cargo run --release -- -p "your prompt here"

# Headless first-run config bootstrap
./target/release/oli init --provider ollama

# Quick API surface check
./target/release/oli --help

# Render the public-API docs
cargo doc --no-deps --lib --open
```
