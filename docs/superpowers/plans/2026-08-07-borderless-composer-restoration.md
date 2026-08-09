# Borderless Composer Restoration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restore the borderless Codex-style composer while preserving oli's fixed shell and redraw safeguards.

**Architecture:** Change only `draw_input` and its renderer tests. Keep the five-row composer rectangle, mode-transition terminal invalidation, full-width progress band, transcript scrolling, and stderr suspension unchanged.

**Tech Stack:** Rust 2024, Ratatui, tui-textarea, Ratatui `TestBackend`.

## Global Constraints

- Keep the composer height fixed at five rows.
- Paint every composer cell with `theme.user_band_bg`.
- Do not change progress geometry or redraw invalidation.
- Preserve unrelated uncommitted changes.

---

### Task 1: Restore the borderless composer

**Files:**
- Modify: `src/tui/ui/mod.rs`
- Test: `src/tui/ui/mod.rs`

**Interfaces:**
- Consumes: `draw_input(Frame, Rect, App)`, `COMPOSER_HEIGHT`, and `App::input`.
- Produces: a fixed-height borderless composer with a `›` gutter and padded text area.

- [ ] **Step 1: Write the failing visual contract test**

Render `draw_input` into a `20x5` `TestBackend` and assert the four corners are spaces, row one starts with ` › hello`, and every cell uses `theme.user_band_bg`.

- [ ] **Step 2: Verify the test fails**

Run: `cargo test --lib tui::ui::tests::composer_uses_borderless_prompt_band`

Expected: failure because the current composer renders rounded border glyphs.

- [ ] **Step 3: Implement the previous borderless renderer**

Restore `COMPOSER_V_PAD = 1` and `COMPOSER_GUTTER = 3`. Render a full-area tinted `Block`, draw the `›` glyph at the first text row, and render either the busy hint or `App::input` in the remaining padded rectangle. Keep `COMPOSER_HEIGHT = 5`.

- [ ] **Step 4: Align old composer tests with the approved contract**

Remove the rounded-border assertion and retain tests for full-area tint, prompt alignment, and fixed-height containment.

- [ ] **Step 5: Run focused tests**

Run: `cargo test --lib tui::ui::tests`

Expected: all UI tests pass.

### Task 2: Verify

**Files:**
- No additional production files.

**Interfaces:**
- Consumes: restored composer.
- Produces: verification evidence.

- [ ] **Step 1: Run formatting and diff checks**

Run: `cargo fmt --check && git diff --check`

Expected: exit 0.

- [ ] **Step 2: Run the full library suite**

Run: `cargo test --lib`

Expected: all tests pass.
