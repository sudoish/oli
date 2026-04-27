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
- Subprocess-based external tools (MCP-lite).
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
├── repl/
│   ├── mod.rs
│   ├── slash.rs         # Slash command registry
│   └── stream.rs        # Streaming output rendering
├── config.rs            # TOML config loading
├── error.rs
└── memory.rs            # Phase 4: optional notes directory
```

## Configuration

`~/.config/agent/config.toml`:

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
```

CLI flags `--provider <name>` and `--model <id>` override config for a single
run. `/provider` and `/model` slash commands switch mid-session.

## Local-model survival kit

The features that wouldn't matter against frontier models but are
non-negotiable on a 7B local model:

- **Token-aware context with auto-compact.** Track per-turn usage; when
  approaching the model's context window, summarize older turns into a single
  message. Without this, daily use is impossible on small windows.
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
  only the summary.
- Hooks: `PreToolUse`, `PostToolUse`, `Stop`. Spawn user-configured commands
  with event JSON on stdin.
- Native Ollama provider — only if grammar-constrained / `format: "json"`
  output is needed to fix tool-call reliability on stubborn models.

### Phase 4 — Native Anthropic + polish
- `AnthropicNativeProvider` with prompt caching (the only feature
  OpenAI-compat genuinely can't deliver).
- Diff preview before `Edit`/`Write`.
- Per-project `.agent/config.toml` overrides.
- Optional memory directory mirroring `~/.claude/memory`.

## Success criteria

- [ ] Can complete a non-trivial code change end-to-end against
      `qwen2.5-coder:7b` on Ollama with no babysitting.
- [ ] Same binary works against Claude via OpenRouter (or native Anthropic in
      Phase 4) with a single config flip.
- [ ] Adding a new tool: one file under `tools/`, one register call.
- [ ] Adding a new external tool: zero code changes, three config lines.
- [ ] Core (excluding tests) stays under 2500 LOC through Phase 3.
- [ ] REPL feels responsive — first token visible within streaming latency,
      no UI freezes.

## Open questions

- **Repo strategy.** Continue on this repo (current default) vs. fork to
  decouple from CodeCrafters stage submissions.
- **OpenRouter as default fallback.** Kept as a configured non-default
  provider so Claude is one flag away.
- **First-target local model.** `qwen2.5-coder:7b` for Phase 1 iteration.
- **TUI.** Deferred indefinitely; revisit only if streaming + line-based
  rendering proves insufficient.

## Non-goals (explicit)

- Replicating every Claude Code feature. Parity is the ceiling, not the goal.
- Hosting, sharing, or syncing sessions across machines.
- Any form of telemetry.
