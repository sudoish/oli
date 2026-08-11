# examples

Small, working extensions for oli's two extension surfaces:

- **Lua plugins** (`plugins/`) — drop a `.lua` file into a discovery dir, no rebuild.
- **Subprocess tools** (`subprocess/`) — register an external binary in `config.toml`, no rebuild.

If you're new to oli and want to understand what these surfaces can do,
read this whole file then poke each example file. They're each under 60
lines and try to do one thing well.

## Quick start (60-second tour)

Drop one example in, launch oli, confirm it loaded:

```sh
# from the repo root, after `cargo build`
mkdir -p .oli/plugins
cp examples/plugins/safety_net.lua .oli/plugins/
./target/debug/oli
```

Then inside the session:

```
> /plugins
Loaded plugins (1):
  safety-net (v0.1.0)  source=.oli/plugins/safety_net.lua
    slash: /safety-net-stats
    hooks: pre_tool_use

> /tools
Registered tools (11):
  Read         …
  Bash         …
  …
```

If `/plugins` shows your file with the right tools/hooks, you're wired up.
If it doesn't show up at all, `/diagnostics` will tell you what failed
during load (syntax error, missing return, etc.).

Hot reload while iterating: `/plugins reload` re-scans the discovery dirs
and swaps tools/hooks/slashes atomically. No restart needed.

## Discovery dirs

| Path | Scope |
|---|---|
| `~/.config/oli/plugins/` | All projects (global) |
| `<project>/.oli/plugins/` | Just this project (overrides global of same name) |

Files must end in `.lua` and `return` a table. Anything else is ignored.

## Lua plugins

A plugin is a Lua file that returns a table with up to three keys:

```lua
local plugin = { name = "my-plugin", version = "0.1.0" }
plugin.tools          = { ... }   -- callable by the model
plugin.slash_commands = { ... }   -- callable by the user via /name
plugin.hooks          = { pre_tool_use = ..., post_tool_use = ..., stop = ... }
return plugin
```

### What's in the box

| File | What it shows |
|---|---|
| [`word_count.lua`](plugins/word_count.lua) | A tool with a JSON-Schema parameter spec, composing built-in `Read` via `ctx:tool(...)`. |
| [`safety_net.lua`](plugins/safety_net.lua) | A `pre_tool_use` hook returning `{ skip = "..." }` to refuse destructive Bash commands, plus a `/safety-net-stats` slash command reading per-session state. |
| [`redact_secrets.lua`](plugins/redact_secrets.lua) | A `post_tool_use` hook returning `{ replace = "..." }` to mask API-key-shaped strings out of Bash output before the model sees them. |

### `ctx` — the host bridge

Every tool/hook/slash callback receives a `ctx` table:

| Call | What it does |
|---|---|
| `ctx:tool(name, args)` | Dispatch any host tool by name. Returns the tool's string result. Async — Lua suspends until it resolves. |
| `ctx:read_file(path)` | Sugar for `ctx:tool("Read", {file_path = path})`. |
| `ctx:write_file(path, content)` | Sugar for `ctx:tool("Write", ...)`. |
| `ctx:shell(cmd)` | Sugar for `ctx:tool("Bash", {command = cmd})`. Policy-gated. |
| `ctx:prompt(text)` | Spawn a fresh subagent and return its final message. Capped at 10 turns. |
| `ctx:get_state(key)` / `ctx:set_state(key, value)` | Per-plugin, per-session key/value bag. Persists across calls in one session; resets at process exit. |
| `ctx:ask_user(question)` | Blocking stdin read. Freezes the loop until the user answers — use sparingly. |
| `ctx:log(level, msg)` | Surface a line in `/diagnostics`. `level` is `"error"` / `"warn"` / `"info"` / `"debug"`. |

### Hook return shapes

| Return | Effect |
|---|---|
| `nil`, `false`, or any value you didn't ask for | Continue normally. |
| `{ skip = "reason" }` from `pre_tool_use` | Short-circuit dispatch. The model receives `reason` as the tool result. |
| `{ replace = value }` from `post_tool_use` | Substitute the tool result with `value`. `value` may be a string or a JSON-encodable table. |

Hooks fire **before** the policy gate, so a plugin can short-circuit a
Bash call before the bash_allowlist even sees it. Dispatch order, from
`src/agent/mod.rs`: `pre_tool_use → policy → tool → post_tool_use`.

### Sandbox

Plugins run in a sandboxed Lua state. Removed globals: `os`, `io`,
`require`, `dofile`, `loadfile`, `debug`, and `package.loadlib`. The
intent is that filesystem and shell access flows through `ctx:read_file`
/ `ctx:write_file` / `ctx:shell` — which go through the same policy gate
as the model's own tool calls — rather than bypassing it.

### Naming gotcha

A plugin has three "names" and they don't have to agree:

- **File stem** — used for plugin IDs in `/diagnostics`, `[plugin:...]` log prefixes, and `Hook::name()`.
- **`plugin.name`** field — used as the display name in `/plugins`.
- **Tool / slash / hook `name`** — what the model or user types.

If you're filtering hooks by plugin (rare, mostly relevant in tests),
the value you want is the file stem, not `plugin.name`. The convention
in this repo is to keep them as close as possible — e.g. file
`safety_net.lua` → `plugin.name = "safety-net"`.

## Subprocess tools

External binaries that speak a tiny JSON-over-stdio protocol:

- The arguments object is piped to stdin as JSON.
- The result is read from stdout.
- A non-zero exit surfaces stdout + stderr to the model.

Register them in `~/.config/oli/config.toml` (global) or
`.oli/config.toml` (project). Use **absolute paths** — oli runs the
subprocess from whatever cwd it was launched in, not from the project
root, so a relative path breaks the moment you `cd` somewhere.

| File | What it shows |
|---|---|
| [`format_json.py`](subprocess/format_json.py) | Pretty-print a JSON string with sorted keys. Exits 2 on schema errors, 1 on malformed input JSON. See [`subprocess/README.md`](subprocess/README.md) for registration TOML and a three-tier testing guide. |

This is "MCP-lite": same contract as a tool over Model Context Protocol
minus the protocol negotiation. Three lines of config and a script in
any language = new tool, no recompile.

## Iterating on a plugin

1. Edit the `.lua` (or `.py`).
2. Inside oli: `/plugins reload` (for Lua) or `/config reload` (for subprocess).
3. `/diagnostics` if anything looks off — load errors, sandbox violations, hook errors all land there.
4. `/plugins` and `/tools` to confirm the new shape registered.

A failed plugin never crashes the session — the loader logs the error
to `/diagnostics` and skips the file, so the other plugins still load.

## Where to go next

- Look at the inline tests in `src/plugins/mod.rs` for more API examples.
- `AGENTS.md` at the repo root has the broader architecture map.
- `src/plugins/mod.rs` top-of-file docs (lines 1–60) have the canonical API reference — this README is its newcomer-friendly counterpart.
