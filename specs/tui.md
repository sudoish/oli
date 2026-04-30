# TUI Roadmap

Supersedes `specs/ui.md` (the line-oriented + crossterm-sprinkles
plan). User direction: **be ambitious, build a real TUI on
ratatui**. This doc is the vision and the build plan.

## Mission

Replace the line-oriented `rustyline` REPL with a polished
ratatui-based TUI that *feels* like a modern coding agent: a
scrollable transcript with markdown + syntax-highlighted code, tool
calls rendered as cards with live progress, an always-visible
status bar, frictionless modal approvals, and an input area with
multi-line, completion, and history search.

The line-mode REPL is *retained* as `oli --plain` for piped use,
SSH sessions on minimal terminals, and CI scripts. `oli -p` stays
non-interactive. The TUI becomes the default for `oli` with no
flag.

## Goals

1. **Looks great in iTerm2, Alacritty, kitty, Apple Terminal.**
   Renders correctly at 80×24 and at 200×60. No flicker.
2. **Streams responsively.** First model token visible within
   provider streaming latency; redraws coalesced so a fast Ollama
   stream doesn't burn CPU.
3. **Scrollable transcript.** PgUp/PgDn, mouse wheel, `g`/`G` to
   top/bottom. "Stick to bottom" auto-resumes when streaming.
4. **Markdown + syntax highlight.** Bold, italic, headers, lists,
   links, fenced code with language-aware highlighting.
5. **Tool calls as cards.** `→ Read src/main.rs · 37 lines · 0.04s`
   inline in the transcript, status updates in place from running
   to ✓ / ✗.
6. **Single-key approval modal.** Diff preview, `y`/`n`/`a`/`d`,
   ESC cancels.
7. **Discoverable input.** Tab completion (slashes, file paths,
   models), Ctrl-R history search, multi-line paste, persistent
   history.
8. **Recoverable.** `/undo`, Ctrl+E to edit-and-rerun, Ctrl-C
   really kills running tools.
9. **Trustworthy state.** Status bar always answers: which model,
   session id, token budget (with color), git branch, cost.

## Non-goals (explicit)

- Mouse-driven UI (we accept wheel scroll only — keys are the
  language of the CLI user).
- File tree / project explorer / multi-pane IDE layouts.
- Image / chart / dashboard rendering.
- Full vim modal editing in the input area (deferred — the
  `tui-textarea` we'll likely use supports it; turn on later).
- Theming framework (one good default theme; light/dark switch
  via `$COLORFGBG` is the ceiling for now).

## Approach

**Full alternate-screen ratatui.** We take over the terminal,
render everything ourselves, restore on exit. The trade-off is
that native terminal scrollback no longer captures the transcript;
we own that experience instead. Three reasons it's worth it:

- Inline rendering (ratatui's `Viewport::Inline`) is lower
  fidelity and fights the natural "transcript at top" layout — a
  status bar doesn't really work in inline mode.
- Tool-call cards that update in place need precise cursor control
  that interleaves badly with stdout-style streaming.
- Modal approvals (diff preview overlaid on the transcript) need
  layering ratatui handles natively.

To keep the loss of native scrollback from biting:

- Implement Cmd-K / Ctrl-L style "clear screen" that just resets
  the visible viewport (doesn't drop transcript state).
- Persist transcripts to the existing JSONL so a user who wants to
  grep their session has the file.
- A `/copy <n>` command grabs the n-th-most-recent assistant
  message to the clipboard via OSC52 (works in modern terminals).
- A `--plain` fallback prints to stdout in the existing REPL shape
  for users who genuinely need terminal-scroll workflow.

### Stack

- **`ratatui`** for layout and widgets.
- **`crossterm`** for the backend (event loop, raw mode, alternate
  screen, mouse).
- **`tui-textarea`** for the input area (multi-line, vim mode toggle,
  selection — saves us reimplementing line editing).
- **`pulldown-cmark`** for markdown parsing.
- **`syntect`** for code-block syntax highlighting (loaded lazily —
  the syntax bundle is ~2 MB).
- **`tokio`** for the runtime; we keep one event channel funneling
  input events, agent stream chunks, and tool-card updates into the
  render loop.

### Architecture

```
              ┌─────────────────────────────────────────────┐
              │                Render task                  │
              │  ratatui::Terminal::draw(state)             │
              │  driven by tokio mpsc<UiEvent>              │
              └──────────────┬──────────────────────────────┘
                             │
       ┌─────────────────────┼─────────────────────┐
       │                     │                     │
┌──────┴──────┐      ┌───────┴───────┐     ┌───────┴───────┐
│ Input task  │      │  Agent task   │     │  Hook bridge  │
│ crossterm   │      │  Agent::run_  │     │  PreToolUse / │
│ event stream│      │  streaming    │     │  PostToolUse  │
│ → KeyEvent, │      │  → ContentSink│     │  → ToolCard   │
│   Resize,   │      │   pushes Tick │     │  events       │
│   Mouse     │      │   chunks      │     │               │
└─────────────┘      └───────────────┘     └───────────────┘
```

One `mpsc::UnboundedSender<UiEvent>` is shared. Producers:

- **Input task**: a `tokio::spawn` reading `crossterm::event::EventStream`,
  mapping crossterm events to `UiEvent::Input(KeyEvent)` etc.
- **Agent task**: spawned on user submit, runs `agent.run_streaming(prompt, &mut sink)`
  where the sink translates each chunk into `UiEvent::ContentChunk(s)`.
- **Hook bridge**: a `Hook` impl registered on the agent's
  `HookRegistry` that maps `PreToolUse`/`PostToolUse`/`Stop` into
  `UiEvent::ToolCard{...}`.

Render task owns the `App` state and processes `UiEvent`s. Each
event:

- Mutates `App` state (append chunk, update card, change mode).
- Triggers a single redraw via `Terminal::draw(|f| ui::draw(f, &app))`.

Coalescing: if multiple chunks arrive between draws (Ollama burst),
we drain the channel before drawing so we render once per frame
batch. Cap redraws at ~60 fps via a `tokio::time::interval`.

### App state shape

```rust
pub struct App {
    /// Logical transcript: a vec of items, each rendered into the
    /// scrollable area. Items are user prompts, assistant messages
    /// (mutable while streaming), tool cards, system notices.
    pub transcript: Vec<TranscriptItem>,
    /// Index of the active assistant message we're appending
    /// chunks to. None when not streaming.
    pub active_assistant: Option<usize>,
    /// Active tool cards keyed by tool_call_id so PostToolUse can
    /// find the corresponding PreToolUse card.
    pub active_tools: HashMap<String, usize>,
    pub mode: Mode,
    pub input: TextArea<'static>,
    pub scroll: ScrollState,
    pub status: StatusModel,
    pub approval: Option<ApprovalState>,
    pub completion: Option<CompletionMenu>,
}

pub enum Mode {
    Idle,
    Thinking { since: Instant },
    Streaming,
    AwaitingApproval,
    SessionPicker,
    HelpBrowser,
}

pub enum TranscriptItem {
    UserPrompt { body: String },
    Assistant { body: String, done: bool },
    ToolCard {
        tool: String,
        args_preview: String,
        state: ToolState, // Running { since } | Done { duration, summary, ok }
    },
    System { body: String },
}
```

### Coexistence with the line-mode REPL

- Default: `oli` launches the TUI.
- `oli --plain` keeps `repl::run` (today's rustyline path).
- `oli -p "prompt"` stays headless / non-interactive; no UI.
- TTY detection: if stdin or stdout isn't a TTY, fall back to
  `--plain` automatically.
- Both UIs share `Agent`, `HookRegistry`, `SlashRegistry`,
  `PluginReloader`, `McpHandle`s. The TUI is *another consumer* of
  the same harness, not a fork.

## Phases

Each phase ends in something usable. The first phase already gives
a working TUI shell; subsequent phases layer features.

### Phase F — TUI skeleton (1–2d)

Bring up the layout and the event loop. No agent integration yet —
echo-only.

- **F1. Module skeleton.** New `src/tui/` directory:
  `mod.rs`, `app.rs`, `event.rs`, `ui.rs`. `tui::run(agent, slashes, reloader)`
  as the entry point.
- **F2. Three-pane layout.** Status bar (1 row top), transcript
  (flex middle), input (3-row bottom). Resize-aware.
- **F3. Crossterm + alternate screen lifecycle.** Enable raw
  mode, enter alternate screen, mouse capture; restore on exit
  via Drop guard.
- **F4. Echo loop.** User types in the input, hits Enter, the
  text becomes a `UserPrompt` transcript item. No agent call yet.
- **F5. Quit / cancel.** Ctrl+D / `:q` exits cleanly; Ctrl+C in
  Idle exits, Ctrl+C in active modes cancels.
- **F6. Plain-mode fallback.** `oli --plain` flag wired into
  `main.rs`; the existing REPL becomes the secondary path.
- **Done when:** `oli` launches into a usable shell with input +
  transcript + status bar; resizing the terminal redraws cleanly;
  `oli --plain` still works; tests covering app state mutations
  pass.

### Phase G — Agent integration (~2d)

Wire the agent to the TUI so prompts actually run.

- **G1. Streaming sink → UI events.** `agent.run_streaming(prompt, &mut sink)`
  with a sink closure that pushes `UiEvent::ContentChunk(String)`
  onto the channel. Each chunk appends to the active assistant
  item; the TUI redraws.
- **G2. Mode transitions.** On submit: `Idle → Thinking{since}`.
  On first chunk: `Thinking → Streaming`. On agent return:
  `Streaming → Idle`.
- **G3. Slash commands.** Lines starting with `/` go through
  `SlashRegistry::dispatch` exactly as the rustyline REPL does
  today; output becomes a `System` transcript item. `/exit`
  routes through the same teardown.
- **G4. Cancel.** Ctrl+C during Streaming aborts the agent task,
  truncates `Memory` to the pre-turn `len()`, drops back to
  Idle. Same semantics as today.
- **G5. Slash + plugin reload outcome.** `SlashOutcome::Rebuild`
  is honored — slashes and tools are swapped in place mid-session.
- **Done when:** a full agent run streams into the transcript;
  Ctrl+C cancels mid-stream and the conversation rolls back;
  `/clear`, `/help`, `/cost`, `/sessions`, `/plugins reload`
  all work.

### Phase H — Tool-call cards (~1d)

Render `PreToolUse` / `PostToolUse` as inline cards.

- **H1. Hook bridge.** A new `TuiHook` registered on
  `agent.hooks` translates payloads into `UiEvent::ToolStart{id, tool, args_preview}`
  and `UiEvent::ToolDone{id, duration, summary, ok}`.
- **H2. Card widget.** Two-line card:
  ```
  → Read   src/main.rs                          0.04s ✓
    37 lines
  ```
  State while running shows a small spinner before the timer
  resolves.
- **H3. Cards interleave with assistant content.** A turn that
  streams "looking at this file..." then dispatches Read then
  continues "...so we should change line 42" renders three
  transcript items in order.
- **H4. Result summary heuristics.** Per-tool summary lines:
  Read → "N lines"; Bash → "exit 0" or "exit 3"; Edit/Write →
  "± N lines"; Grep → "N matches in M files"; subagent → "X tool
  rounds, K tokens".
- **Done when:** a multi-tool turn renders a stack of cards each
  with timing and summary; cards animate from running → done.

### Phase I — Approval modal (~0.5d)

- **I1. Modal layer.** When `Decision::Ask` fires, the agent task
  awaits an `oneshot::Receiver<bool>` while the UI pops a modal
  overlaid on the transcript.
- **I2. Single-key.** `y`/`n`/`a`/`d`/ESC. `a` adds the (tool, args)
  pattern to a session-allow set so subsequent matches auto-approve.
- **I3. Diff preview inside the modal.** Reuse `policy::render_unified_diff`
  output but inside a ratatui `Paragraph` with `Wrap { trim: false }`
  and ANSI passthrough.
- **I4. Long-diff scroll.** PgUp/PgDn scrolls the modal body when
  the diff exceeds the modal height.
- **Done when:** an Edit triggers a modal with a colored diff;
  pressing `a` then a second similar Edit doesn't re-prompt.

### Phase J — Markdown + syntax highlighting (~2d)

- **J1. Markdown parser → ratatui Lines.** Use `pulldown-cmark` to
  walk the assistant body. Map: bold → `Modifier::BOLD`, italic
  → ITALIC, headers → bold + colored gutter `▌ `, lists → `• `
  / `  - ` indents, links → underlined text + `(url)` in dim,
  inline code → reverse-video.
- **J2. Code fences via syntect.** Lazy-load the bundled
  `SyntaxSet` + `ThemeSet` on first fence; cache subsequent
  highlights. Fall back to dim mono for unknown languages.
- **J3. Theme detection.** Default to dark; honor `$COLORFGBG`
  (which most terminals export) to pick a light theme when the
  background is light.
- **J4. Streaming-safe rendering.** Re-parse the assistant body
  on each chunk *or* parse incrementally — incremental is faster
  but harder. Start with re-parse + memoize on body length; if
  it shows up in profiles, switch.
- **Done when:** an assistant message containing `**bold**`, `# A heading`,
  ``` ```rust fn main(){} ``` ``` renders bold, gutter-marked,
  and syntax-highlighted respectively. Theme matches dark/light
  terminal.

### Phase K — Input ergonomics (~1d)

- **K1. Multi-line via tui-textarea.** Shift+Enter inserts a
  newline, Enter submits. Pasted multi-line content stays as one
  turn.
- **K2. Slash autocomplete.** Tab on `/m<TAB>` shows a popup with
  matching slashes; arrow keys + Enter to pick. Powered by the
  live `SlashRegistry`.
- **K3. File path autocomplete with `@`.** `@src/m<TAB>` expands
  to `@src/main.rs`. Glob-based.
- **K4. History.** Up/Down through past prompts; Ctrl-R opens a
  search-as-you-type overlay.
- **K5. Persistent history.** `~/.config/oli/history.jsonl`,
  read on startup, appended on each submit.
- **Done when:** completion popups feel like fish/zsh; Ctrl-R
  works; multi-line paste lands as one prompt.

### Phase L — Scrollback & navigation (~1d)

- **L1. Scrollable transcript.** PgUp/PgDn + arrow keys when input
  is empty; mouse wheel always.
- **L2. Stick to bottom.** When scrolled to bottom and a new chunk
  arrives, auto-scroll. When the user scrolls up, freeze; show a
  "↓ N new" indicator that becomes a hint to press End.
- **L3. Top/bottom shortcuts.** `g`/`G` (vim style) and Home/End.
- **L4. `/copy N`.** Copy the N-th-most-recent assistant message
  to the clipboard via OSC52. Falls back to a hint if the
  terminal doesn't support OSC52.
- **Done when:** scrolling through a 200-message session is
  smooth; new content arriving while scrolled up doesn't yank
  the view; "↓ 12 new" indicator shows up correctly.

### Phase M — Status bar polish (~0.5d)

- **M1. Always-on status line.**
  ```
  ▌ session 17144·3000  •  claude-opus-4.7  •  4.2k / 200k  •  main *  •  $0.04
  ```
- **M2. Token gauge color.** Green < 60%, amber 60–85%, red > 85%
  of the model's context window.
- **M3. Mode indicator.** Subtle right-side: ⠋ thinking 3.1s /
  ▶ streaming / ⏸ awaiting approval / · idle.
- **M4. Width-aware collapse.** On narrow terminals (< 80 cols),
  drop fields right-to-left in priority order: cost > branch >
  session id > token count > model.
- **Done when:** status reflects reality; doesn't flicker;
  collapses gracefully at 60 cols.

### Phase N — Discoverability overlays (~1d)

- **N1. Interactive `/sessions`.** Replace the text listing with
  a fuzzy-filterable picker. Type to filter, arrows to navigate,
  Enter to resume.
- **N2. Interactive `/help`.** Browse commands by category, see
  full description + example invocations on the right pane.
- **N3. `/<cmd> ?`** popup-style inline help.
- **N4. First-run wizard.** When no `~/.config/oli/config.toml`
  exists at TUI startup, run a 4-step wizard: provider, API key
  (masked input), default model, save.
- **N5. Onboarding hints.** First time the user sees an approval
  prompt: a faint "Press `a` to allow this for the rest of the
  session" footnote. Disappears after the first `a`.
- **Done when:** a fresh user goes from `oli` to a working
  session entirely through TUI overlays.

### Phase O — Recoverability (~0.5d)

- **O1. `/undo`.** Drops the last user turn (and assistant
  response, and any tool round-trips) from `Memory` and the
  transcript. Wraps `Memory::truncate`.
- **O2. Ctrl+E edit-last.** Reopens the last user prompt in the
  input box for editing; submitting re-runs (after truncating
  to before that prompt).
- **O3. Real Bash kill.** Currently Ctrl+C drops the agent
  future; the spawned shell child keeps running until stdio
  breaks. Expose a kill handle from the Bash tool, integrate
  with the cancel path.
- **Done when:** `Bash(command="sleep 60")` Ctrl+C'd at second 2
  has the child gone within ~1s.

## Suggested ordering & milestones

- **Week 1 ship:** F1–F6 + G1–G5 + H1–H4. The harness has a
  proper TUI: status bar + scrollable transcript + tool-card
  stack + input area, end-to-end working with the real agent.
  No markdown, no completion yet — but the shell looks right.
- **Week 2 ship:** I1–I4 + K1–K5 + L1–L4. Approvals as modals,
  multi-line + completion + history input, scroll & stick-to-
  bottom. The harness *feels* right.
- **Week 3 ship:** J1–J4 + M1–M4 + N1–N5 + O1–O3. Markdown +
  syntax highlight + status polish + discoverability overlays
  + recoverability. The harness is *done*.

## Acceptance for "ideal DX"

A user can:

- Launch `oli`, see a polished TUI with status bar, transcript,
  input area.
- Type a 30-line code block (multi-line input) and submit.
- Watch markdown render: `**bold**` is bold, ```rust ... ``` is
  syntax-highlighted, `# headings` have a gutter.
- See tool calls appear as cards inline:
  `→ Read src/main.rs · 37 lines · 0.04s ✓` updating in place.
- Approve `Edit` with one keystroke `y`; press `a` to skip the
  rest of this session's similar Edits.
- Look at the bottom of the screen and know which model,
  session, token budget (color-graded), git branch they're on.
- Tab-complete `/sess<TAB>` to `/sessions` and pick from a fuzzy
  picker.
- Tab-complete `@src/m<TAB>` to `@src/main.rs`.
- Ctrl+R to search history; Ctrl+E to edit-and-rerun.
- PgUp to scroll back; mouse-wheel works; "↓ 12 new" appears
  when streaming continues out of view; press End to catch up.
- Run `/undo` and have the last turn vanish.
- Ctrl+C a runaway `Bash` and see the child actually killed.
- Pipe to a script: `oli --plain` falls back to the line REPL;
  `oli -p` stays one-shot.

## Open decisions

- **`pulldown-cmark` vs `comrak`.** comrak is more featureful
  (footnotes, tables, strikethrough) but bigger. Start with
  pulldown-cmark; revisit if assistant output uses GFM features
  we drop.
- **`tui-textarea` vs hand-rolled input.** tui-textarea is mature
  and saves work. The fact that vim-mode falls out for free is a
  small bonus.
- **Streaming markdown re-parse vs incremental.** Re-parse is
  simpler. With pulldown-cmark on a few KB of body, the cost is
  microseconds. Profile if anyone notices.
- **Theme detection.** `$COLORFGBG` is what most terminals
  expose; we use that. Allow `[ui].theme = "dark"|"light"|"auto"`
  override in config later.
- **Keybind layout.** Default emacs-y. Document a `[ui].vim_mode`
  config flag; turn on after Phase K.
- **OSC52 clipboard.** Works in iTerm2, kitty, WezTerm,
  Alacritty (with config), tmux (with `set -g set-clipboard on`).
  Falls back to a hint pointing the user at their terminal's
  copy bindings.
- **Mouse selection / copy.** Alternate-screen mode breaks
  terminal-native mouse selection in many terminals. Document the
  `--plain` workaround and `/copy N` shortcut.
- **Inline ratatui later.** If the alternate-screen trade-off
  hurts more than expected, revisit `Viewport::Inline` as a
  Phase P. Not blocking the initial ship.

## Rolling status

Mirror commit SHAs into `specs/progress.md` at each phase
boundary. `specs/ui.md` is preserved as the historical
line-oriented plan; this doc supersedes it.

| ID | Item                                              | Status |
| -- | ------------------------------------------------- | ------ |
| F1 | TUI module skeleton (`src/tui/`)                  | DONE   |
| F2 | Three-pane layout (status / transcript / input)   | DONE   |
| F3 | Crossterm + alternate-screen lifecycle            | DONE   |
| F4 | Echo loop (input → transcript)                    | DONE   |
| F5 | Quit / cancel keybindings                         | DONE   |
| F6 | `oli --plain` fallback to line REPL               | DONE   |
| G1 | Streaming sink → UI events                        | DONE   |
| G2 | Idle / Thinking / Streaming mode transitions     | DONE   |
| G3 | Slash command dispatch                            | DONE   |
| G4 | Cancel mid-stream                                 | DONE   |
| G5 | `SlashOutcome::Rebuild` plumbed through           | DONE   |
| H1 | Hook bridge: PreToolUse / PostToolUse → UiEvent   | DONE   |
| H2 | Tool-card widget with spinner + timing            | DONE   |
| H3 | Cards interleave with assistant content           | DONE   |
| H4 | Per-tool result summaries                         | DONE   |
| I1 | Approval modal layer                              | DONE   |
| I2 | Single-key approval (y/n/a/d/ESC)                 | DONE   |
| I3 | Diff preview inside modal                         | DONE   |
| I4 | Modal scroll for long diffs                       | DONE   |
| J1 | Markdown parser → ratatui Lines                   | DONE   |
| J2 | Syntect-highlighted code fences                   | DONE   |
| J3 | Light/dark theme detection                        | DONE   |
| J4 | Streaming-safe markdown re-parse                  | DONE   |
| K1 | Multi-line input via tui-textarea                 | DONE   |
| K2 | Slash autocomplete popup                          | DONE   |
| K3 | `@path` file autocomplete                         | DONE   |
| K4 | Up/Down + Ctrl-R history                          | DONE   |
| K5 | Persistent history file                           | DONE   |
| L1 | Scrollable transcript (Pg/wheel)                  | DONE   |
| L2 | Stick-to-bottom + "↓ N new" hint                  | DONE   |
| L3 | Ctrl+Home / Ctrl+End shortcuts (g/G dropped)      | DONE   |
| L4 | `/copy N` via OSC52                               | DONE   |
| M1 | Always-on status line                             | DONE   |
| M2 | Token-gauge color thresholds                      | DONE   |
| M3 | Mode indicator (spinner / arrow / pause / dot)    | DONE   |
| M4 | Width-aware status collapse                       | DONE   |
| N1 | Interactive `/sessions` picker                    | DONE   |
| N2 | Interactive `/help` browser                       | DONE   |
| N3 | `/<cmd> ?` inline help popup                      | DONE   |
| N4 | First-run setup wizard                            | PART   |
| N5 | Fading onboarding hints                           | DONE   |
| O1 | `/undo` rolls back last turn                      | DONE   |
| O2 | Ctrl+E edit-and-rerun                             | DONE   |
| O3 | Real Bash child SIGKILL on cancel                 | DONE   |
