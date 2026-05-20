# examples

Small, working extensions that exercise oli's two extension surfaces.

## Lua plugins (`examples/plugins/`)

Plugins are `.lua` files discovered from
`~/.config/oli/plugins/` (global) and `<project>/.oli/plugins/`
(project-scoped). Each file returns a table with any of
`tools`, `slash_commands`, and `hooks`. The runtime is sandboxed:
`os`, `io`, `package.loadlib`, `dofile`/`loadfile`, and `debug` are
removed before evaluation; filesystem and shell access go through
`ctx:read_file` / `ctx:write_file` / `ctx:shell`, which dispatch
through the same policy gate as the model's own tool calls.

To try one of these locally:

```sh
mkdir -p .oli/plugins
cp examples/plugins/safety_net.lua .oli/plugins/
oli  # then check `/plugins` to confirm it loaded
```

| File | What it shows |
|---|---|
| `word_count.lua` | Registering a tool with a JSON-Schema parameter spec and composing the built-in `Read` tool via `ctx:tool(...)`. |
| `safety_net.lua` | A `pre_tool_use` hook that returns `{ skip = "..." }` to refuse destructive Bash commands. |
| `redact_secrets.lua` | A `post_tool_use` hook that returns `{ replace = "..." }` to mask API-key-shaped strings out of tool results before the model sees them. |

### Plugin API quick reference

`ctx` is the host bridge passed into every tool/hook/slash:

- `ctx:tool(name, args)` — dispatch any registered host tool; returns its string result.
- `ctx:read_file(path)` / `ctx:write_file(path, content)` — sugar for `Read` / `Write`.
- `ctx:shell(cmd)` — dispatch `Bash` (policy-gated).
- `ctx:prompt(text)` — spawn a fresh subagent and return its final message.
- `ctx:get_state(key)` / `ctx:set_state(key, value)` — per-plugin, per-session key/value bag.
- `ctx:ask_user(question)` — blocking stdin read; use sparingly.
- `ctx:log(level, msg)` — surface a line in `/diagnostics` (`error` / `warn` / `info` / `debug`).

Hook return shapes:

- `nil` / `false` / anything unrecognized → continue.
- `{ skip = "reason" }` on `pre_tool_use` → short-circuit dispatch; the model receives the reason as the tool result.
- `{ replace = value }` on `post_tool_use` → substitute the tool's result with `value` (string or JSON-encodable table).

## Subprocess tools (`examples/subprocess/`)

Subprocess tools are external binaries registered via TOML config —
no rebuild required. They speak a tiny JSON-over-stdio protocol:
the arguments object is piped to stdin, the result is read from
stdout, and a non-zero exit code surfaces stderr to the model.

| File | What it shows |
|---|---|
| `format_json.py` | Pretty-print a JSON string with sorted keys. Validates input shape, exits 2 on schema errors, 1 on bad JSON. See [`subprocess/README.md`](subprocess/README.md) for registration + a three-tier testing guide. |

Register it in `~/.config/oli/config.toml` (global) or
`.oli/config.toml` (project) under the `[tools]` table:

```toml
[[tools.subprocess]]
name = "FormatJson"
command = "python3"
args    = ["examples/subprocess/format_json.py"]
description = "Pretty-print a JSON string with sorted keys."

[tools.subprocess.parameters]
type = "object"
required = ["json"]

[tools.subprocess.parameters.properties.json]
type = "string"
description = "The JSON document to pretty-print."

[tools.subprocess.parameters.properties.indent]
type = "integer"
description = "Indent width (0–8). Defaults to 2."
```

After editing config, run `/config reload` inside a session to pick
the tool up without restarting. `/tools` will list `FormatJson` and
`/diagnostics` will show any load errors.

The subprocess pattern is "MCP-lite": same contract as a tool over
Model Context Protocol minus the protocol negotiation. Any language
that can read stdin and write stdout works — the example is Python
3 for clarity, but a shell one-liner or a Go binary fits the same
shape.
