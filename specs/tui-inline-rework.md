# Inline viewport rework — scrollback-backed transcript

Status: **in progress**

## Problem

In buffer-terminals (`neovim:terminal`, `vscode`, `emacs:term`,
`jetbrains` — see `caps.rs:122-125`), `auto_viewport()` resolves to
`ViewportMode::Inline` (`caps.rs:202-208`). Inline reserves a fixed
block of rows via ratatui's `Viewport::Inline` (`terminal.rs:160-169`).

The renderer re-derives the **entire transcript every frame** and
paints it inside the viewport (`ui/mod.rs:9-11, 28-47`). That model
assumes oli owns the screen — true under fullscreen/alt-screen (the
default), false inline. When a turn starts streaming, new content
arrives and the host scrolls the inline block; ratatui's cached
`viewport_area` desyncs from where the block actually sits, it
repaints at the shifted rows, and the **previous frame's top rows
orphan into the scrollback above the live block** and "float". The
two fragments seen in the wild — the tail of a `Read(…)` transcript
line and the *idle* activity hint `[/]: turns · …` (which can only
render in `Mode::Idle`, proving it's a stale frame) — are exactly
this.

ratatui's per-frame blank-buffer + diff cleans stale cells in
fullscreen; it cannot reach rows that fall outside the viewport it
tracks. `specs/tui.md:63-65` already flagged inline as lower-fidelity
("a status bar doesn't really work in inline mode") — but inline gets
auto-selected anyway.

## Approach — adopt ratatui's intended inline idiom

Stop rendering the whole transcript in the viewport. Instead:

- **Finalized transcript items flush to native scrollback** via
  `Terminal::insert_before`, which scrolls the host *through ratatui*
  so `viewport_area` stays in sync — no orphaning possible.
- **The viewport renders only the live tail** (the in-flight turn) +
  status + activity strip + input.

### Commit watermark

`App` gains `committed: usize` — the count of leading transcript
items already flushed to scrollback. Items are never removed from the
`transcript` Vec (other state indexes into it by absolute position:
`active_assistant`, `active_tools`, `focused_card_idx`), so `committed`
is a watermark, not a drain.

`committable_count(items, committed)` walks the contiguous prefix of
**final** items from `committed` and stops at the first live one:

- `UserPrompt`, `System` → always final.
- `Assistant { done }` → final iff `done`.
- `ToolCard` → final iff `Done` (Streaming/Running are live).

Because commit only ever advances over a contiguous final prefix,
ordering into scrollback is preserved and long turns flush
progressively as their sub-segments finalize (keeping the live tail
small).

### Why fullscreen is untouched

`committed` stays `0` in fullscreen (the commit step never runs there;
`insert_before` is a no-op outside `Viewport::Inline` regardless). The
viewport builder renders `[committed, len)`, which is `[0, len)` in
fullscreen — byte-for-byte identical to today. The only fullscreen-
visible change is that `build_transcript_lines` now delegates to a
range-parametrized variant.

### Height for `insert_before`

`insert_before(height, draw_fn)` needs an exact row count up front.
We use `Paragraph::line_count(width)` (ratatui
`unstable-rendered-line-info` feature) on the same lines/wrap we then
render, so the scrollback buffer is sized exactly — no trailing blank
gaps, no clipping. Committed lines render at full width (no block
padding) so `line_count(width)` matches the render wrap width exactly.

## Trade-offs (accepted)

Once content is in native scrollback, oli's custom controls over
*history* no longer apply in inline mode — the host terminal (and, in
`:terminal`, Neovim/Emacs) owns scrollback and search instead:

- **Scroll** (PgUp/PgDn, wheel, `g`/`G`) and **turn-jump** (`[`/`]`)
  act only on the live tail. Historical scrollback is the host's.
- **Search** (Ctrl+F) covers only the live tail.
- **Tool-card expand** (`{`/`}`, Enter) works only on the live turn's
  cards; once a card flushes it's static text in scrollback.
- **Undo** (Ctrl+E / Ctrl+U) can only roll back the *uncommitted* tail;
  a flushed turn can't be unprinted (its scrollback stays, though
  agent memory is still truncated). `undo_last_user_turn` clamps
  `committed` to the new length to stay sound.

These are inherent to scrollback-backed inline and match the spec's
long-standing "inline is lower-fidelity" stance. Fullscreen keeps
every feature.

## Out of scope (follow-ups)

- Shrinking the reserved inline height when idle (it still reserves
  `inline_height` rows; mostly blank at rest). Pre-existing; not a
  regression. Tracked separately.
- Re-pinning the viewport on resize (`UiEvent::Resize` stays a no-op;
  ratatui autoresize handles it).

## Test plan

- **Unit (pure, no terminal):** `committable_count` over mixed item
  states — empty, all-final, final-prefix-then-live, live-first,
  re-entrancy (calling twice doesn't double-count), undo clamp.
- **Unit:** `build_transcript_lines_range` renders only the requested
  range; `committed=0` equals the legacy full render.
- **Manual (the only place the bug reproduces):** run oli in Neovim
  `:terminal` / Emacs term, submit a prompt that triggers tool calls,
  confirm no floating leftover above the input across the turn and at
  idle; confirm finished turns land in native scrollback.

## Status

| Step | State |
|---|---|
| `committable_count` + clamp + tests | ✅ |
| `App.committed` field | ✅ |
| range-parametrized transcript builder | ✅ |
| inline commit step in run loop + feature flag | ✅ |
| manual verification in `:terminal` | ⛔ needs real buffer-terminal |
