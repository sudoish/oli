<div align="center">

# oli

⚠️ **Experimental Project** ⚠️

*This is a purely experimental project aimed at learning the components of a coding agent. Not intended for production use.*

---

</div>

A minimal, hackable, scriptable coding-agent runtime.

`oli` drives an LLM through a small set of code-aware
tools (`Read`, `Write`, `Edit`, `Bash`, `Grep`, `Glob`, `Task`) with automatic
tool execution by default and an opt-in approval flow. It runs locally against [Ollama] by default,
flips to any OpenAI-compatible endpoint (OpenRouter, OpenAI, LM Studio,
vLLM, llama.cpp server) with a config change, and speaks the native
Anthropic Messages API when you want prompt caching.

The whole core is small enough to read in one sitting — the design target
is **~2500 LOC of agent loop** with extension surfaces (`Tool`, `Provider`,
`Policy`, `SlashCommand`, `Hook`) instead of a deep framework. Add a tool
in one file, a plugin in one Lua file, an external binary in three lines
of TOML.

[Ollama]: https://ollama.com

---

## Table of contents

- [Why oli](#why-oli)
- [Install](#install)
- [First run](#first-run)
- [Daily use](#daily-use)
- [Configuration](#configuration)
- [Extending oli](#extending-oli)
  - [Lua plugins](#lua-plugins)
  - [Subprocess tools (MCP-lite)](#subprocess-tools-mcp-lite)
  - [MCP servers](#mcp-servers)
  - [Hooks](#hooks)
  - [Adding to the binary](#adding-to-the-binary)
- [Architecture](#architecture)
- [Where to read next](#where-to-read-next)

---

## Why oli

- **Local-first.** Runs against a 7B model on your laptop with no API
  key. Token-aware context with auto-compact, a fallback tool-call
  parser for models that emit `<tool_call>{...}</tool_call>` as plain
  text, and a per-model capability registry that adapts the system
  prompt to small context windows.
- **Provider-agnostic.** Same binary, same prompts, same tools — point
  it at Ollama, OpenRouter, OpenAI, LM Studio, vLLM, llama.cpp's
  `server`, or Anthropic native. One config flip.
- **Safe by default.** Every shell command, file write, and edit goes
  through a policy engine. Conservative defaults; per-fingerprint
  `[A]llow always` persists across sessions.
- **Extensible without recompiling.** Drop a `.lua` file in
  `~/.config/oli/plugins/` to register tools, slash commands, and
  hooks. Drop three lines of TOML to wire up an external binary as a
  tool. Connect MCP servers over stdio or SSE.
- **Resumable and scriptable.** Every run is a JSONL transcript at
  `~/.config/oli/sessions/<id>.jsonl`. `oli run --conversation <id>` and
  `oli run --continue` replay it; `/sessions` browses the lot.
- **Small surface.** Five extension traits, no plugin framework, no
  package manager. The whole agent loop fits in your head.

---

## Install

### Prerequisites

- Rust **1.95+** (2024 edition). `rustup show` to check.
- A model backend — choose one:
  - **Ollama** (recommended for local): `brew install ollama`, then
    `ollama serve` and `ollama pull qwen3-coder:30b` (or any
    tool-capable model).
  - An OpenAI-compatible endpoint (LM Studio, vLLM, llama.cpp's
    `server`, OpenRouter, OpenAI).
  - An Anthropic API key for the native provider (gives you prompt
    caching).

### Build

```sh
git clone <this repo>
cd oli
cargo build --release
./target/release/oli --help
```

The binary is self-contained — copy `target/release/oli` to anywhere
on your `$PATH`.

Release notes are in [CHANGELOG.md](CHANGELOG.md). Maintainers cut the
private-agent baseline with [the release procedure](docs/baseline-release.md).

---

## First run

`oli init` walks you through provider/model selection and writes
`~/.config/oli/config.toml`. It probes Ollama if it sees it running
and offers to `ollama pull` a model on the spot. Non-interactive
forms are available for scripted setup:

```sh
oli init                                            # interactive wizard
oli init --provider ollama                          # headless, all defaults
oli init --provider openrouter --api-key sk-...     # full non-interactive
oli init --provider ollama --force                  # overwrite existing config
```

Once configured, use `oli run` for one-command/one-result automation or
run `oli` without a subcommand for the line-mode REPL.

---

## Daily use

### Invocations

| Command | What it does |
| --- | --- |
| `oli` | Interactive line-mode REPL. |
| `oli run -p "find callers of foo"` | Persist one run, print the final response, and exit. |
| `oli run --conversation <id> -p "continue"` | Append to a specific conversation. |
| `oli run --continue -p "continue"` | Append to the most recent conversation. |
| `oli run --output json -p "..."` | Emit one machine-readable result object. |
| `oli run --strict -p "..."` | Deny every operation requiring approval. |
| `oli run --max-turns N -p "..."` | Override the turn cap for one run. |

### Slash commands (highlights)

Type `/` at the prompt. Append `?` to see a command's description
without running it (e.g. `/cost ?`).

| Command | What it does |
| --- | --- |
| `/help` | List all commands. |
| `/tools` | List every tool registered — built-ins, plugins, MCP. |
| `/plugins` / `/plugins reload` | List loaded plugins; reload re-scans dirs without restarting. |
| `/mcp` | MCP server health, tool counts, restart failed servers. |
| `/config reload` | Re-parse global + project config and apply live. |
| `/provider` / `/model` | Show or swap the active provider / model. |
| `/sessions` | List saved conversation ids. |
| `/cost` | Last-call + session-total token usage. |
| `/memory` / `/compact` | Memory stats; force a compaction pass. |
| `/clear` | Drop conversation history (system prompt is preserved). |
| `/system` | Render or overwrite the pinned system prompt. |
| `/paths` | Resolved on-disk locations — config, plugins, sessions, notes, policy. |
| `/diagnostics` | Operational warnings (plugin load failures, MCP errors, etc.). |
| `/exit` | Leave (also `Ctrl+D`). |

The full keybinding + slash-command map lives in
[`docs/cheatsheet.md`](docs/cheatsheet.md).

### Project context

oli walks **up** from the cwd looking for `AGENTS.md` and `CLAUDE.md`
files and folds them into the system prompt. Both are also loaded
from `~/.codex/AGENTS.md` and `~/.claude/CLAUDE.md` as user-level
overlays. A repo-root `AGENTS.md` is found from any subdirectory.

### Approval flow

Tools run automatically by default. Set `[policy] mode = "ask"` to have the
line REPL prompt with the diff or command preview when a policy rule returns
`Ask`:

| Key | Effect |
| --- | --- |
| `y` / `Y` | Allow this one call. |
| `n` / `Esc` | Deny. |
| `a` | Allow the same `(tool, args)` fingerprint for the session. |
| `[A]` (capital A) | Allow always — also writes to `~/.config/oli/policy-allow.json`. |
| `d` | Deny the fingerprint for the session. |

The granular `auto_allow`, `ask`, `bash_allowlist`, and MCP read rules apply in
ask mode. Headless `oli run` never waits for input: unresolved approval requests
are denied. `--strict` forces ask mode and therefore denies every gated mutation.

---

## Configuration

oli reads two TOML files and merges them, project on top of global:

| Path | Scope |
| --- | --- |
| `~/.config/oli/config.toml` | Global; generated by `oli init`. |
| `<project>/.oli/config.toml` | Project overlay; walked up from cwd. |

A minimal local-first config:

```toml
default_provider = "ollama"

[providers.ollama]
kind          = "openai-compat"
base_url      = "http://localhost:11434/v1"
api_key       = "ollama"                  # required by spec, ignored by server
default_model = "qwen3-coder:30b"

[[caps]]
prefix                          = "qwen3-coder"
ctx_window                      = 256_000
supports_native_tool_calls      = true
supports_streaming_tool_deltas  = true
```

### Signing in with a ChatGPT subscription

Instead of an OpenAI API key, oli can authenticate against a ChatGPT
Plus/Pro subscription. API-key auth stays the default and is unaffected —
this is an extra provider kind, not a replacement.

```console
$ oli login                 # opens a browser on this machine
$ oli login --paste         # browser on another machine; paste the redirect URL back
$ oli login --device-auth   # headless: shows a code to enter elsewhere
$ oli login --check         # refresh token, discover models, send a real prompt
$ oli logout                # discards the stored credentials
```

**If oli is running on a remote host** (SSH, Tailscale, a container), use
`--paste`. The redirect goes to `http://localhost:1455/auth/callback`,
and that `localhost` is whichever machine the *browser* is on — not the
one running oli, so the plain flow waits for a callback that can never
arrive. With `--paste`, oli binds nothing, prints the sign-in URL, and
waits for you to paste the redirect URL back:

```console
$ oli login --paste
Open this URL in a browser — any machine, it does not have to be this one:

  https://auth.openai.com/oauth/authorize?...

After you sign in, the browser will try to reach
http://localhost:1455/auth/callback and show a connection error. That is
expected: nothing is listening there. Copy the full URL out of the
address bar and paste it below.

Pasted URL: http://localhost:1455/auth/callback?code=...&state=...
Signed in as you@example.com (pro plan).
```

The connection error in the browser is not a failure — the authorization
code is in the address bar regardless. `--paste` also works when ports
1455 and 1457 are both busy, since it never binds one.

Tokens land in `~/.config/oli/auth.json` (mode 0600) and are refreshed
automatically before they expire. Point a provider at them:

A successful login writes this block for you, sets `default_model` from
the model list your subscription actually serves, and points
`default_provider` at it. Other provider blocks are left untouched, so
switching back is a one-line edit. Pass `--no-config` to store
credentials and stop there.

```toml
[providers.chatgpt]
kind          = "openai-chatgpt"
base_url      = "https://chatgpt.com/backend-api/codex"   # optional; this is the default
default_model = "gpt-5.6-terra"
```

Caveats worth knowing before you rely on it:

- This endpoint speaks the **Responses API**, not Chat Completions. It is
  a separate transport from `openai-compat`, not a flag on it.
- **The model names are not the public API's.** No `gpt-4o`, and no
  `gpt-5.x-codex`. At the time of writing the subscription served
  `gpt-5.6-terra`, `gpt-5.6-luna`, `gpt-5.5` and `gpt-5.4-mini`. Since
  that changes, nothing is hardcoded — `oli login` asks the endpoint and
  `/model` lists what you can use.
- Subscription auth is not a documented OpenAI feature and there is no
  client registration for third-party CLIs, so oli presents the same
  OAuth client id and `originator` as OpenAI's own CLI. Override with
  `OLI_CHATGPT_CLIENT_ID` / `OLI_CHATGPT_ORIGINATOR` if you have your own.
- OpenAI's tolerance for this is informal and could end. If it does,
  every failure path names the API-key fallback explicitly rather than
  failing cryptically.

Before a release, follow [the subscription release gate](docs/subscription-release-gate.md).
To route a ChatGPT subscription through Tailscale's AI gateway, follow the
[Aperture setup guide](docs/aperture.md).
The private remote-workstation reference design begins with its
[topology and threat model](docs/remote-workstation-threat-model.md).
The runnable clean-host setup is under
[`examples/remote-workstation/`](examples/remote-workstation/).

A more loaded config with multiple providers and a stricter policy:

```toml
default_provider = "ollama"

[providers.ollama]
kind          = "openai-compat"
base_url      = "http://localhost:11434/v1"
api_key       = "ollama"
default_model = "qwen3-coder:30b"

[providers.openrouter]
kind          = "openai-compat"
base_url      = "https://openrouter.ai/api/v1"
api_key_env   = "OPENROUTER_API_KEY"
default_model = "anthropic/claude-sonnet-4.6"

[providers.anthropic]
kind          = "anthropic"               # native Messages API (prompt caching)
api_key_env   = "ANTHROPIC_API_KEY"
default_model = "claude-opus-4-7"

[policy]
mode            = "ask"
auto_allow      = ["Read", "Glob", "Grep", "ListNotes", "SearchNotes"]
ask             = ["Write", "Edit"]
bash_allowlist  = ["git status", "git diff", "cargo *", "ls *", "rg *"]

[[tools.subprocess]]
name        = "FormatJson"
command     = "/absolute/path/to/examples/subprocess/format_json.py"
description = "Pretty-print a JSON string with sorted keys."
```

`api_key` (literal) and `api_key_env` (env-var name) are both
accepted — prefer `api_key_env` so secrets don't sit in TOML on disk.

The full schema is in [`specs/README.md`](specs/README.md). Use
`/paths` from inside oli to see exactly where it's reading config,
plugins, sessions, notes, and the policy allow-list from on **your**
machine.

---

## Extending oli

Five surfaces, increasing in how much they ask of you:

| Surface | Touches | Requires |
| --- | --- | --- |
| **Subprocess tool** | `config.toml` | An executable that reads JSON on stdin, writes a result on stdout. Any language. |
| **MCP server** | `config.toml` | An MCP-conformant server (stdio or SSE). |
| **Lua plugin** | A single `.lua` file in a discovery dir | Lua. Hot-reloads. |
| **Hook** | A Lua plugin OR an MCP-registered handler | Same as the surface that owns it. |
| **Native code** | Edit `src/` and rebuild | Rust. |

### Lua plugins

A plugin is a single Lua file that returns a table:

```lua
local plugin = { name = "word-count", version = "0.1.0" }

plugin.tools = {
  {
    name        = "WordCount",
    description = "Count words in a file.",
    parameters  = {
      type = "object",
      properties = { path = { type = "string" } },
      required   = { "path" },
    },
    execute = function(args, ctx)
      local body = ctx:tool("Read", { file_path = args.path })
      local _, n = body:gsub("%S+", "")
      return tostring(n) .. " words"
    end,
  },
}

return plugin
```

Drop it into one of the discovery dirs and oli picks it up:

| Path | Scope |
| --- | --- |
| `~/.config/oli/plugins/*.lua` | Global. |
| `<project>/.oli/plugins/*.lua` | Project. Shadows global of the same name. |

Iterate without restarting: `/plugins reload` re-scans both dirs and
swaps tools/hooks/slashes atomically. Failures land in
`/diagnostics`; a broken plugin never crashes the session.

The host bridge (`ctx`) gives plugins:

- `ctx:tool(name, args)` — dispatch any registered tool. Returns the
  string result. Async — Lua suspends until it resolves.
- `ctx:read_file(path)` / `ctx:write_file(path, content)` /
  `ctx:shell(cmd)` — sugar over `Read`/`Write`/`Bash`; all
  policy-gated.
- `ctx:prompt(text)` — spawn a fresh subagent loop with the same
  provider/model/policy and return its final message (10-turn cap).
- `ctx:get_state(key)` / `ctx:set_state(key, value)` — per-plugin,
  per-session bag.
- `ctx:ask_user(question)` — blocking stdin read. Use sparingly.
- `ctx:log(level, msg)` — surface a line in `/diagnostics`.

**Sandbox:** `os`, `io`, `require`, `dofile`, `loadfile`, `debug`, and
`package.loadlib` are removed. Filesystem and shell access flows
through `ctx:*` — which goes through the same policy gate as the
model's own tool calls.

Three runnable examples are in [`examples/plugins/`](examples/plugins):

- [`word_count.lua`](examples/plugins/word_count.lua) — a tool that
  composes the built-in `Read`.
- [`safety_net.lua`](examples/plugins/safety_net.lua) — a
  `pre_tool_use` hook that refuses destructive Bash commands, plus a
  slash command that reports per-session stats.
- [`redact_secrets.lua`](examples/plugins/redact_secrets.lua) — a
  `post_tool_use` hook that masks API-key-shaped strings out of Bash
  output before the model sees them.

See [`examples/README.md`](examples/README.md) for a 60-second tour
and the host API reference.

### Subprocess tools (MCP-lite)

For tools written in any language, register an external binary in
config:

```toml
[[tools.subprocess]]
name        = "FormatJson"
command     = "/absolute/path/to/format_json.py"
description = "Pretty-print a JSON string with sorted keys."
```

The arguments object is piped to stdin as JSON. The result is read
from stdout. A non-zero exit surfaces stdout + stderr to the model.
Use absolute paths — oli runs the subprocess from whatever cwd it was
launched in.

A working Python example with a three-tier testing guide lives at
[`examples/subprocess/`](examples/subprocess/).

### MCP servers

oli is a Model Context Protocol client (stdio + SSE). Add a server
in config and its tools show up in `/tools` alongside the built-ins:

```toml
[[mcp.servers]]
name      = "filesystem"
transport = "stdio"
command   = "npx"
args      = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]

[[mcp.servers]]
name      = "my-sse"
transport = "sse"
url       = "https://example.com/mcp"
```

`/mcp` shows per-server health and tool counts and lets you restart
failed servers without restarting oli. See [`specs/mcp.md`](specs/mcp.md)
for the protocol details (tools/list_changed refresh, transport
selection, etc.).

### Hooks

Hooks fire on `pre_tool_use` / `post_tool_use` / `stop` events and
can short-circuit or rewrite tool calls. They run **before** the
policy gate, so a hook can refuse a Bash command before the
allowlist even sees it.

Dispatch order: `pre_tool_use → policy → tool → post_tool_use`.

Return shapes:

| Return | Effect |
| --- | --- |
| `nil` / `false` | Continue normally. |
| `{ skip = "reason" }` from `pre_tool_use` | Short-circuit dispatch; the model receives `reason` as the tool result. |
| `{ replace = value }` from `post_tool_use` | Substitute the tool result. `value` may be a string or a JSON-encodable table. |

Hooks are registered from Lua plugins today; see
[`examples/plugins/safety_net.lua`](examples/plugins/safety_net.lua)
for a working `pre_tool_use` denial hook.

### Adding to the binary

If a surface above isn't enough — say you want a tool that needs Rust
async primitives, or a brand-new provider — the AGENTS.md "where to
add what" table is the map:

| Goal | Where |
| --- | --- |
| New tool | `src/tools/<name>.rs` impl `tools::Tool`; register in `src/bin/oli.rs`. |
| New provider | `src/providers/<name>.rs` impl `Provider`; wire into `providers::build()`. |
| New slash command | `src/repl/slash.rs`; register in `SlashRegistry::default_set_with_reloader`. |
| Model capability override | `[[caps]]` block in config, layered over defaults in `src/agent/caps.rs`. |

The test loop is fast (`cargo test --lib` is ~2s for the full
suite). TDD is the convention — see `AGENTS.md` for the discipline
that keeps the codebase small.

---

## Architecture

```
src/
├── bin/oli.rs       # CLI entry; wires startup, registers tools and hooks
├── bootstrap.rs     # shared startup and persisted-session wiring
├── agent/           # think → call → observe loop
│   ├── mod.rs       #   Agent + Memory trait
│   ├── context.rs   #   System prompt + AGENTS.md/CLAUDE.md ingestion
│   └── caps.rs      #   per-model capability table
├── providers/       # Provider trait + anthropic / openai_compat / fake
├── tools/           # built-ins: read, write, edit, bash, grep, glob, task,
│                    #            notes, subprocess
├── policy/          # auto_allow / ask / bash_allowlist + persisted allow-list
├── plugins/         # Lua runtime (mlua), discovery dirs, hot-reload
├── mcp/             # MCP clients (stdio + SSE)
├── hooks/           # PreToolUse / PostToolUse / Stop dispatch
├── repl/            # line-mode REPL + SlashRegistry + built-in slash commands
├── notes/           # cross-session note store (filesystem, TOML frontmatter)
├── config.rs        # layered TOML loader (global + project)
└── wizard_init.rs   # first-run config wizard
```

Five extension traits — `Tool`, `Provider`, `Policy`, `SlashCommand`,
`Hook` — and that's it. The agent loop is re-entrant (a tool
executor can spin up a fresh loop with its own message list and tool
budget; this is how `Task` and `ctx:prompt(...)` work). The hook
dispatcher is shared between built-in hooks and plugin-registered
hooks — one mechanism, two registration sources.

---

## Where to read next

| If you want to… | Read |
| --- | --- |
| Use oli day-to-day | [`docs/cheatsheet.md`](docs/cheatsheet.md) — every keybind, slash, file path, and feature flag. |
| Understand the design | [`specs/README.md`](specs/README.md) — mission, principles, in/out of scope, full config schema, plugin contract, roadmap. |
| Track what's shipped | [`specs/progress.md`](specs/progress.md) — phase-by-phase status with commit SHAs. |
| Write a plugin | [`examples/README.md`](examples/README.md) — 60-second tour + host API reference. |
| Modify the code | [`AGENTS.md`](AGENTS.md) — module map, "where to add what" table, testing conventions, gotchas. |
| Read the memory design | [`specs/memory.md`](specs/memory.md) — `Memory` trait + pluggable strategies (linear+compact, RAG, graph, hierarchical). |
| Read the MCP design | [`specs/mcp.md`](specs/mcp.md) — stdio + SSE transports, refresh semantics. |
