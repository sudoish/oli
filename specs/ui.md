# UI / DX Roadmap (superseded by `specs/tui.md`)

> **Note:** this doc is the line-oriented + crossterm-sprinkles plan.
> The user opted to be more ambitious and build a full ratatui TUI
> instead. See `specs/tui.md` for the active plan; this file is kept
> for context on the rejected branch.

How the harness *feels* matters as much as what it does. Today's REPL
works — multi-turn streaming, slash commands, Ctrl-C cancel, diff
preview, progress hook — but it's line-oriented and bare. The goal
of this roadmap is the *ideal* DX for a coding agent in the terminal:
fast, discoverable, recoverable, readable.

This is the spec; per-phase notes land alongside it as
`specs/ui-<topic>.md` if any phase grows large enough to warrant one.

## Goals

1. **One-second-to-feel-fast.** Input is responsive (no perceptible
   lag on keypress); first model token visible within streaming
   latency.
2. **One-glance status.** A user can always answer: which model,
   which session, how much context is used, what's the harness
   doing right now.
3. **Friction-free approvals.** Single-key responses for the
   approval prompt; no enter, no full-line scrub.
4. **Discoverable.** Tab-completion for slashes, file paths, model
   names. `/help` is browsable.
5. **Readable.** Markdown rendered. Code blocks syntax-highlighted.
   Diffs colored. Errors with context, not stack-trace soup.
6. **Recoverable.** Undo the last prompt. Edit and re-run. Cancel a
   running tool cleanly. Search history.
7. **Trustworthy.** "Allow this command for the rest of the session"
   without dropping to the config file. State is visible, not
   magic.

## Non-goals (explicit)

- Mouse support. CLI users live on the keyboard; mouse adds
  surface area without adding fluency.
- IDE-style file tree, multi-pane layouts, project graphs. We're a
  chat agent, not an IDE.
- Image rendering, charts, dashboards.
- A graphical desktop wrapper.
- Vim mode (deferred — `reedline` supports it; we ship emacs first
  and add a mode toggle if there's demand).

## Current state

The REPL today (`src/repl/`):

- **Input**: `rustyline::DefaultEditor` — single-line, basic emacs
  bindings, in-memory history, no completion hooks, no multi-line.
- **Slash commands**: 12 built-ins (`/clear`, `/help`, `/cost`, …)
  plus `/mcp` and any plugin-registered ones. Dispatched from
  `repl::run` via `SlashRegistry::dispatch`.
- **Streaming**: assistant content streams to stdout as raw text.
  No markdown rendering, no syntax highlight.
- **Tool progress**: `ProgressHook` prints `→ Tool(file_path=…)` to
  stderr on `PreToolUse`. Single line, clipped to 60 chars, no
  timing, no completion marker.
- **Approval**: `ReadlineApprover` reads `y/N` via stdin on a
  blocking task; renders a tool-aware preview (unified diff for
  Edit/Write since Phase D).
- **Cancel**: Ctrl-C truncates memory back to the pre-turn `len()`
  via `tokio::select!` against the in-flight future.
- **Status**: none. Session id is printed once at the start of a
  resumed session and never again.

## Approach

The original spec said *"TUI / ratatui rendering"* is out of scope
indefinitely. That decision still mostly holds — a full ratatui
runtime is too heavy for the harness's "minimal, hackable, single-
binary" ethos and pushes the core well past the 2500-LOC line.

But there's a middle path that gets ~80% of the DX win:

- **`reedline`** for input. Same project that powers `nushell`. Gives
  us multi-line, hints, autocompletion, history search, persistent
  history, validators — all behind one trait. Drop-in replacement
  for `rustyline`.
- **`crossterm`** for ANSI capabilities, raw-mode key reads (the
  approval prompt), and the always-visible status line.
- **`syntect`** (or `tree-sitter-highlight`) for code-block
  syntax highlighting.
- A small markdown renderer that emits ANSI — bold, italic, headers,
  lists, fenced code (passed through syntect). Either pull
  `termimad` or write ~200 LOC ourselves. The body of typical
  assistant content isn't that complex.

We keep the *transcript* as plain stdout (so users can scroll and
copy with their terminal), and overlay:

- A status line (always at the bottom).
- Inline tool-call cards (drawn in place, updated on completion).
- Single-key approval prompts (raw mode, restored on return).
- Slash/path completion menus (drawn as a popup above the prompt).

This is line-oriented + sprinkles of crossterm for the high-leverage
moments. No full alternate-screen TUI; users still get terminal
scrollback for free.

## Phases

Each phase ends in something usable; you can stop at any phase
boundary.

### Phase F — Input ergonomics (~2d)

Replace `rustyline` with `reedline`. Most of the rest follows from
that swap.

- **F1. Multi-line input.** Shift+Enter inserts a newline; Enter
  submits. Reedline's `Validator` trait enforces "submit only when
  the user explicitly hits Enter on a non-continuation line."
  Pasted code blocks finally work.
  - Files: `src/repl/mod.rs` (swap editor); add
    `src/repl/input/validator.rs`.
  - Done when: pasting a 30-line snippet doesn't fragment turns.

- **F2. Slash command autocomplete.** Tab on `/m<TAB>` cycles
  through `/memory`, `/model`, `/mcp`. Hint text greys forward
  while typing.
  - Files: `src/repl/input/completer.rs` (impl `Completer` against
    the live `SlashRegistry`).
  - Done when: every registered slash is reachable via tab; plugin
    slashes added by `/plugins reload` show up immediately.

- **F3. File path autocomplete with `@`.** Typing `@src/m<TAB>`
  expands to `@src/main.rs`. Glob-based (no fancy fuzzy yet) so
  it's obvious what's being matched.
  - Files: `src/repl/input/completer.rs` extends F2's completer.
  - Done when: `@<TAB>` in the repo root lists top-level entries;
    descending into directories works.

- **F4. History search.** Ctrl-R opens a search-as-you-type prompt
  over prior turns. `reedline` ships this; we just enable it.
  - Done when: typing a substring of a prior prompt narrows the list
    incrementally; Enter re-runs it.

- **F5. Persistent history across sessions.** Stored at
  `~/.config/oli/history` (line-per-entry). Reedline file-backed
  history.
  - Done when: history survives REPL restart.

### Phase G — Streaming output polish (~2d)

- **G1. ANSI color baseline.** Colors via `owo-colors` (or
  `nu-ansi-term`) — small dependency, no global state. Use
  `is_terminal()` to fall back to plain when piped to a file.
  - Done when: `→ Read(...)` is dim, `✓` is green, errors are red,
    in a TTY; piping to `oli -p ... > out.txt` strips ANSI.

- **G2. Markdown in assistant content.** Render bold, italic,
  headers (small `▌ ` gutter), lists, links (`text` underlined +
  `(url)` faded), inline code. Block code goes through G3.
  - Two routes: `termimad` (works, opinionated rendering) or write
    ~150 LOC against a minimal markdown parser. Lean toward
    termimad — render quality matters and the dep is small.
  - Done when: `**bold**`, `# Heading`, `- item`, `[link](url)`
    look right in iTerm2, Apple Terminal, Alacritty, kitty.

- **G3. Syntax-highlighted code fences.** Code blocks (` ``` `)
  pick up language hint and render with `syntect`. Default theme
  follows `$COLORFGBG` (light vs dark) when present, else dark.
  - Done when: ```rust fn main() { ... } ``` blocks come out
    highlighted; unknown-language fences fall back to dim mono.

- **G4. Tool-call cards.** Replace the one-line stderr
  `→ Tool(...)` with a two-line block:
  ```
  → Read   src/main.rs                                    0.04s
    37 lines
  ```
  Drawn in place via cursor save/restore: PreToolUse paints the
  first line; PostToolUse appends timing and a result summary
  (line count for Read, cmd exit for Bash, etc).
  - Files: `src/repl/render/tool_card.rs` + extend `ProgressHook`
    in `src/repl/mod.rs`.
  - Done when: a multi-tool turn shows a stack of completed cards
    above the assistant's final response.

- **G5. Thinking spinner.** Between user submit and first model
  token, show a spinner with elapsed time on the same line. Clear
  on first chunk arrival.
  - Done when: `oli` against an Ollama model that takes 4s to first
    token shows `· thinking (3.2s)` updating live.

### Phase H — Approval UX (~0.5d)

- **H1. Single-key approval.** Raw mode prompt:
  ```
  [Edit src/x.rs] approve? [y]es / [n]o / [a]llow this session / [d]eny session
  ```
  No Enter required. Restores cooked mode on return. Plain
  `y`/`n`/`a`/`d` keys; ESC cancels (acts as `n`).
  - Files: `src/policy/approver.rs` extracted from `src/policy/mod.rs`.
  - Done when: a single keystroke completes the prompt; ANSI is
    cleaned up on cancel.

- **H2. Session-allow shortcut.** `a` adds the (tool, args-pattern)
  to an in-memory session allowlist; subsequent matching calls
  auto-approve without prompting. `d` does the inverse for the
  session.
  - Done when: approving an `Edit src/main.rs` once and then
    issuing another Edit on the same file doesn't re-prompt.

- **H3. Pager for long diffs.** Diffs > 50 lines get a "press
  space to view, q to skip" hint and open `$PAGER` (or fall back
  to inline scroll) on space.
  - Done when: a 200-line write diff doesn't flood the terminal;
    user can paginate.

- **H4. Persist trust.** `T` on the approval prompt says "allow this
  exact command pattern, persist to `~/.config/oli/config.toml`'s
  `[policy].bash_allowlist`." Confirm the write before doing it.
  - Done when: `cargo bench` pre-approved across sessions after one
    `T` confirmation.

### Phase I — Status & navigation (~1d)

- **I1. Always-on status line.** Sticky at the bottom of the
  terminal:
  ```
  ▌ session 1714415000123  •  claude-opus-4.7  •  4.2k / 200k tokens  •  main *  •  cost $0.04
  ```
  Updates on every `last_usage` change, on `/provider` swap, on
  `/clear`. Width-aware; collapses fields on narrow terminals.
  - Files: `src/repl/render/status.rs`.
  - Done when: the line is visible, accurate, and stays at the
    bottom across scrollback.

- **I2. Scrollback navigation hint.** Status line shows `↑↓ scroll`
  when the user has scrolled up; reminds them how to get back.
  Optional — doesn't fight terminal native scroll.

- **I3. Interactive session picker.** `/sessions` opens a
  reedline-style menu of recent sessions with timestamp +
  first-prompt preview. Enter picks one, ESC cancels.
  - Done when: `/sessions` no longer prints a wall of ids; arrow-
    keys + Enter reach the desired session.

- **I4. Live token / cost gauge.** Same data as `/cost`, surfaced
  as a small bar in the status line. Color shifts amber at 70%
  context-window usage, red at 90%.
  - Done when: a long session visibly shows the budget filling up.

### Phase J — Recoverability (~0.5d)

- **J1. `/undo`.** Drops the last user turn and the assistant's
  response (and any tool round-trips) from memory. Implementation
  rides the existing `Memory::truncate` machinery.
  - Done when: `/undo` rolls back exactly one prompt; running it
    twice goes back two; `/undo` with empty history is a no-op
    with a hint.

- **J2. Edit-last.** Ctrl+E opens the last user prompt in `$EDITOR`,
  re-submits on save. Equivalent to `/undo` + new prompt.
  - Done when: tweaking a prompt and re-running doesn't require
    retyping.

- **J3. Better tool-cancel.** Ctrl-C while a Bash tool is running
  shows `· interrupting Bash...` and SIGINTs the child. Today's
  cancel only drops the future at the agent level; the child
  process keeps running until it notices stdin/stdout breakage.
  - Files: bash tool needs to expose a kill handle; integrate with
    the REPL's signal handler.
  - Done when: `Bash(command="sleep 60")` Ctrl-C'd at second 2
    sees the child gone within ~1s.

### Phase K — Discoverability (~0.5d)

- **K1. `/help` browser.** Replace the flat printout with an
  interactive list: arrow keys to highlight, Enter to show full
  description and example invocations, ESC to close.
  - Done when: a new user can browse all available commands
    without leaving the REPL.

- **K2. First-run wizard.** `oli init` (and an interactive prompt
  on REPL start when no config exists) walks through provider
  setup: pick provider, paste API key (masked), pick default
  model, save to `~/.config/oli/config.toml`.
  - Done when: a fresh user goes from `oli` to a working session
    without reading the README.

- **K3. Inline command help.** Typing `/<cmd> ?` (or just hitting
  Tab on a known slash) shows inline help for that command.
  - Done when: `/model ?` lists usage and available models.

- **K4. Onboarding hints.** First-time tips that fade after the
  user has used the relevant feature once. E.g. on first
  approval prompt: "Press `a` to allow this for the whole
  session."
  - Done when: hints don't keep nagging an experienced user.

## Suggested ordering & milestones

- **Week 1 ship:** F1–F5 + G1 + H1. Multi-line + completion +
  history search + colored output + single-key approval. This
  alone is a step-change in feel — and lands without termimad or
  syntect dependencies.
- **Week 2 ship:** G2–G5 + I1–I2. Markdown / syntax highlight /
  tool cards / spinner / status line. The harness now *looks*
  professional.
- **Week 3 ship:** H2–H4, I3–I4, J1–J3, K1–K4. Trust controls,
  recoverability, discoverability. Everything past "looks great"
  into "feels great."

## Acceptance for "ideal DX"

A user can:

- Paste 30 lines of code and submit them as a single turn.
- Type `/sess<TAB>` to autocomplete `/sessions`, see an interactive
  picker, arrow-key to a recent one, Enter to resume.
- Type `@src/m<TAB>` to autocomplete `@src/main.rs` into the input.
- Watch the assistant render markdown: `**bold**` is bold, ``` ```rust ```
  is syntax-highlighted, `# Heading` has a small gutter.
- See tool calls as cards: `→ Read src/main.rs · 37 lines · 0.04s`.
- Approve `Edit` with one keystroke `y`; press `a` to skip prompts
  for the rest of the session on Edits in the same file.
- Look at the bottom of the screen and know which model, session,
  token budget, git branch they're on.
- Ctrl+R to search history, Ctrl+E to edit-and-rerun the last
  prompt.
- Run `/undo` to roll back a regrettable turn.
- Ctrl-C a runaway `Bash` and see the child actually killed in <1s.

## Open decisions

- **Markdown renderer**: termimad vs hand-rolled vs `mdcat` library.
  Lean termimad — render quality matters, dep is small, no
  unsafe, no global state.
- **Syntax highlighter**: syntect vs tree-sitter. syntect is a
  smaller dep with good defaults; tree-sitter has better
  highlighting for some langs but pulls in a lot of grammar
  crates. Pick syntect first, revisit if quality is an issue.
- **Status line implementation**: crossterm's cursor save/restore
  per chunk vs alternate-screen mode. Keep alternate-screen *off*
  so users get native terminal scrollback; redraw the status line
  on every render.
- **`reedline` vs continuing with `rustyline`**: reedline gives us
  F1–F5 nearly for free; the swap is the cheapest path to most of
  Phase F. Adopt.
- **Should approvals be raw-mode by default for piped stdin?**
  No — when stdin isn't a TTY, fall back to `[y/N]` line input
  (or `--strict` denies). Detect via `is_terminal`.
- **Vim mode**: deferred. Reedline supports it; revisit if a user
  asks.
- **TUI graduation point**: revisit if Phase G/I rendering grows
  past ~600 LOC of crossterm cursor manipulation. At that point a
  ratatui adoption is probably cheaper.

## Status tracker

Mirror commit SHAs into `specs/progress.md` at each phase boundary.

| ID | Item                                              | Status |
| -- | ------------------------------------------------- | ------ |
| F1 | Multi-line input via reedline                     | TODO   |
| F2 | Slash autocomplete on Tab                         | TODO   |
| F3 | File path autocomplete with `@`                   | TODO   |
| F4 | Ctrl-R history search                             | TODO   |
| F5 | Persistent history across sessions                | TODO   |
| G1 | ANSI color baseline                               | TODO   |
| G2 | Markdown rendering for assistant content          | TODO   |
| G3 | Syntax highlighting in code fences                | TODO   |
| G4 | Tool-call cards with timing                       | TODO   |
| G5 | Thinking spinner                                  | TODO   |
| H1 | Single-key approval                               | TODO   |
| H2 | Session-allow shortcut                            | TODO   |
| H3 | Pager for long diffs                              | TODO   |
| H4 | Persist trust to config                           | TODO   |
| I1 | Always-on status line                             | TODO   |
| I2 | Scrollback navigation hint                        | TODO   |
| I3 | Interactive session picker                        | TODO   |
| I4 | Live token / cost gauge                           | TODO   |
| J1 | `/undo` rolls back last turn                      | TODO   |
| J2 | Ctrl+E edit-and-rerun                             | TODO   |
| J3 | Bash child SIGINT on Ctrl-C                       | TODO   |
| K1 | Interactive `/help` browser                       | TODO   |
| K2 | First-run setup wizard                            | TODO   |
| K3 | Inline `/<cmd> ?` help                            | TODO   |
| K4 | Onboarding hints                                  | TODO   |
