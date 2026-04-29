# Agent Harness — Spec

A minimal, hackable, single-binary terminal coding agent in Rust. Designed to
replace daily Claude Code use, run primarily against local models via Ollama,
and stay small enough to read in one sitting.

This document is the high-level spec. Per-phase or per-feature design notes
will land alongside it as `specs/<topic>.md`.

## Mission

Build a coding agent that:

- Runs locally against Ollama by default, but can flip to any OpenAI-compatible
  endpoint (OpenRouter, OpenAI, LM Studio, vLLM, llama.cpp server) with a
  config change.
- Exposes a small, well-shaped extension surface so adding tools, providers,
  policies, slash commands, and hooks is mechanical.
- Stays close to the metal — a reader can hold the whole agent loop in their
  head.

## Principles

1. **Readable in one sitting.** Cap the core at ~2000 LOC.
2. **One trait per extension axis.** `Tool`, `Provider`, `Policy`,
   `SlashCommand`, `Hook`. Nothing more.
3. **No abstraction before the third concrete user.** First two
   implementations live as concrete types; the trait shows up when the third
   appears.
4. **Config over code.** New tool, new model, new external integration → edit
   TOML, don't recompile.
5. **Safe by default, escapable on demand.** Every shell/edit goes through a
   policy; defaults are conservative.
6. **Local-first.** Every decision considers a 7B-parameter model on consumer
   hardware: small context, flaky tool-call format, slow first token.

## In scope

- Interactive REPL with streaming.
- Multi-provider via OpenAI-compat (Ollama, OpenRouter, OpenAI, LM Studio).
- Native Anthropic provider — only when prompt caching is the goal.
- Tool surface: `Read`, `Write`, `Edit`, `Bash`, `Grep`, `Glob`, `Task`
  (subagent).
- Permission/policy system, config-driven.
- Slash commands.
- Two-tier extensibility:
  - **Subprocess tools (MCP-lite)** — language-agnostic external binaries.
  - **Scripted plugins (Lua)** — in-process, single-file plugins that register
    tools, slash commands, and hooks, and can call back into the harness to
    run prompts.
- Token-aware context management with auto-compact.
- Session persistence (resume / continue).
- Tool-call fallback parser for weaker local models.

## Out of scope (for now)

- TUI / `ratatui` rendering.
- Real MCP protocol — subprocess-lite covers the same need with less surface.
- IDE integration.
- Multi-user or hosted modes.
- Custom prompt caching beyond what Anthropic's API offers.

## Architecture sketch

```
src/
├── main.rs              # CLI entry
├── agent/
│   ├── mod.rs           # Agent loop
│   ├── context.rs       # System prompt, env discovery, CLAUDE.md ingestion
│   └── compact.rs       # Token tracking + summarization
├── providers/
│   ├── mod.rs           # Provider trait
│   ├── openai_compat.rs # Ollama, OpenRouter, OpenAI, LM Studio, ...
│   └── anthropic.rs     # Phase 4: native, for prompt caching
├── tools/
│   ├── mod.rs           # Tool trait + registry
│   ├── read.rs
│   ├── edit.rs
│   ├── bash.rs
│   ├── grep.rs
│   ├── glob.rs
│   ├── task.rs          # Phase 3: subagent
│   └── subprocess.rs    # Phase 2: external tools
├── policy/
│   ├── mod.rs           # Policy trait + default
│   └── config.rs
├── plugins/
│   ├── mod.rs           # Plugin loader, lifecycle, registration
│   ├── host.rs          # Host API exposed to plugin scripts
│   └── lua.rs           # Lua runtime (via `mlua`)
├── repl/
│   ├── mod.rs
│   ├── slash.rs         # Slash command registry
│   └── stream.rs        # Streaming output rendering
├── config.rs            # TOML config loading
├── error.rs
└── memory.rs            # Phase 4: optional notes directory
```

## Configuration

`~/.config/oli/config.toml`:

```toml
default_provider = "ollama"
default_model    = "qwen2.5-coder:7b"

[providers.ollama]
kind     = "openai-compat"
base_url = "http://localhost:11434/v1"
api_key  = "ollama"   # required by spec, ignored by server

[providers.openrouter]
kind          = "openai-compat"
base_url      = "https://openrouter.ai/api/v1"
api_key_env   = "OPENROUTER_API_KEY"
default_model = "anthropic/claude-haiku-4.5"

[policy]
auto_allow      = ["Read", "Glob", "Grep"]
ask             = ["Write", "Edit"]
bash_allowlist  = ["git status", "git diff", "cargo *", "ls *"]

[[tools.subprocess]]
name        = "MyCustomTool"
command     = "/path/to/binary"
description = "..."

[plugins]
# Lua plugins are auto-discovered from:
#   ~/.config/oli/plugins/*.lua
#   <project>/.oli/plugins/*.lua
disabled = []   # opt-out by plugin name
```

CLI flags `--provider <name>` and `--model <id>` override config for a single
run. `/provider` and `/model` slash commands switch mid-session.

## Plugin system

A **plugin** is a unit of user-authored extension code that can register
tools, slash commands, and hooks, and that can call back into the harness —
including running prompts against the configured model. This is the
OpenCode-shaped "write your own code to extend the agent" surface.

Distinct from subprocess tools: subprocess tools are language-agnostic
external binaries with simple I/O; plugins run in-process with full host API
access. Both coexist; plugins are for authors who want to compose harness
capabilities, subprocess tools are for wrapping existing binaries.

### Goals

- **Trivial to author.** One file, no build step, no package manager, no
  manifest beyond what the file itself declares.
- **Compose harness capabilities.** Plugins can run prompts, call other
  tools, ask the user — without re-implementing agent plumbing.
- **Sandboxed enough.** A misbehaving plugin can't trash the user's machine.
  Shell, file, and tool access flow through the same policy engine as
  built-in tools.
- **Distributable as a single file.** No `node_modules`-style trees. Copy a
  `.lua` file into the plugins dir and it works.

### Scripting language: Lua (via `mlua`)

| Option       | Footprint    | Familiarity                 | Async    | Verdict                                |
| ------------ | ------------ | --------------------------- | -------- | -------------------------------------- |
| Lua (`mlua`) | ~200 KB      | Neovim, Wezterm, Redis, NGINX | Native | **Chosen**                             |
| Rhai         | ~600 KB      | Rust-shaped                 | Awkward  | Backup if Lua proves wrong             |
| Boa (JS)     | ~3 MB        | JS                          | Limited  | Immature spec coverage                 |
| Deno (V8)    | +30 MB binary | TS/JS                       | Native   | Too heavy for a minimal harness        |

Lua wins on size and async ergonomics with `mlua`'s tokio integration. The
plugin contract is host-language-agnostic: if TypeScript becomes a hard
requirement later, a Deno or WASM runtime can be added as a second backend
without breaking existing Lua plugins.

### Plugin contract (sketch)

```lua
-- ~/.config/oli/plugins/repo-summarize.lua
local plugin = {
  name    = "repo-summarize",
  version = "0.1.0",
}

plugin.tools = {
  {
    name        = "SummarizeRepo",
    description = "Summarize the current repo using the agent.",
    parameters  = { type = "object", properties = {}, required = {} },
    execute     = function(args, ctx)
      local files   = ctx:tool("Glob",  { pattern = "**/*.rs" })
      local summary = ctx:prompt("Summarize this Rust repo. Files:\n" .. files)
      return summary
    end,
  },
}

plugin.slash_commands = {
  {
    name        = "/summarize",
    description = "Summarize the current repo",
    execute     = function(args, ctx)
      return ctx:run_tool("SummarizeRepo", {})
    end,
  },
}

plugin.hooks = {
  pre_tool_use = function(event, ctx)
    ctx:log("debug", "running tool: " .. event.tool_name)
  end,
}

return plugin
```

### Host API (`ctx`)

Exposed to every plugin entry point (tool `execute`, slash command `execute`,
hook handler):

```
ctx:prompt(text)              -> string    -- one-shot LLM call, current model
ctx:prompt_with(opts)         -> string    -- prompt + tools + max_turns + system
ctx:tool(name, args)          -> value     -- invoke a registered tool
ctx:run_tool(name, args)      -> value     -- alias for tool(); subject to policy
ctx:shell(cmd)                -> string    -- runs through Bash + policy
ctx:read_file(path)           -> string
ctx:write_file(path, content)
ctx:log(level, msg)
ctx:ask_user(question)        -> string    -- terminal prompt, blocks
ctx:get_state(key)            -> value     -- per-session, per-plugin
ctx:set_state(key, value)
```

`ctx:prompt(...)` is the lever. It runs a fresh agent loop with the same
provider/model/policy the user is on, returns the assistant's final message,
and lets plugins compose prompts with code without re-doing the agent
plumbing. Effectively the same machinery as the `Task` subagent tool, exposed
as an API.

### Lifecycle events

Hook names mirror Claude Code's event vocabulary so the mental model
transfers:

- `session_start`, `session_end`
- `user_prompt_submit`
- `assistant_message`
- `pre_tool_use`, `post_tool_use`
- `pre_compact`, `post_compact`

Hooks run in plugin registration order. A plugin throwing an error is logged
and isolated — it does not crash the session.

### Discovery & lifecycle

- Auto-discovered on session start from
  `~/.config/oli/plugins/*.lua` (global) and
  `<project>/.oli/plugins/*.lua` (per-project).
- Project plugins shadow global plugins of the same name.
- Plugins can be disabled by name in config.
- `/plugins` slash command lists loaded plugins and their registered
  components; `/plugins reload` re-evaluates the directory without restart.

### Sandboxing

- Plugin Lua sandbox removes `os.execute`, `os.exit`, `io.popen`, raw `io`
  filesystem access, and `package.loadlib`. Equivalents are exposed via
  `ctx:shell` / `ctx:read_file` / `ctx:write_file`, which all flow through
  the harness policy engine.
- Plugin shell calls go through the same `Policy::check` as built-in tool
  calls. There is no privileged plugin path.
- Plugins do not bypass user-facing approval prompts; a plugin that wants to
  run an unfamiliar shell command will be asked the same way a model would.

### Design constraints flowing earlier

These shape Phase 0–2 work even though the runtime lands in Phase 3:

- `Tool`, `SlashCommand`, and `Hook` traits must be cheaply constructible
  from Lua tables (parameters via JSON-Schema-shaped Lua tables, executors as
  Lua functions). Avoid generics on these traits.
- The agent loop must be re-entrant: a tool executor needs to be able to ask
  for a fresh agent loop run via the same machinery, with its own message
  list and tool budget.
- The hook dispatcher is shared between built-in hooks (Phase 3) and
  plugin-registered hooks. One mechanism, two registration sources.

## Local-model survival kit

The features that wouldn't matter against frontier models but are
non-negotiable on a 7B local model:

- **Token-aware context with auto-compact.** Track per-turn usage; when
  approaching the model's context window, summarize older turns into a single
  message. Without this, daily use is impossible on small windows. Lands
  behind the `Memory` trait (default impl: `LinearWithCompact`); see
  `specs/memory.md` for the pluggable strategy surface.
- **Tool-call fallback parser.** Many local models emit tool calls as plain
  text (`<tool_call>{...}</tool_call>`, fenced JSON, etc.) rather than the
  structured `tool_calls` field. If the structured field is empty but the
  content looks tool-call-shaped, parse and dispatch.
- **Model-capability registry.** Hardcoded (and config-overridable) map of
  model-name-prefix → `{ context_window, supports_native_tool_calls,
  supports_streaming_tool_deltas }`. Drives whether the fallback parser kicks
  in and how the system prompt is shaped.
- **Trim for small contexts.** Shorter tool descriptions, lower default `Read`
  line cap, fewer tools registered when a small-window model is selected.

## Roadmap

Each phase ends in something usable; you can stop at any phase boundary.

### Phase 0 — Foundations (~1d)
- Module split per architecture sketch above.
- `Tool` trait + registry.
- `Provider` trait + `OpenAICompatProvider`.
- TOML config loader.
- `thiserror`-based error types.
- Test scaffold: fake provider for tool-loop unit tests.

**Done when:** behavior is byte-identical to today; adding a tool is one new
file plus one register call.

### Phase 1 — Daily driver (~2–3d)
- Interactive REPL (`rustyline`) with streaming.
- System prompt: cwd, `git status` summary, OS, date, parent listing.
- `CLAUDE.md` ingestion (project walk-up + `~/.claude/CLAUDE.md`).
- New tools: `Edit` (exact-string replace, read-first invariant), `Grep` (`rg`
  shell-out), `Glob`, `Read` with `offset`/`limit`.
- Bounded tool output (truncation marker).
- Token-aware context + auto-compact.
- Tool-call fallback parser + model-capability registry.

**Done when:** can hold a multi-turn session against `qwen2.5-coder:7b` on
Ollama, navigate this repo, and make a non-trivial code change without
babysitting.

### Phase 2 — Flexibility surface (~2d)
- Policy engine: `Policy::check(tool, args, cwd) -> Allow | Ask | Deny`. Default
  policy reads from config.
- Slash commands: `/clear`, `/help`, `/model`, `/provider`, `/cost`, `/compact`,
  `/system`, `/tools`. `/model` lists Ollama tags via `/api/tags`.
- Subprocess tool registration (MCP-lite). External binary speaks JSON over
  stdio; registered via config; appears as a normal `Tool`.

**Done when:** a new external tool requires zero code changes — drop a binary,
add three config lines.

### Phase 3 — Power features (open scope)
- Session persistence: JSONL transcript per session, `--resume <id>`,
  `--continue` (latest).
- Subagent (`Task` tool): spawn a child loop with isolated context, return
  only the summary. Same machinery powers plugin `ctx:prompt(...)`.
- Hooks: `PreToolUse`, `PostToolUse`, `Stop`. Shared dispatcher with plugin
  hooks (one mechanism, two registration sources).
- **Plugin runtime: Lua via `mlua`.** Auto-discover plugins from
  `~/.config/oli/plugins/` and `<project>/.oli/plugins/`, expose host
  API (`ctx:prompt`, `ctx:tool`, `ctx:shell`, ...), wire plugin-registered
  tools / slash commands / hooks into the corresponding registries. Sandbox
  removes raw `os`/`io` access; everything flows through the policy engine.
- `/plugins` and `/plugins reload` slash commands.
- Native Ollama provider — only if grammar-constrained / `format: "json"`
  output is needed to fix tool-call reliability on stubborn models.

### Phase 4 — Native Anthropic + polish
- `AnthropicNativeProvider` with prompt caching (the only feature
  OpenAI-compat genuinely can't deliver).
- Diff preview before `Edit`/`Write`.
- Per-project `.oli/config.toml` overrides.
- `NotesStore` trait + filesystem default — cross-session "long-term"
  memory exposed to the model as `WriteNote` / `SearchNotes` /
  `ListNotes` tools. Distinct from active-context `Memory`; see
  `specs/memory.md`.

## Success criteria

- [ ] Can complete a non-trivial code change end-to-end against
      `qwen2.5-coder:7b` on Ollama with no babysitting.
- [ ] Same binary works against Claude via OpenRouter (or native Anthropic in
      Phase 4) with a single config flip.
- [ ] Adding a new tool: one file under `tools/`, one register call.
- [ ] Adding a new external tool: zero code changes, three config lines.
- [ ] A user can write a single-file Lua plugin that registers a tool, calls
      `ctx:prompt(...)` to delegate to the LLM, and have it usable in a
      session after `/plugins reload` — no rebuild.
- [ ] Core (excluding tests, excluding embedded Lua runtime) stays under
      2500 LOC through Phase 3.
- [ ] REPL feels responsive — first token visible within streaming latency,
      no UI freezes.

## Open questions

- **OpenRouter as default fallback.** Kept as a configured non-default
  provider so Claude is one flag away.
- **First-target local model.** `qwen2.5-coder:7b` for Phase 1 iteration.
- **TUI.** Deferred indefinitely; revisit only if streaming + line-based
  rendering proves insufficient.
- **Plugin scripting language.** Lua chosen as default. Open: do we want a
  TypeScript backend (Deno) as a second runtime later, or stay Lua-only and
  let WASM cover polyglot needs?
- **Plugin async model.** `mlua` async or sync-only with the host driving
  reentrant calls? Affects how plugin authors write `ctx:prompt(...)`.

## Non-goals (explicit)

- Replicating every Claude Code feature. Parity is the ceiling, not the goal.
- Hosting, sharing, or syncing sessions across machines.
- Any form of telemetry.
