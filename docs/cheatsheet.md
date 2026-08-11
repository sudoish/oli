# oli cheatsheet

The compact reference for Oli's headless CLI and optional line REPL.

## Headless CLI

| Invocation | What it does |
| --- | --- |
| `oli run -p "find callers of foo"` | Create a persisted conversation, print the final response, and exit. |
| `printf 'find callers of foo' \| oli run` | Read the prompt from non-terminal stdin. |
| `oli run --conversation <id> -p "continue"` | Append a turn to a saved conversation. |
| `oli run --continue -p "continue"` | Append to the most recently modified conversation. |
| `oli run --output json -p "..."` | Print one JSON object containing conversation id, response, provider, model, and usage. |
| `oli run --strict -p "..."` | Force ask-mode policy and deny every unresolved approval non-interactively. |
| `oli run --max-turns N -p "..."` | Override `[agent].max_turns` for this run. |

Text mode writes only the final response to stdout and writes
`conversation: <id>` to stderr. Diagnostics and failures also use stderr.
Headless runs never wait for an approval answer.

## Interactive REPL

Invoke `oli` without a subcommand. The REPL prints its session id, streams
progress in the terminal, and supports interactive approval when policy mode is
`ask`.

| Input | Action |
| --- | --- |
| `Enter` | Submit the current prompt. |
| `Ctrl+C` while running | Cancel the current turn. |
| `Ctrl+C` at the prompt | Clear the current input. |
| `Ctrl+D` | Exit. |

## Slash commands

| Command | What it does |
| --- | --- |
| `/help` | List registered commands. |
| `/clear` | Drop conversation history while preserving the system prompt. |
| `/cost` | Show last-call and session-total token usage. |
| `/tools` | List built-in, plugin, and MCP tools. |
| `/system` | Show or replace the pinned system prompt. |
| `/memory` / `/compact` | Inspect or compact memory. |
| `/provider` / `/model` | Inspect or switch the active provider/model. |
| `/sessions` | List persisted conversation ids. |
| `/plugins` / `/plugins reload` | Inspect or reload Lua plugins. |
| `/mcp` | Show MCP server health and restart failed servers. |
| `/config reload` | Reload global and project configuration. |
| `/paths` | Print resolved config, session, note, plugin, and policy paths. |
| `/diagnostics` | Show local operational warnings. |
| `/exit` | Exit the REPL. |

## Approval answers in the REPL

| Answer | Effect |
| --- | --- |
| `y` | Allow this invocation. |
| `n` | Deny this invocation. |
| `a` | Allow this fingerprint for the current process. |
| `A` | Persist the fingerprint in `~/.config/oli/policy-allow.json`. |
| `d` | Deny this fingerprint for the current process. |

## Files

| Path | Contents |
| --- | --- |
| `~/.config/oli/config.toml` | Global provider, model, policy, MCP, and tool configuration. |
| `<project>/.oli/config.toml` | Project-scoped overlay found by walking upward from cwd. |
| `~/.config/oli/sessions/<id>.jsonl` | Persisted conversation transcript and read-set events. |
| `~/.config/oli/policy-allow.json` | Persisted approval fingerprints. |
| `~/.config/oli/plugins/` | Global Lua plugins. |
| `<project>/.oli/notes/` | Long-term notes used by the notes tools. |

## Setup and authentication

| Invocation | What it does |
| --- | --- |
| `oli init` | Configure a provider using stdin prompts. |
| `oli init --provider ollama` | Write the Ollama defaults non-interactively. |
| `oli login` | Authenticate with a ChatGPT subscription. |
| `oli login --device-auth` | Authenticate on a headless or remote machine. |
| `oli login --check` | Refresh credentials, discover models, and run the release-check prompt. |
| `oli logout` | Remove stored ChatGPT credentials. |
