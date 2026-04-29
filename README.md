# oli

A minimal, hackable, single-binary terminal coding agent.

Runs locally against Ollama by default; a config flip points it at any
OpenAI-compatible endpoint (OpenRouter, OpenAI, LM Studio, vLLM,
llama.cpp's server) or at the native Anthropic Messages API for prompt
caching.

## Build & run

```sh
cargo build --release
./target/release/oli                    # interactive REPL
./target/release/oli -p "say hi"        # single-shot
./target/release/oli --resume <id>      # resume a saved session
./target/release/oli --continue         # resume the most recent
```

## Config

`~/.config/oli/config.toml` (global) and `<project>/.oli/config.toml`
(per-project, walks up from cwd; merged over global) drive provider,
model, policy, plugins, and capability overrides. See
`specs/README.md` for the full schema.

A minimal local-first config:

```toml
default_provider = "ollama"

[providers.ollama]
kind          = "openai-compat"
base_url      = "http://localhost:11434/v1"
api_key       = "ollama"
default_model = "qwen3-coder:30b"

[[caps]]
prefix                          = "qwen3-coder"
ctx_window                      = 256_000
supports_native_tool_calls      = true
supports_streaming_tool_deltas  = true
```

## What ships

- Streaming REPL with rustyline, multi-turn history, slash commands
  (`/clear`, `/help`, `/cost`, `/tools`, `/memory`, `/compact`,
  `/provider`, `/model`, `/sessions`, `/plugins`, `/exit`).
- Tools: `Read`, `Write`, `Edit`, `Bash`, `Grep`, `Glob`, `Task`
  (subagent), plus `WriteNote` / `SearchNotes` / `ListNotes` for
  cross-session memory and any `[[tools.subprocess]]` external binaries
  registered via config.
- Policy engine with `auto_allow` / `ask` / `bash_allowlist`. REPL
  prompts for any `Ask` decision; `-p` mode auto-approves.
- Pluggable `Memory` trait — default `LinearWithCompact` summarizes
  older turns when nearing the context window; alternative strategies
  (RAG, graph) drop in.
- Session persistence — every REPL session is a JSONL transcript at
  `~/.config/oli/sessions/<id>.jsonl`. `--resume` / `--continue` /
  `/sessions` read them back.
- Lua plugin runtime (`mlua`). Auto-discovers `.lua` files under
  `~/.config/oli/plugins/` and `<project>/.oli/plugins/`. Plugins can
  register tools, slash commands, and hooks; sandbox strips `os` /
  `io` / `package.loadlib` and routes file/shell access through the
  policy gate.
- Native Anthropic provider with prompt caching on system + tools.
- Hook dispatcher for `PreToolUse` / `PostToolUse` / `Stop`.

## Architecture

`specs/README.md` is the high-level spec; `specs/memory.md` covers the
pluggable active-context memory design. `specs/progress.md` tracks
phase-by-phase status with commit SHAs.
