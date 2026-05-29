# TUI Polish & Buffer-Terminal Compatibility

Phases F–O shipped the ratatui TUI; phases V1–V3 (`specs/tui-readability.md`)
tightened activity signaling and visual rhythm. This doc is the next layer:
**make the TUI feel polished to navigate and behave correctly when oli is
running inside another app's terminal buffer** (Neovim `:terminal`, VSCode
integrated terminal, Helix, Zellij, Emacs `term`, etc.).

User direction: **stay on ratatui.** Don't swap frameworks. Borrow components
from the ratatui ecosystem (`nucleo`, `ratatui-image`, the textarea we already
use) and write the small parts that don't yet have a good crate.

## Mission

1. **Run cleanly inside buffer terminals.** A Neovim user who runs `:terminal
   oli` in a buffer should get a TUI that doesn't clobber the surrounding
   buffer, doesn't capture mouse the host wants, doesn't hang on terminal
   queries the host won't answer, and degrades gracefully when graphics /
   keyboard / clipboard protocols are missing.
2. **Feel as navigable as Claude Code / pi.** Fuzzy completion everywhere,
   in-transcript search, jump-by-turn, cancel hints, inline streaming diffs,
   a real theme module.
3. **No regressions for the primary path.** A fresh terminal user
   (iTerm2 / Alacritty / kitty / WezTerm) sees a slightly nicer TUI and
   loses nothing.

## Current state (after Phase T + V)

- Full alternate-screen ratatui (`src/tui/mod.rs`, `Terminal::with_options`
  with `Viewport::Fullscreen`).
- Mouse capture on, focus events on, bracketed paste handled.
- `/copy N` uses OSC52 (no fallback for terminals that don't honor it).
- Completion uses prefix match for slashes (`src/tui/completion.rs`) and
  glob for `@paths`. No fuzzy ranking.
- Colors are scattered through `src/tui/ui/*.rs` and `src/tui/markdown.rs`.
  Light/dark detection via `$COLORFGBG`. No named theme module.
- Images are not rendered. `Read` on a binary returns "binary file" text;
  the model never sees an image, the user never sees one inline.
- No in-transcript search; no turn-jump navigation.
- Edits surface as a diff *only inside the approval modal*. While the
  model is writing, the user sees no diff preview.

## Goals

1. **Inline viewport mode** for buffer terminals: oli renders in a fixed
   region of the existing buffer, doesn't take alt-screen, and the host
   keeps its scroll model.
2. **Capability detection and graceful degradation** — every feature
   (mouse, OSC52, kitty keyboard, truecolor, Kitty/Sixel/iTerm2 graphics)
   is detected at startup and the UI adapts without erroring.
3. **Fuzzy everything** — slash commands, `@path`, `/model`, `/sessions`
   picker, history search.
4. **In-transcript search and turn navigation.** Press `/` to find,
   `[`/`]` to jump between user turns, `Ctrl+O` to jump back to where
   the cursor was before a scroll.
5. **Inline streaming diff** for `Edit` and `Write` while the model is
   writing — not just at approval time.
6. **First-class theme module.** Centralize colors; ship 3 named themes;
   `[ui].theme = "..."` in config.
7. **Optional inline image rendering** via `ratatui-image` for image
   inputs and image-returning tools.

## Non-goals

- Switching off ratatui (cursive, tui-realm, iced, etc. — out).
- A full theme DSL or runtime theme reload (config-time only).
- Web/WASM rendering (`ratzilla`-style) — different product.
- Hybrid TUI+web like Warp. Not the bet.
- Mouse-driven UI beyond wheel scroll (already a non-goal in `tui.md`).
- IDE-like multi-pane layouts.

## Approach

Three layered phase blocks, ordered by leverage.

- **Phase W** — buffer-terminal compatibility. Foundational because it
  changes the rendering model (`Viewport::Inline` becomes available
  alongside `Viewport::Fullscreen`) and gates several polish items.
- **Phase X** — navigation & input ergonomics. Fuzzy completion, search,
  turn-jump. Pure additive on top of W.
- **Phase Y** — visual polish. Theme module, inline diff streaming,
  optional image rendering. Smallest blast radius; ships last so it can
  consume the theme module the rest of the codebase migrates to.

---

## Phase W — Buffer-terminal compatibility (2–3 d)

The hostile environment is **`:terminal` inside Neovim** — a libvterm-backed
terminal embedded in an editor buffer. The user expects to scroll the
buffer up to see earlier content, copy with the editor's clipboard, and
not have oli's alternate-screen lifecycle fight the editor. Most other
buffer terminals (VSCode, Helix, Zellij, Emacs term) hit a subset of the
same issues.

### W1 — Inline viewport mode

**Problem:** Alt-screen takes over the whole terminal. Inside `:terminal`
that means the editor buffer's normal scroll is useless — every redraw
clobbers the visible area, and exiting flashes the screen.

**Solution:** Add `Viewport::Inline { height }` as a second rendering
mode. Oli renders a fixed-height block in the host's current buffer; on
exit, the block stays as scrollback. No alt-screen, no mouse capture by
default, no terminal state to restore beyond cursor visibility.

#### Files
- `src/tui/mod.rs` — extend the `Terminal::with_options` call site to
  pick `Viewport::Inline { height: rows }` when inline mode is active.
  Default height: `min(terminal_rows - 2, 32)`. Recomputed on `Resize`.
- `src/tui/mod.rs` — gate mouse capture, focus events, and bracketed
  paste behind the runtime mode (inline → off by default, fullscreen →
  on as today).
- `src/tui/driver.rs` — no Drop-time `LeaveAlternateScreen` in inline
  mode; just `disable_raw_mode` and a final cursor-show.
- `src/bin/oli.rs` — new flag `--inline` (and inverse `--fullscreen`)
  for explicit selection. Both override auto-detection.
- `src/config.rs` — new `[ui].viewport = "auto" | "fullscreen" | "inline"`.
  Default `"auto"` (see W2). Flag wins over config.

#### Acceptance
- `oli --inline` inside a normal terminal renders a fixed block at the
  bottom of the buffer, doesn't enter alt-screen, and on `/exit` leaves
  the block in scrollback.
- `oli --fullscreen` inside `:terminal` still works (user opted in).
- Resize event recalculates inline height; transcript and input redraw
  correctly without going taller than the host terminal.

#### Done when
- A user inside Neovim `:terminal` runs `oli --inline`, has a working
  session, exits, and can `:scroll` the buffer to see the transcript as
  normal terminal output — no missing rows, no torn rendering.

### W2 — Capability detection + auto-mode

**Problem:** The user shouldn't have to know whether they're in a buffer
terminal. Wrong default → bad first impression.

**Solution:** A `Capabilities` struct probed once at startup. Feeds the
viewport default and the feature toggles in W4–W6.

#### Files
- New `src/tui/caps.rs` — `Capabilities { is_buffer_terminal: bool,
  truecolor: bool, kitty_keyboard: bool, osc52: bool, graphics:
  GraphicsKind, mouse: bool, focus_events: bool }`.
- Detection logic, in order of cheapness:
  - Env vars: `$NVIM` and `$NVIM_LISTEN_ADDRESS` → Neovim. `$VSCODE_*`,
    `$TERM_PROGRAM=vscode` → VSCode. `$ZELLIJ` → Zellij.
    `$INSIDE_EMACS` → Emacs term. Combined ⇒ `is_buffer_terminal=true`.
  - `$COLORTERM=truecolor|24bit` → truecolor.
  - `$TERM` prefix → known terminal family (xterm-kitty, alacritty,
    wezterm, foot, ghostty); used to seed graphics + clipboard.
  - Optional DA1/DA2 query (with a 100ms timeout) only when inside a
    known-good terminal family. Skip queries inside buffer terminals —
    those often don't respond and Neovim has been bitten by this.
- `src/tui/mod.rs` — auto-mode picks `Inline` when `is_buffer_terminal`
  is true. Otherwise `Fullscreen`.

#### Acceptance
- Inside `:terminal` (with `$NVIM` set), `oli` (no flag) starts in inline
  mode, mouse off, no DA queries sent.
- Inside iTerm2 with `$TERM_PROGRAM=iTerm.app`, `oli` starts in fullscreen
  mode, mouse on, kitty-keyboard detection attempted.

#### Done when
- Capability detection logged at startup via `log_info!` so
  `/diagnostics` shows the resolved mode + flags.
- No detection step blocks startup for more than 150 ms total.

### W3 — Mouse capture as opt-in

**Problem:** Mouse capture inside a buffer terminal steals scroll-wheel
events from the host (Neovim's `mouse=` setting expects them).

**Solution:** Mouse capture off by default in inline mode; opt-in via
`/mouse on` or `[ui].mouse = true`. Always off when
`is_buffer_terminal=true` unless the user explicitly turns it on.

#### Files
- `src/tui/mod.rs` — startup mouse capture gated on
  `caps.mouse && config.ui.mouse_enabled`.
- `src/repl/slash.rs` — new `/mouse [on|off]` slash; `/mouse` with no arg
  shows current state.
- `src/tui/app/mod.rs` — a `mouse_enabled: bool` runtime flag; toggling
  it issues `EnableMouseCapture` / `DisableMouseCapture` against the
  active backend.

#### Done when
- `oli --inline` doesn't capture the mouse; scrolling the host buffer
  works as the host expects.
- `/mouse on` enables wheel-driven transcript scroll within oli; `/mouse
  off` returns control to the host.

### W4 — OSC52 clipboard with fallback

**Problem:** `/copy N` writes OSC52 unconditionally. Buffer terminals
often ignore it; the user gets no feedback and no clipboard.

**Solution:** Detect OSC52 support via env (`$TERM_PROGRAM`, `$TERM`
allowlist) + an optional manual override `[ui].osc52 = "on" | "off" |
"auto"`. When unsupported, fall back to printing the content into a
modal labeled "Copy below (your terminal blocked OSC52)" with a
single-keystroke close.

#### Files
- `src/tui/caps.rs` — `osc52: bool` set from terminal allowlist
  (kitty / wezterm / iTerm2 / alacritty-with-flag / tmux-with-flag).
  Default off inside Neovim `:terminal`.
- `src/repl/slash.rs` (or `src/tui/app/overlay.rs`) — `/copy N`: if
  `caps.osc52`, write OSC52; else open a `CopyFallback` overlay.

#### Done when
- `/copy 1` inside `:terminal` opens a modal containing the assistant's
  last message; Esc closes; user can select + yank with their editor's
  normal flow.
- `/copy 1` inside kitty writes to system clipboard as today; no modal.

### W5 — No-hang terminal queries

**Problem:** Some capability detection (cursor-position, DA1/DA2,
background-color OSC query) hangs forever in buffer terminals that
don't reply.

**Solution:** All terminal queries gated behind a `caps.query_ok` boolean
seeded from W2's env heuristics. When false, skip the query, use
defaults (truecolor=true if `$COLORTERM` says so, else 256).

#### Files
- `src/tui/caps.rs` — `query_ok: bool`.
- Anywhere a query exists today (mainly `$COLORFGBG` consumption and
  any future cursor-position probes) — wrap in a 100 ms timeout and a
  `query_ok` gate.

#### Done when
- Startup time inside Neovim `:terminal` is within 50 ms of startup
  outside it; no probes hang.

### W6 — Kitty keyboard protocol opt-in

**Problem:** `Ctrl+Shift+R` and similar high-fidelity chords need the
kitty keyboard protocol enabled. Enabling it inside a buffer terminal
that doesn't support it produces escape-sequence garbage in the input.

**Solution:** Enable only when `caps.kitty_keyboard` is true (kitty,
WezTerm with config, foot, ghostty). Document the supported keybind
matrix for the basic case.

#### Files
- `src/tui/caps.rs` — `kitty_keyboard: bool` from `$TERM` allowlist.
- `src/tui/mod.rs` — `PushKeyboardEnhancementFlags` only when enabled.
- `docs/cheatsheet.md` — keybind table marks "kitty-keyboard required"
  entries (e.g., `Ctrl+Shift+Enter`).

#### Done when
- Inside Neovim `:terminal` no `\e[27u`-style escapes leak into the
  input box.

### W7 — Host-buffer scrollback behavior in inline mode

**Problem:** In inline mode the user scrolls oli's transcript with
PgUp/End/wheel inside oli — but they may also want the host buffer's
scrollback. The two scroll surfaces collide.

**Solution:** Inline mode constrains its own scroll to the inline
viewport. PgUp/PgDn inside oli scroll oli's transcript. Mouse wheel:
when mouse is off (W3 default), wheel goes to the host buffer; when
on, wheel is captured by oli. Document this clearly.

#### Files
- `docs/cheatsheet.md` — inline-mode section explaining the split.
- `src/tui/event.rs` — no code change beyond the mouse gate from W3;
  this phase is mostly behavioral documentation.

#### Done when
- A Neovim user can scroll the editor buffer up to see earlier oli
  output without disturbing oli's input box focus.

---

## Phase X — Navigation & input ergonomics (2 d)

### X1 — Fuzzy completion via `nucleo`

**Problem:** Slash completion is prefix-only (`src/tui/completion.rs`),
`@paths` is glob-based, `/model` and `/provider` pickers are exact-match
inside their overlays. Users have learned to expect fuzzy ranking from
fzf/zoxide/Helix.

**Solution:** Adopt `nucleo` (the matcher behind Helix and Zed) as the
single ranking engine for: slash autocomplete, `@path` completion,
`/sessions` picker, `/model` picker, `/provider` picker, Ctrl-R history
search.

#### Files
- `Cargo.toml` — add `nucleo = "0.5"`.
- New `src/tui/fuzzy.rs` — thin wrapper exposing `rank<T>(query:
  &str, items: &[T], key: impl Fn(&T) -> &str) -> Vec<(usize, u16)>`
  (item index + score). Lazy-built `Matcher` cached on `App`.
- `src/tui/completion.rs` — swap prefix logic for `fuzzy::rank`.
  Preserve the "exact-prefix wins" tie-break (so `/help` matches
  `/help` before `/help-debug`).
- `src/tui/ui/overlays.rs` — `/sessions`, `/model`, `/provider`,
  `Ctrl-R` overlays all consume `fuzzy::rank`. Highlight the matched
  characters in the rendered row (nucleo returns match positions).

#### Acceptance
```rust
#[test]
fn fuzzy_completion_matches_subsequence() {
    let items = vec!["/help", "/sessions", "/model"];
    let ranked = fuzzy::rank("sn", &items, |s| s);
    assert_eq!(items[ranked[0].0], "/sessions");
}

#[test]
fn fuzzy_completion_prefers_exact_prefix() {
    let items = vec!["/help", "/help-debug"];
    let ranked = fuzzy::rank("help", &items, |s| s);
    assert_eq!(items[ranked[0].0], "/help");
}
```

#### Done when
- Typing `/ses` in the input ranks `/sessions` first; typing `ssn`
  still matches it.
- Ctrl-R `gtdf` matches `git diff` from history.
- Match positions render with a brighter color in completion popups.

### X2 — In-transcript search

**Problem:** No way to find earlier text without scrolling manually.

**Solution:** `/` inside oli opens a one-line search bar at the top of
the transcript pane; type to filter; Enter jumps to the first match
and highlights all matches; `n`/`N` cycle; Esc closes and clears
highlights.

#### Files
- New `src/tui/app/search.rs` — search state (query, match positions,
  current index).
- `src/tui/app/mod.rs` — new `Overlay::Search` variant; key router
  dispatches input.
- `src/tui/ui/transcript.rs` — when search is active, walk transcript
  lines and apply a highlight `Style` to matched ranges.
- `src/tui/ui/overlays.rs` — render the search input bar.

#### Done when
- A 100-turn session: `/` then type `panic`, press Enter, transcript
  scrolls to first occurrence; `n` jumps to next; `Esc` clears.

### X3 — Turn-jump navigation

**Problem:** Long sessions are hard to navigate. PgUp moves by a
screen, not by semantic unit.

**Solution:** When the input box is empty:
- `[` jumps to the start of the *previous user turn*.
- `]` jumps to the start of the *next user turn*.
- `Ctrl+O` jumps to the last position before a scroll.
- `Ctrl+I` (companion) jumps forward in the position stack.

When the input box has text, these keys insert as normal.

#### Files
- `src/tui/app/transcript.rs` — `turn_offsets()` returns the row index
  of each `UserPrompt` item, computed from the laid-out transcript.
- `src/tui/event.rs` / router — `[`/`]`/`Ctrl+O`/`Ctrl+I` handlers
  guarded on `input.is_empty()`.

#### Done when
- Empty input + `[` scrolls the transcript so the previous user prompt
  is at the top of the visible region.
- A position stack of depth ≥ 8 supports `Ctrl+O` / `Ctrl+I` round-trip.

### X4 — Always-visible keybinding hints

**Problem:** The hint band (`src/tui/hints.rs`) is good but the user
has to discover it. Some keybindings (like X3's `[`/`]`) are easy to
miss.

**Solution:** Extend the existing hint system so each overlay/mode
contributes a context-appropriate hint line, rendered in the dim
activity-strip area from Phase V1 (or one row above the input if V1
shipped without the strip).

#### Files
- `src/tui/hints.rs` — keyed hint set per `Mode` × `Overlay`. Hint
  lines are short: `[: prev turn · ]: next · /: search · Esc: cancel`.
- `src/tui/ui/mod.rs` — render the hint at low contrast on the same
  row that holds the cancel hint.

#### Done when
- Idle mode shows nav hints; Streaming mode shows `Esc to cancel ·
  Ctrl+C: hard cancel`; Search overlay shows `n/N: next/prev · Esc:
  close`.

---

## Phase Y — Visual polish (2 d)

### Y1 — Theme module

**Problem:** Colors live inline in `src/tui/ui/*.rs`,
`src/tui/markdown.rs`, `src/tui/ui/transcript.rs`. Adding a light theme
or honoring `[ui].theme = "..."` means touching all of them.

**Solution:** Extract a `Theme` struct with semantic fields. Three
named presets: `dark` (current default), `light` (currently auto-
detected via `$COLORFGBG`), `dimmed` (low-contrast for OLED / late
night). `[ui].theme = "dark" | "light" | "dimmed" | "auto"`.

#### Files
- New `src/tui/theme.rs`:
  ```rust
  pub struct Theme {
      pub bg: Color,
      pub fg: Color,
      pub dim: Color,
      pub accent: Color,        // status chip, headers
      pub user: Color,          // user prompt header
      pub assistant: Color,     // assistant header
      pub tool_running: Color,
      pub tool_ok: Color,
      pub tool_err: Color,
      pub diff_added: Color,
      pub diff_removed: Color,
      pub match_highlight: Color, // X2 search highlight
      pub gauge_ok: Color,
      pub gauge_warn: Color,
      pub gauge_danger: Color,
      pub border: Color,
  }
  pub fn load(name: &str) -> Theme { /* dark | light | dimmed | auto */ }
  ```
- Every place that constructs a `Style::default().fg(Color::...)`
  switches to `style().fg(theme.<field>)`. `App` holds an
  `Arc<Theme>`; render functions take `&Theme`.
- `src/tui/ui/mod.rs` — pass `&Theme` into `draw_status`,
  `draw_transcript`, `draw_activity`, every overlay.
- `src/config.rs` — `[ui].theme` deserialized into the theme name.

#### Done when
- `[ui].theme = "light"` produces a coherent light theme — no leftover
  hardcoded `Color::Cyan` flickering on white.
- A new color introduced anywhere requires *adding a `theme.<field>`*,
  not picking a literal. Reviewer can grep for `Color::Rgb`,
  `Color::Cyan`, etc. and find ~zero hits outside `theme.rs`.

### Y2 — Inline streaming diff for `Edit` / `Write`

**Problem:** Diffs are computed at `Decision::Ask` time and rendered
inside the approval modal. Before the modal opens, the user has no
visual sense of what the model is about to do — they see only
`→ Edit src/foo.rs   ⠋ 0.3s` on the activity strip.

**Solution:** While the model is *streaming a tool call* for `Edit` or
`Write`, render a compact streaming-diff card inline in the transcript.
The model's accumulated arguments are diffed-against-current on each
chunk (memoized on chunk length). On `PreToolUse`, the card transitions
to "awaiting approval" and the approval modal takes over the full
preview.

#### Files
- `src/tui/app/transcript.rs` — `TranscriptItem::ToolCard` gains a
  `streaming_preview: Option<DiffPreview>` field.
- `src/agent/mod.rs` (or wherever streaming-tool-args are surfaced) —
  emit a `UiEvent::ToolArgsChunk { id, partial_args }` event during
  streaming. The TUI hook bridge consumes it.
- `src/tui/ui/transcript.rs` — render a 6-line peek of the diff
  inside the running tool card; `Ctrl+P` (only when this card is
  current) expands to a full peek without leaving the transcript.
- Stale-edit semantics from Phase D still apply — preview shown but
  modal is the gate.

#### Done when
- The model says "I'll edit `src/foo.rs`" and starts streaming an
  `Edit` call. Within 200 ms a 6-line `± diff` peek appears under the
  tool card and grows as more chunks arrive.
- The approval modal still gates the actual write; preview is purely
  visual.

### Y3 — Inline image rendering (optional, behind a feature)

**Problem:** Models that emit images (vision-capable providers) and
tools that read image files have no way to surface visual content in
oli. The user sees `[Image: 1024x768 PNG]` text and that's it.

**Solution:** Add `ratatui-image` behind a `images` Cargo feature
(default off — adds an iconv/png stack we don't always want). When
enabled, image-typed tool results and assistant image attachments
render inline using the host's best available protocol (Kitty /
iTerm2 / Sixel / unicode-half-block fallback).

#### Files
- `Cargo.toml` — `ratatui-image = { version = "...", optional =
  true }`; `images = ["dep:ratatui-image"]`.
- `src/tui/caps.rs` — `graphics: GraphicsKind { Kitty, ITerm2,
  Sixel, HalfBlock, None }` from terminal family.
- `src/tui/ui/transcript.rs` — when an item carries an image and
  `caps.graphics != None && cfg!(feature = "images")`, render via
  `ratatui-image`; otherwise the `[Image: ...]` text fallback.
- Inside buffer terminals (`is_buffer_terminal=true`), force
  `HalfBlock` or `None` — never attempt Kitty/Sixel.

#### Done when
- `oli` built with `--features images` in kitty renders an image
  result inline at a sensible size.
- The same binary inside `:terminal` falls back to half-block or text
  without erroring.

### Y4 — Per-tool result expand-in-place

**Problem:** A tool card shows a one-line summary (`Bash · exit 0 · 12
lines`). To see the full output the user runs `/show` or scrolls into
the JSONL. Friction.

**Solution:** `Enter` on a focused tool card (X3's turn-jump
navigation lands the cursor on cards) expands it to show the full
captured output, capped at e.g. 40 lines with a "show more" continuation
to `/show <id>`.

#### Files
- `src/tui/app/transcript.rs` — `TranscriptItem::ToolCard` gains
  `expanded: bool`.
- `src/tui/ui/transcript.rs` — render full output when expanded;
  collapse on second `Enter`.
- Key router — `Enter` on focused card toggles expand.

#### Done when
- A `Bash` card shows summary; focus + Enter expands to the captured
  output; Enter again collapses; nothing breaks if output > 40 lines
  (truncates with hint).

---

## Suggested ordering & milestones

- **Day 1–2 ship:** W1–W3 + W7. The inline mode works in `:terminal`
  with no mouse fights; capability detection is in place; a Neovim user
  can have a productive session.
- **Day 3 ship:** W4–W6. OSC52 fallback, no-hang query gating, kitty
  keyboard gated. Buffer-terminal story complete.
- **Day 4–5 ship:** X1 + X2 + X3 + X4. Fuzzy everything, search,
  turn-jump, hints. The TUI *feels* polished.
- **Day 6–7 ship:** Y1 + Y2. Theme module + inline diff streaming.
  The TUI *looks* polished.
- **Day 8 (optional):** Y3 + Y4. Image rendering and expand-in-place.

## Open decisions

- **Auto-mode preference inside tmux.** tmux is a passthrough; behave
  like the underlying terminal. Detect via `$TMUX` but defer to
  outer-terminal family for graphics/keyboard. Inline-mode-as-default
  inside tmux is probably wrong; keep fullscreen unless overridden.
- **Inline viewport height.** 32 rows is a guess. Possibly make
  configurable: `[ui].inline_height = N | "auto"`.
- **Streaming diff for non-Edit tools.** `Bash` could show a
  *streaming command preview*, but the value is lower (it's a
  one-line command). Skip for now; reconsider if the streaming
  infrastructure from Y2 has other consumers.
- **Theme names.** `dark` / `light` / `dimmed` is a reasonable seed.
  More can come from config; deliberately not building a theme
  marketplace.
- **`nucleo` adds ~120 KB to the binary.** Acceptable. If the cost
  becomes annoying, gate behind a `fuzzy` feature default-on.
- **`ratatui-image` adds an image-decode stack.** Worth ~1 MB to
  release. Default-off feature flag keeps the line-mode binary
  small.

## Acceptance for "buffer-terminal first-class"

A user inside Neovim's `:terminal` can:

- Run `oli` with no flags and have it auto-pick inline mode.
- Type into the input, see the activity strip update, scroll the
  *editor buffer* to see earlier turns without disturbing oli.
- `/copy 1` opens a fallback modal (OSC52 disabled in `:terminal`);
  yank with the editor's clipboard.
- Submit a prompt, watch tool cards stream, approve an `Edit` modal,
  and never see escape-sequence garbage or terminal-state desync.
- Exit `oli` and have the transcript remain visible in the buffer's
  scrollback.
- Switch back to `oli` later via `oli --continue` and have the same
  session restored.

## Acceptance for "polished navigation"

A user inside any modern terminal can:

- Type `/ses` and pick `/sessions` from a fuzzy popup.
- Inside `/sessions`, type `bug` to filter sessions whose first
  prompt mentions a bug.
- Type `gtdf` in Ctrl-R history search; the match positions are
  highlighted in the popup.
- Press `/`, type `panic`, hit Enter, jump to the first occurrence in
  the transcript; press `n`/`N` to cycle.
- With empty input, press `[` to jump to the previous user prompt;
  `Ctrl+O` to return.
- See an inline diff peek under a streaming `Edit` card before the
  approval modal opens.
- Edit `[ui].theme = "light"`, `/config reload`, and have the whole
  UI flip cleanly.

## Rolling status

| ID | Item                                              | Status |
| -- | ------------------------------------------------- | ------ |
| W1 | `Viewport::Inline` mode + `--inline`/`--fullscreen` | TODO |
| W2 | Capability detection (`src/tui/caps.rs`) + auto-mode | TODO |
| W3 | Mouse capture as opt-in (inline default off)      | TODO |
| W4 | OSC52 clipboard with copy-fallback modal          | TODO |
| W5 | No-hang terminal queries (100 ms gates)           | TODO |
| W6 | Kitty keyboard protocol opt-in                    | TODO |
| W7 | Inline-mode scroll vs host-buffer scrollback docs | TODO |
| X1 | Fuzzy completion via `nucleo`                     | TODO |
| X2 | In-transcript search (`/` + n/N)                  | TODO |
| X3 | Turn-jump navigation (`[`/`]` + Ctrl+O/I)         | TODO |
| X4 | Context-aware keybinding hints                    | TODO |
| Y1 | `src/tui/theme.rs` + named themes via config      | TODO |
| Y2 | Inline streaming diff for Edit / Write            | TODO |
| Y3 | Inline image rendering (`images` feature)         | TODO |
| Y4 | Per-tool card expand-in-place                     | TODO |

Mirror commit SHAs into `specs/progress.md` at each phase boundary.
