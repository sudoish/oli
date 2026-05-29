# oli cheatsheet

Every keyboard affordance and slash command in one place. Linked from
`oli --help` and discoverable inside the TUI via `/help`.

> Convention: `Ctrl+X` = Control, `Shift+X` = Shift, `[A]` = capital `A`
> as a literal keystroke (different from `[a]`).

## CLI

| Invocation                                         | What it does                                                                 |
| -------------------------------------------------- | ---------------------------------------------------------------------------- |
| `oli`                                              | Interactive TUI (default) or line-mode REPL on `--plain` / non-TTY.          |
| `oli -p "find callers of foo"`                     | One-shot mode. Streams the answer to stdout, exits.                          |
| `oli --resume <id>`                                | Resume a specific session by id (file stem in `~/.config/oli/sessions/`).    |
| `oli --continue`                                   | Resume the most recent session by mtime.                                     |
| `oli --strict -p "..."`                            | One-shot, deny every `Ask` policy decision (unattended-safe scripted runs).  |
| `oli --max-turns N`                                | Cap turns for this run (overrides `[agent].max_turns` config).               |
| `oli --plain`                                      | Force the line-mode REPL even on a TTY.                                      |
| `oli --inline`                                     | Render TUI inline in the host buffer (no alt-screen). Buffer-terminals.      |
| `oli --fullscreen`                                 | Force alt-screen even when auto-detection would pick inline mode.            |
| `oli init`                                         | Interactive `~/.config/oli/config.toml` setup on stdin.                      |
| `oli init --provider ollama`                       | Headless config bootstrap; defaults for everything.                          |
| `oli init --provider openrouter --api-key sk-...`  | Full non-interactive bootstrap.                                              |
| `oli init --provider ollama --force`               | Overwrite an existing config file.                                           |

## TUI

### Viewport modes

oli renders in one of two modes, picked by `--inline` / `--fullscreen`,
then `[ui].viewport`, then auto-detection.

- **Fullscreen** (default in a fresh terminal): owns the screen via
  alternate-screen, mouse capture on, focus events on. Exiting
  restores the prior terminal contents.
- **Inline** (default inside Neovim `:terminal`, VSCode integrated
  terminal, Emacs `term`): renders in a fixed block in the host
  buffer, no alt-screen, no mouse capture. The transcript stays as
  normal scrollback when oli exits. Use the **host buffer's** scroll
  affordances to scroll past the inline block; PgUp/PgDn inside oli
  still scroll oli's transcript.

Mouse-wheel ownership in inline mode: the host buffer owns the
wheel by default (so editor / VSCode scroll works). Set
`[ui].mouse = true` (or pass `--fullscreen`) to let oli capture it.

### Input box

| Key                         | Action                                                                  |
| --------------------------- | ----------------------------------------------------------------------- |
| `Enter`                     | Submit the current buffer as a prompt or slash command.                 |
| `Shift+Enter` / `Alt+Enter` | Insert a literal newline (multi-line prompts).                          |
| `Tab`                       | Accept the highlighted completion (slash names or `@path` candidates).  |
| `Shift+Tab`                 | Cycle backwards through completion candidates.                          |
| `Up` / `Down`               | Walk the persistent prompt history (single-line buffers only).          |
| `Esc`                       | Clear the input buffer (when no overlay/completion is open).            |

### Transcript scroll

| Key                                | Action                                                                       |
| ---------------------------------- | ---------------------------------------------------------------------------- |
| `PageUp` / `PageDown`              | Scroll one viewport. Reaches bottom → reattaches to live tail.               |
| `Ctrl+Home`                        | Jump to the top of the transcript.                                           |
| `Ctrl+End`                         | Jump to the bottom and reattach. The "↓ N new" badge clears on reattach.     |
| Mouse wheel up/down                | A few lines at a time; scroll-to-bottom reattaches.                          |

### Cancel / quit / undo

| Key                | Action                                                                                |
| ------------------ | ------------------------------------------------------------------------------------- |
| `Ctrl+C` (busy)    | Cancel the in-flight turn. Provider stream + Bash subprocess group both terminate.    |
| `Ctrl+C` (idle)    | Quit the TUI.                                                                         |
| `Ctrl+D`           | Quit the TUI.                                                                         |
| `Ctrl+E`           | Edit-and-rerun: undo the last user turn, drop its body back in the input box.         |
| `Ctrl+R`           | Open Ctrl-R history search (substring, newest-first; `Esc` cancels).                  |

### Approval modal

When the policy returns `Ask`, an approval modal pops over the transcript.

| Key                | Action                                                                                |
| ------------------ | ------------------------------------------------------------------------------------- |
| `y` / `Y`          | Allow this single call.                                                               |
| `n` / `N` / `Esc`  | Deny this call.                                                                       |
| `a`                | Allow the same `(tool, args)` fingerprint for the rest of this session.               |
| `[A]`              | Allow always — also writes the fingerprint to `~/.config/oli/policy-allow.json`.      |
| `d` / `D`          | Deny the fingerprint for the rest of this session.                                    |
| `PgUp` / `PgDn`    | Scroll the diff/preview body.                                                         |

### Overlays (other)

- `/help` opens an interactive command browser (arrow keys, `Esc` to close).
- `/<cmd> ?` shows a one-shot help card for a single command.
- `/sessions` opens a picker over `~/.config/oli/sessions/` — `Enter`
  copies the `--resume <id>` shell command to your clipboard via OSC52.

## Slash commands

Run any of these by typing `/<name>` at the input prompt and pressing
`Enter`. Append `?` (e.g. `/cost ?`) to see a description without
running the command.

| Command                    | What it does                                                                       |
| -------------------------- | ---------------------------------------------------------------------------------- |
| `/help`                    | Open the command browser (or list inline in the line-mode REPL).                   |
| `/clear`                   | Drop conversation history (system prompt is preserved).                            |
| `/cost`                    | Last call + session-total token usage.                                             |
| `/tools`                   | List every tool the agent has registered (built-ins + plugin + MCP).               |
| `/system`                  | Render the pinned system prompt (with `/system <text>` to overwrite).              |
| `/memory`                  | Memory stats: record + pinned counts, summary state.                               |
| `/compact`                 | Force a memory compaction pass against the active provider.                        |
| `/provider`                | List configured providers; `/provider <name>` swaps active.                        |
| `/model`                   | Show current model; `/model <id>` swaps. Lists provider-supported ids if exposed.  |
| `/sessions`                | Open the session picker overlay (TUI) or list ids (line-mode).                     |
| `/plugins`                 | Show loaded plugins; `/plugins reload` re-scans plugin dirs without restarting.    |
| `/mcp`                     | Show MCP server health, tool counts, restart failed servers.                       |
| `/config reload`           | Re-parse `~/.config/oli/config.toml` (and project-local overlay) and apply live.   |
| `/diagnostics`             | Recent operational log (plugin warnings, MCP failures, etc.).                      |
| `/diagnostics clear`       | Wipe the diagnostics ring buffer.                                                  |
| `/exit`                    | Leave the REPL/TUI (also `Ctrl+D`).                                                |

Plugins and MCP servers can register more — they appear in the same
listing.

## Input syntax

- `@path` triggers file-path completion. Open it with `@` at any word
  boundary; `Tab` accepts the highlighted candidate.
- `/<cmd>` at the start of the input is a slash command. Inside a
  prompt body, `/` is just a character.
- `Shift+Enter` (or `Alt+Enter`) inserts a newline; `Enter` alone
  submits.

## Files

| Path                                       | What's there                                                         |
| ------------------------------------------ | -------------------------------------------------------------------- |
| `~/.config/oli/config.toml`                | Provider/model/policy/MCP/plugin config. Generated by `oli init`.    |
| `~/.config/oli/sessions/<id>.jsonl`        | Per-session JSONL transcript. `--resume <id>` replays it.            |
| `~/.config/oli/tui-history.jsonl`          | Persistent prompt history (Up/Down + Ctrl+R search).                 |
| `~/.config/oli/tui-hints.json`             | Hint ids the user has dismissed (faded onboarding tips).             |
| `~/.config/oli/policy-allow.json`          | Fingerprints persisted via `[A]` on the approval modal.              |
| `~/.config/oli/plugins/`                   | Lua plugin directory, scanned at startup and on `/plugins reload`.   |
| `<project>/.oli/config.toml`               | Optional project-scoped overlay; layered on top of the global file.  |
| `<project>/.oli/notes/`                    | Long-term notes store (`WriteNote`/`SearchNotes`/`ListNotes`).       |

## Environment variables

| Variable          | Effect                                                                                |
| ----------------- | ------------------------------------------------------------------------------------- |
| `XDG_CONFIG_HOME` | Override the `~/.config` root for all oli files.                                      |
| `RUST_LOG`        | Stderr threshold for the diagnostics shim. `info` (default) / `warn` / `error` etc.  |
| `COLORFGBG`       | Auto-detect light vs dark terminal theme for the markdown renderer.                   |
| `NVIM`, `NVIM_LISTEN_ADDRESS` | Detected at startup → oli treats the host as a buffer-terminal (inline mode, no OSC52, no DA queries, no mouse capture). |
| `TERM_PROGRAM=vscode`, `VSCODE_INJECTION` | Same: detected as buffer-terminal. |
| `INSIDE_EMACS`    | Detected as buffer-terminal (Emacs `term`).                                           |

## Build features

| Feature              | What it pulls in                                                            |
| -------------------- | --------------------------------------------------------------------------- |
| `tui` (default)      | ratatui + crossterm + tui-textarea-2 + pulldown-cmark.                      |
| `syntax-highlight`   | syntect (code-fence syntax coloring inside the assistant pane).             |

`cargo build --no-default-features` produces a line-mode-only binary
~2-3 MB smaller. Useful for piped CI usage where the TUI is dead weight.
