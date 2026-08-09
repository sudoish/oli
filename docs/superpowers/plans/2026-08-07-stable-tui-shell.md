# Stable TUI Shell Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prevent streamed transcript output from occupying the progress, composer, or footer rows by replacing dynamic bottom-shell geometry with fixed Ratatui regions.

**Architecture:** Keep the existing `App`, event loop, transcript item renderer, overlays, and terminal modes. Make the ordinary shell layout a pure fixed-region calculation, remove synthetic transcript bottom anchoring, and render the existing `tui_textarea::TextArea` inside a fixed bordered composer.

**Tech Stack:** Rust 2024, Ratatui, tui-textarea, Ratatui `TestBackend`, Cargo tests.

## Global Constraints

- Keep Rust as the only implementation language.
- Preserve agent streaming, tool cards, overlays, completion, search, approvals, footer data, and inline transcript commits.
- Use TDD: each behavior starts with a failing focused test.
- Do not restore dynamic composer growth or short-transcript bottom anchoring in this change.
- Preserve pre-existing uncommitted changes; do not stage unrelated files.

---

### Task 1: Fix the ordinary shell geometry

**Files:**
- Modify: `src/tui/ui/mod.rs`
- Test: `src/tui/ui/mod.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `ui::draw(&mut Frame, &mut App)` and the existing `draw_status_row`, `draw_input`, and `draw_footer` renderers.
- Produces: `ordinary_shell_areas(Rect) -> [Rect; 4]`, ordered transcript, progress, composer, footer.

- [ ] **Step 1: Write the failing fixed-layout test**

Add a pure geometry assertion:

```rust
#[test]
fn ordinary_shell_uses_fixed_bottom_regions() {
    let [transcript, progress, composer, footer] =
        ordinary_shell_areas(Rect::new(0, 0, 80, 24));
    assert_eq!(transcript, Rect::new(0, 0, 80, 17));
    assert_eq!(progress, Rect::new(0, 17, 80, 1));
    assert_eq!(composer, Rect::new(0, 18, 80, 5));
    assert_eq!(footer, Rect::new(0, 23, 80, 1));
}
```

- [ ] **Step 2: Run the test and verify it fails**

Run: `cargo test --lib tui::ui::tests::ordinary_shell_uses_fixed_bottom_regions`

Expected: compilation failure because `ordinary_shell_areas` does not exist.

- [ ] **Step 3: Add the fixed-region helper and use it in `draw`**

Add:

```rust
const COMPOSER_HEIGHT: u16 = 5;

fn ordinary_shell_areas(area: Rect) -> [Rect; 4] {
    let chunks = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(COMPOSER_HEIGHT),
        Constraint::Length(1),
    ])
    .split(area);
    [chunks[0], chunks[1], chunks[2], chunks[3]]
}
```

Replace the input-content-dependent layout in `draw` with:

```rust
let [transcript, progress, composer, footer] = ordinary_shell_areas(area);
transcript::draw_transcript(f, transcript, app);
if app.search().is_some() {
    draw_search_bar(f, progress, app);
} else {
    draw_status_row(f, progress, app);
}
draw_input(f, composer, app);
draw_footer(f, footer, app);
```

Update completion placement to receive `transcript` and `composer`.

- [ ] **Step 4: Run the focused test and existing UI tests**

Run: `cargo test --lib tui::ui::tests`

Expected: all `tui::ui::tests` pass.

### Task 2: Contain multiline input in a stock bordered composer

**Files:**
- Modify: `src/tui/ui/mod.rs`
- Test: `src/tui/ui/mod.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: fixed five-row composer `Rect` from `ordinary_shell_areas` and `App::input`.
- Produces: a full-rectangle bordered composer whose inner area contains either `App::input` or the busy hint.

- [ ] **Step 1: Write the failing composer-containment test**

Render an app containing six input lines into a five-row composer and assert that no input marker reaches the row below it:

```rust
#[test]
fn multiline_input_stays_inside_fixed_composer() {
    let mut app = App::new();
    app.set_input_text_pub("input-0\ninput-1\ninput-2\ninput-3\ninput-4\ninput-5");
    let mut term = Terminal::new(TestBackend::new(40, 7)).unwrap();
    term.draw(|f| {
        draw_input(f, Rect::new(0, 0, 40, 5), &app);
        f.render_widget(Paragraph::new("footer-marker"), Rect::new(0, 6, 40, 1));
    })
    .unwrap();
    let buf = term.backend().buffer();
    let footer = (0..40).map(|x| buf[(x, 6)].symbol()).collect::<String>();
    assert!(footer.contains("footer-marker"));
    assert!(!footer.contains("input-"));
}
```

- [ ] **Step 2: Run the test and verify it fails against the borderless composer contract**

Run: `cargo test --lib tui::ui::tests::multiline_input_stays_inside_fixed_composer`

Expected: fail because the composer does not render the specified bordered fixed-area contract.

- [ ] **Step 3: Replace `draw_input` with a bordered stock container**

Render a full-area `Block` and its inner rectangle:

```rust
let block = Block::default()
    .borders(Borders::ALL)
    .border_type(BorderType::Rounded)
    .title(" Input ")
    .style(Style::default().bg(theme.user_band_bg));
let inner = block.inner(area);
f.render_widget(block, area);
```

Render the busy hint as a `Paragraph` in `inner`; otherwise render `&app.input` in `inner`. Remove the custom gutter glyph and dynamic vertical padding constants.

- [ ] **Step 4: Run the focused UI tests**

Run: `cargo test --lib tui::ui::tests`

Expected: all UI tests pass after updating assertions that intentionally described the old borderless composer.

### Task 3: Remove transcript bottom anchoring and add the two-frame regression

**Files:**
- Modify: `src/tui/ui/transcript.rs`
- Modify: `src/tui/ui/mod.rs`
- Test: `src/tui/ui/mod.rs` and `src/tui/ui/transcript.rs`

**Interfaces:**
- Consumes: `build_transcript_lines(&App, u16) -> Vec<Line<'static>>` and fixed regions from `ordinary_shell_areas`.
- Produces: ordinary `Paragraph::scroll((offset, 0))` behavior without synthetic blank lines.

- [ ] **Step 1: Write a failing two-frame streaming regression**

Use one `Terminal<TestBackend>` for both draws. First call `app.on_turn_started()` and draw; then call `app.on_content_chunk` with more uniquely named lines than fit and draw again. Read the second buffer and assert:

```rust
for row in &rows[17..] {
    assert!(!row.contains("stream-marker-"));
}
assert!(rows[17].contains("Working"));
assert!(rows[18].contains('╭'));
assert!(rows[22].contains('╰'));
```

- [ ] **Step 2: Run the regression and record the failing assertion**

Run: `cargo test --lib tui::ui::tests::streaming_redraw_keeps_transcript_out_of_bottom_shell -- --exact`

Expected: fail on the current custom anchoring/dynamic-shell behavior.

- [ ] **Step 3: Remove synthetic anchoring**

In `draw_transcript`, delete:

```rust
let lines = anchor_to_bottom(lines, inner_width, height);
```

Delete `anchor_to_bottom` and its three unit tests. Keep the existing calculation of `max`, `scroll_override`, and `offset`, then pass the original lines directly to the transcript `Paragraph`.

- [ ] **Step 4: Run focused regressions**

Run: `cargo test --lib tui::ui::tests::streaming_redraw_keeps_transcript_out_of_bottom_shell -- --exact`

Expected: pass.

Run: `cargo test --lib tui::ui::transcript::tests`

Expected: all remaining transcript tests pass.

### Task 4: Verify and hand off

**Files:**
- Modify only if test expectations must be aligned with the approved fixed-shell design: `src/tui/ui/mod.rs`, `src/tui/ui/transcript.rs`

**Interfaces:**
- Consumes: completed stable shell.
- Produces: verification evidence and a manual test recipe.

- [ ] **Step 1: Format and inspect the patch**

Run: `cargo fmt --check`

Expected: exit 0. If it reports formatting differences, run `cargo fmt`, then rerun `cargo fmt --check`.

Run: `git diff --check`

Expected: exit 0.

- [ ] **Step 2: Run the full library suite**

Run: `cargo test --lib`

Expected: all library tests pass.

- [ ] **Step 3: Review the final diff against scope**

Run: `git diff -- src/tui/ui/mod.rs src/tui/ui/transcript.rs`

Expected: only fixed shell geometry, stock bordered composer, removal of synthetic anchoring, and their tests; existing user changes remain intact.

- [ ] **Step 4: Commit only owned implementation changes when they can be isolated**

Because `src/tui/ui/mod.rs` already contains user changes, inspect the diff before staging. If the implementation changes cannot be separated from those edits without rewriting or staging user work, leave them uncommitted and report that explicitly. Otherwise run:

```bash
git add src/tui/ui/mod.rs src/tui/ui/transcript.rs
git commit -m "fix(tui): stabilize streaming shell layout"
```

- [ ] **Step 5: Manual verification recipe**

Run oli in fullscreen and inline modes, submit a prompt that streams at least twenty lines and invokes one tool, and confirm that the progress row, composer border, and footer never move or retain transcript fragments.
