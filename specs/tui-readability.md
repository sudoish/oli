# TUI Readability — Activity Strip + Padding Pass

A focused UX pass on the TUI's "what is happening?" signal and on the
density of the transcript pane. Phases F–T shipped a working three-pane
shell (status / transcript / input) with markdown rendering, completion
popups, history search, and an animated mode indicator. This doc fixes
two pain points reported in real use:

1. **The activity indicator is in the wrong place.** It sits in the top-
   right of the status bar — far from where the user is typing and
   reading the latest assistant reply. When work is in flight users
   miss it and the harness feels frozen.
2. **The transcript feels cramped.** No horizontal margin, edge-to-edge
   text, no visual separation between turns.

## Current state (after Phase T)

Layout (`src/tui/ui/mod.rs:30-39`): three vertical bands.

- Row 0: status bar — `oli` chip on the left, identity fields (model,
  tokens, branch, session) flowing right, mode indicator pinned to the
  far right.
- Middle: transcript pane (flex), no horizontal margin.
- Bottom: input box (1–8 lines + 2-row border).

Mode indicator (`src/tui/ui/mod.rs:245-276`): one of four spans — `· idle`,
`⠋ thinking · 1.2s`, `▶ streaming`, `⏸ awaiting approval`. Spinner uses a
10-frame braille cycle (`src/tui/ui/mod.rs:280-284`); animation is driven
by the 60 fps redraw ceiling in `src/tui/mod.rs:160`, so no dedicated
tick events are needed.

Transcript items (`src/tui/ui/transcript.rs`):

- User: `▌ you` header (yellow bold), 2-space gutter for body, blank line
  between items.
- Assistant: `▌ oli` header (cyan bold), markdown-rendered body via
  `pulldown-cmark`, optional syntect syntax highlighting, blinking `▍`
  cursor while streaming.
- System: dark gray italic with 2-space indent, no header.
- Tool card: `→ tool args  spinner duration glyph` on a single line plus
  optional one-line summary detail.

**Definitive gaps** (verified by reading code):

1. **Activity is too far from the eye.** Users typing in the input box
   look at the bottom of the screen, not the top right. With the tool
   trace and the most recent assistant chunk also at the bottom, having
   the spinner up top means people perceive lag they aren't actually
   experiencing.

2. **Tool runs don't update `Mode`.** `on_tool_start`
   (`src/tui/app/transcript.rs:77`) appends a Running tool card but
   leaves `mode` whatever it was — typically `Streaming`. The indicator
   reads `▶ streaming` for the entire duration of a long `Bash` or `Grep`
   call, which is misleading. Same after `on_tool_done` until the next
   chunk arrives.

3. **No horizontal margin around content.** Transcript and status bar
   render edge-to-edge. On wide terminals lines visually butt against
   the terminal frame and feel cramped.

4. **No separation between turns.** A blank line between transcript items
   is the only divider. After 4–5 turns the screen reads as one
   undifferentiated wall.

## Non-goals

- Reskinning markdown rendering — the existing `markdown.rs` output is
  good. This pass leaves heading colors, code-fence styling, and inline
  code alone.
- Configurable themes / spacing knobs. Pick values, ship them, tune from
  feedback.
- Mouse interaction changes.
- Replacing tui-textarea or restructuring the input box.
- New overlays. Approval, sessions, help, history search and wizard
  stay where they are.

## Approach

Three independent phases. Phase V1 is the highest-impact change and
should land first. V2 is a one-line `Block::padding` swap plus a
separator helper. V3 is optional polish.

---

### Phase V1 — Activity strip above the input

**Problem:** The "is anything happening?" indicator sits in the top-right
of the status bar, far from where the user reads and types. Tool runs
also don't surface a distinct mode, so the indicator misreports state
during long tool calls.

**Solution:** Move the mode indicator out of the status bar into a
dedicated 1-row strip directly above the input box, with richer
sub-states and a visible cancel hint.

#### Files

- `src/tui/app/mod.rs` — extend `Mode` enum with a tool-running variant:
  ```rust
  pub enum Mode {
      Idle,
      Thinking { since: Instant },
      Streaming { since: Instant },
      ToolRunning { tool: String, since: Instant },
  }
  ```
  `Streaming` gains a `since` for elapsed-time display. (Today it has
  no clock.)
- `src/tui/app/transcript.rs` — flip mode in `on_tool_start` to
  `ToolRunning { tool, since: now }`; in `on_tool_done` flip back to
  `Thinking { since: now }` (the agent is processing tool output before
  the next stream resumes); on first content chunk after that,
  `on_content_chunk` upgrades to `Streaming { since: now }` as today.
- `src/tui/ui/mod.rs` — add a 4th constraint between transcript and
  input:
  ```rust
  Layout::vertical([
      Constraint::Length(1),               // status bar (identity only)
      Constraint::Min(3),                  // transcript
      Constraint::Length(1),               // activity strip
      Constraint::Length(input_height),    // input
  ])
  ```
  Drop the right-aligned mode indicator from `draw_status` — status bar
  becomes identity-only.
- `src/tui/ui/mod.rs` — new `draw_activity_strip(f, area, app)` that
  renders left-aligned mode + elapsed time, right-aligned `Esc to
  cancel` hint while busy. When idle the strip renders a dim em-dash
  (always reserved row keeps layout stable across state transitions).
- `src/tui/ui/mod.rs` tests — extend the existing render-mode-indicator
  tests for the new `ToolRunning` variant.

Strip content matrix:

| Mode                   | Left                              | Right          |
| ---------------------- | --------------------------------- | -------------- |
| Idle                   | dim `—`                           | (empty)        |
| Thinking (since)       | `⠋ thinking · 2.3s`               | `Esc to cancel`|
| Streaming (since)      | `▶ streaming · 4.1s`              | `Esc to cancel`|
| ToolRunning (tool, since) | `⚙ running grep · 0.8s`        | `Esc to cancel`|
| Awaiting approval      | `⏸ awaiting approval` (yellow bg) | (empty — modal handles input) |

The cancel hint reuses the existing `cancel_tx` plumbing
(`src/tui/app/mod.rs:131`); no new wiring needed.

#### Acceptance

```rust
#[test]
fn tool_start_flips_mode_to_tool_running() {
    let mut app = App::new();
    app.on_turn_started();
    app.on_content_chunk("partial");
    app.on_tool_start(1, "grep".into(), "pattern=foo".into());
    assert!(matches!(app.mode, Mode::ToolRunning { ref tool, .. } if tool == "grep"));
}

#[test]
fn tool_done_returns_to_thinking() {
    let mut app = App::new();
    app.on_turn_started();
    app.on_tool_start(1, "grep".into(), "".into());
    app.on_tool_done(1, Duration::from_millis(500), "ok".into(), true);
    assert!(matches!(app.mode, Mode::Thinking { .. }));
}

#[test]
fn activity_strip_renders_tool_running_label() {
    let mut app = App::new();
    app.mode = Mode::ToolRunning { tool: "grep".into(), since: Instant::now() };
    let spans = render_activity_strip_left(&app);
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(text.contains("running grep"));
}
```

#### Done when

- Mode indicator no longer appears in the status bar; the right side of
  row 0 hosts identity fields only.
- Activity strip is visible directly above the input on every frame
  (always-reserved row).
- Long-running tool calls display `⚙ running <tool> · Xs` for their full
  duration.
- After a tool completes, the strip reads `⠋ thinking · Xs` until the
  next content chunk arrives, then flips to `▶ streaming · Xs`.
- Esc-to-cancel hint visible right-aligned whenever the harness is
  busy.

---

### Phase V2 — Padding & visual rhythm

**Problem:** Transcript text runs edge-to-edge with no horizontal margin,
and turns aren't visually separated beyond a single blank line.

**Solution:** Add a 1-column horizontal margin around the transcript and
a dim horizontal rule between user→assistant turn boundaries.

#### Files

- `src/tui/ui/transcript.rs:17` — wrap the `Paragraph` in a `Block` with
  `Padding::horizontal(1)`:
  ```rust
  let block = Block::default().padding(Padding::horizontal(1));
  let para = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
  ```
  This insets transcript content one column from each edge without
  drawing borders.
- `src/tui/ui/transcript.rs` — replace the bare blank line between
  `UserPrompt` and the next `Assistant` with a dim horizontal rule
  (`────────`) sized to the inner width. Inside-a-turn separators
  (`Assistant → ToolCard → Assistant continuation`) keep a plain blank
  line. Implementation: track the previous item kind in the loop and
  emit the rule only on the user→assistant transition.
- `src/tui/ui/mod.rs` — apply the same 1-col padding to the status bar
  and activity strip so the whole UI has consistent inset.

#### Acceptance

```rust
#[test]
fn transcript_inserts_rule_between_user_and_assistant_turns() {
    let mut app = App::new();
    app.transcript.push(TranscriptItem::UserPrompt { body: "q".into() });
    app.transcript.push(TranscriptItem::Assistant { body: "a".into(), done: true });
    let lines = build_transcript_lines(&app); // helper extracted from draw_transcript
    let rendered: Vec<String> = lines.iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect();
    assert!(rendered.iter().any(|l: &String| l.contains("────")));
}

#[test]
fn transcript_does_not_insert_rule_between_assistant_and_tool() {
    // Within-turn boundary stays as a blank line.
    let mut app = App::new();
    app.transcript.push(TranscriptItem::Assistant { body: "a".into(), done: true });
    app.transcript.push(TranscriptItem::ToolCard {
        tool: "grep".into(),
        args_preview: "".into(),
        state: ToolCardState::Done { duration: Duration::ZERO, summary: "ok".into(), ok: true },
    });
    let lines = build_transcript_lines(&app);
    let rules = lines.iter().filter(|l| {
        l.spans.iter().any(|s| s.content.contains("────"))
    }).count();
    assert_eq!(rules, 0);
}
```

#### Done when

- Transcript content is inset 1 column from the terminal edges.
- A dim horizontal rule appears between every user prompt and the
  assistant response that follows.
- No rule appears between an assistant message and a tool card emitted
  in the same turn.

---

### Phase V3 — Tool card readability (optional)

**Problem:** During long tool runs the running line doesn't visibly tick
until the user scrolls — elapsed time only appears on the activity
strip, not on the card itself.

**Solution:** Right-align elapsed time on the running tool card line so
the card shows progress even when the activity strip is off-screen
(scrolled away from the live region).

#### Files

- `src/tui/ui/transcript.rs` — `render_tool_card_line`: when state is
  `Running`, compute elapsed and right-pad the line with the elapsed
  string in a dim color.
- `src/tui/ui/transcript.rs` — `render_tool_card_detail`: for the Done
  detail line, use a muted gutter color (e.g., `Color::DarkGray` for the
  4-space indent) so the detail text doesn't compete with body prose.

#### Acceptance

- Running tool card shows `⠋ grep ...   1.4s` with the elapsed time
  visibly updating between frames.
- Done tool card detail line is visually subordinate to assistant body
  text on dark and light themes.

#### Done when

- Manual smoke: kick off a slow `Bash` command (`sleep 5`) and see the
  inline elapsed time tick up while the card is on screen.

---

## Order

1. **Phase V1 first** — biggest perceived improvement (the "is it
   stuck?" feeling) and the smallest blast radius. Existing `Mode`
   tests cover the transitions; we add one variant and one layout row.
2. **Phase V2** — one-line `Padding` change plus a separator helper.
3. **Phase V3** — only if V1 doesn't already make the activity feel
   live enough.

## Open decisions

- **Always-reserved activity row vs. collapse-when-idle.** V1 reserves
  the row to keep layout stable. Alternative: collapse on `Idle` to
  reclaim a transcript line, at the cost of a 1-row jump on every state
  transition. Start with always-reserved; revisit if the empty row
  feels wasteful.
- **Separator style.** A single dim `────` rule reads cleanly. A double
  rule or a labeled rule (`── you · 14:32 ───`) is louder; defer until
  V2 lands and we can judge in real use.
- **Cancel hint placement.** Right-aligned on the activity strip is
  cheap. If it competes for space with elapsed-time on narrow
  terminals, drop it first under the same priority logic as the status
  bar's responsive collapse (`src/tui/ui/mod.rs:260-272`).
