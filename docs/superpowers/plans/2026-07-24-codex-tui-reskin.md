# Codex TUI Reskin Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Re-skin oli's TUI to the Codex CLI visual language — kill the top status bar, borderless tinted composer, `•`-bullet transcript cells, collapsing tool cards, inline approval pane — per `specs/codex-tui-reskin.md`.

**Architecture:** In-place phased reskin of the existing ratatui renderers (`src/tui/ui/`), six TDD slices. Rendering stays re-derived-from-`App` per frame; all new renderers are pure `Vec<Line>` builders tested structurally (the V2 pattern).

**Tech Stack:** Rust 2024 (MSRV 1.95), ratatui, crossterm, tui-textarea-2 0.10, cargo.

## Global Constraints

- Stay on ratatui. No new dependencies.
- Test loop: `cargo test --lib` (~2s). TDD: failing test first, always.
- Match the surrounding code style; no backwards-compat shims; one concern per task.
- `Theme` gains `user_band_bg` (Task 2) and `diff_add_bg` / `diff_del_bg` (Task 4); all three presets (`dark`, `light`, `dimmed`) get values.
- Footer collapse order: drop `? for shortcuts` → drop `branch` → center-truncate cwd → drop `model`. Right-side gauge never truncates.
- Spec amendments discovered during planning (approved look otherwise unchanged):
  1. **No line-number gutter in diffs.** `policy::render_unified_diff` (`src/policy/mod.rs:342`) emits `    + body` / `    - body` / `      body` with no line numbers. Diff rows get sign-colored fg + full-width bg tint, no gutter.
  2. **No `(+N -M)` diffstat on `• Edited` headers** — the Edit tool result (`Successfully edited <path> (N replacements)`) carries no add/del counts.
  3. Collapsed `Done` tool cards keep oli's dim ` 0.12s` duration suffix.

---

### Task 1: Shell layout + composer + footer

Kill the top status bar and activity strip; new band layout: transcript / status row (2) / borderless composer / footer. Add `cwd` to the status model.

**Files:**
- Modify: `src/tui/app/mod.rs` (`StatusModel` at :176, placeholder at :725, `?` key in `on_key`)
- Modify: `src/tui/mod.rs` (`initial_status` at :82)
- Modify: `src/tui/ui/mod.rs` (`draw` at :28; delete `TITLE`, `draw_status`, `build_status_fields`, `token_gauge_field`, `draw_activity_strip`, `render_activity_strip_left/right`, `context_hint_text`, `inner_for_input`; new `draw_status_row`/`render_status_row`, `draw_footer`/`build_footer_line`, `context_gauge_spans`, `footer_identity_spans`, `truncate_middle`, `fmt_elapsed`, new `draw_input`)

**Interfaces:**
- Consumes: existing `App.status: StatusModel`, `App.mode: Mode`, `app.approval()`, `spinner_glyph` (ui/mod.rs:442), `format_count` (ui/mod.rs:249), `spans_width` (ui/mod.rs:170).
- Produces: `StatusModel.cwd: String`; `render_status_row(&App) -> Vec<Line<'static>>`; `build_footer_line(&App, u16) -> Line<'static>`; `fmt_elapsed(u64) -> String`; `truncate_middle(&str, usize) -> String`. Task 6 reuses `fmt_elapsed`.

- [ ] **Step 1: Write the failing tests**

Append to `src/tui/ui/mod.rs` `mod tests` (replacing the deleted `token_gauge_*`, `build_status_fields_*`, and all `activity_strip_*` tests):

```rust
    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn fmt_elapsed_renders_seconds_minutes_hours() {
        assert_eq!(fmt_elapsed(0), "0s");
        assert_eq!(fmt_elapsed(59), "59s");
        assert_eq!(fmt_elapsed(65), "1m 05s");
        assert_eq!(fmt_elapsed(3723), "1h 02m 03s");
    }

    #[test]
    fn truncate_middle_keeps_head_and_tail() {
        assert_eq!(truncate_middle("abcdefghij", 6), "ab…hij");
        assert_eq!(truncate_middle("short", 10), "short");
    }

    #[test]
    fn context_gauge_renders_percent_left_with_threshold_colors() {
        let green = app_with_status(StatusModel {
            ctx_window: 100_000,
            last_usage: Some(Usage { prompt_tokens: 30_000, completion_tokens: 5_000, total_tokens: 35_000 }),
            ..Default::default()
        });
        let spans = context_gauge_spans(&green);
        assert_eq!(spans[0].content, "65%");
        assert_eq!(spans[0].style.fg, Some(Color::Green));
        assert_eq!(spans[1].content, " context left");

        let amber = app_with_status(StatusModel {
            ctx_window: 100_000,
            last_usage: Some(Usage { prompt_tokens: 70_000, completion_tokens: 0, total_tokens: 70_000 }),
            ..Default::default()
        });
        assert_eq!(context_gauge_spans(&amber)[0].style.fg, Some(Color::Yellow));

        let red = app_with_status(StatusModel {
            ctx_window: 100_000,
            last_usage: Some(Usage { prompt_tokens: 90_000, completion_tokens: 0, total_tokens: 90_000 }),
            ..Default::default()
        });
        assert_eq!(context_gauge_spans(&red)[0].style.fg, Some(Color::Red));
    }

    #[test]
    fn context_gauge_falls_back_to_used_count_when_window_unknown() {
        let app = app_with_status(StatusModel {
            ctx_window: 0,
            last_usage: Some(Usage { prompt_tokens: 12_345, completion_tokens: 0, total_tokens: 12_345 }),
            ..Default::default()
        });
        let spans = context_gauge_spans(&app);
        assert_eq!(line_text(&Line::from(spans)), "12.3k used");
    }

    #[test]
    fn status_row_blank_when_idle_and_when_approval_pending() {
        let app = app_with_status(StatusModel::default());
        assert!(render_status_row(&app).iter().all(|l| line_text(l).is_empty()));

        let mut app = app_with_status(StatusModel::default());
        app.on_approval_requested("Edit".into(), serde_json::json!({"file_path":"x"}), "edit".into());
        assert!(render_status_row(&app).iter().all(|l| line_text(l).is_empty()));
    }

    #[test]
    fn status_row_working_with_elapsed_while_streaming() {
        let mut app = app_with_status(StatusModel::default());
        app.on_turn_started();
        app.on_content_chunk("hi");
        let lines = render_status_row(&app);
        let text = line_text(&lines[0]);
        assert!(text.contains("Working"), "got: {text}");
        assert!(text.contains("esc to interrupt"), "got: {text}");
    }

    #[test]
    fn status_row_shows_tool_detail_while_running() {
        let mut app = app_with_status(StatusModel::default());
        app.on_turn_started();
        app.on_tool_start(1, "Grep".into(), "pattern src/".into());
        let lines = render_status_row(&app);
        assert!(line_text(&lines[1]).contains("└ Grep pattern src/"));
    }

    #[test]
    fn footer_collapses_in_order_when_narrow() {
        let app = app_with_status(StatusModel {
            model: "kimi-k3".into(),
            ctx_window: 100_000,
            branch: Some("main".into()),
            cwd: "~/dev/oli".into(),
            ..Default::default()
        });
        // Wide: everything fits.
        let wide = line_text(&build_footer_line(&app, 100));
        assert!(wide.contains("? for shortcuts"), "got: {wide}");
        assert!(wide.contains("main"), "got: {wide}");

        // 60 cols: shortcuts dropped first; branch + model survive.
        let mid = line_text(&build_footer_line(&app, 60));
        assert!(!mid.contains("? for shortcuts"), "got: {mid}");
        assert!(mid.contains("main"), "got: {mid}");
        assert!(mid.contains("kimi-k3"), "got: {mid}");

        // 25 cols: only the model + gauge survive.
        let narrow = line_text(&build_footer_line(&app, 25));
        assert!(!narrow.contains("main"), "got: {narrow}");
        assert!(narrow.contains("kimi-k3"), "got: {narrow}");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib tui::ui::tests`
Expected: FAIL — `fmt_elapsed`, `truncate_middle`, `context_gauge_spans`, `render_status_row`, `build_footer_line` don't exist (compile error).

- [ ] **Step 3: Implement**

`src/tui/app/mod.rs` — `StatusModel` gains `cwd`:

```rust
pub struct StatusModel {
    pub session_id: Option<String>,
    pub model: String,
    pub ctx_window: u32,
    pub branch: Option<String>,
    /// `~`-relativized cwd for the footer.
    pub cwd: String,
    pub last_usage: Option<crate::providers::Usage>,
    pub session_usage: crate::providers::Usage,
}
```

Placeholder (:725): `t.set_placeholder_text("Ask oli to do anything");`

`?` key — add to `App::on_key`'s early `match key.code` block:

```rust
            KeyCode::Char('?')
                if self.input.lines().len() == 1 && self.input.lines()[0].is_empty() =>
            {
                self.open_help_browser();
                return SubmitAction::None;
            }
```

`src/tui/mod.rs` — populate `cwd` in `initial_status` (:82) and add the helper:

```rust
    let initial_status = app::StatusModel {
        session_id,
        model: agent.model.clone(),
        ctx_window: agent.caps.ctx_window as u32,
        branch: detect_git_branch(),
        cwd: current_dir_label(),
        last_usage: None,
        session_usage: Default::default(),
    };
```

```rust
/// `~`-relativized cwd for the footer.
fn current_dir_label() -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    let raw = cwd.to_string_lossy().into_owned();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && raw.starts_with(&home) => {
            format!("~{}", &raw[home.len()..])
        }
        _ => raw,
    }
}
```

`src/tui/ui/mod.rs` — delete `TITLE`, `draw_status`, `build_status_fields`, `token_gauge_field`, `draw_activity_strip`, `render_activity_strip_left`, `render_activity_strip_right`, `context_hint_text`, `inner_for_input`. New `draw`:

```rust
pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let input_lines = app.input.lines().len().max(1).min(8) as u16;
    let chunks = Layout::vertical([
        Constraint::Min(3),              // transcript
        Constraint::Length(2),           // status row (+ tool detail)
        Constraint::Length(input_lines), // composer (borderless)
        Constraint::Length(1),           // footer
    ])
    .split(area);

    transcript::draw_transcript(f, chunks[0], app);
    if app.search().is_some() {
        draw_search_bar(f, chunks[1], app);
    } else {
        draw_status_row(f, chunks[1], app);
    }
    draw_input(f, chunks[2], app);
    draw_footer(f, chunks[3], app);

    if app.completion.is_some() {
        draw_completion_popup(f, chunks[0], chunks[2], app);
    }
    use crate::tui::app::Overlay;
    match &app.overlay {
        Some(Overlay::Approval(s)) => overlays::draw_approval_modal(f, area, s, app),
        Some(Overlay::SessionsPicker(s)) => overlays::draw_sessions_picker(f, area, s, &app.theme),
        Some(Overlay::HelpBrowser(s)) => overlays::draw_help_browser(f, area, s, &app.theme),
        Some(Overlay::InlineHelp(s)) => overlays::draw_inline_help(f, area, s, &app.theme),
        Some(Overlay::HistorySearch(s)) => overlays::draw_history_search(f, area, s, app),
        Some(Overlay::CopyFallback(s)) => overlays::draw_copy_fallback(f, area, s, &app.theme),
        Some(Overlay::Wizard(s)) => overlays::draw_wizard(f, area, s),
        Some(Overlay::Search(_)) | None => {}
    }
}
```

New functions (extend the `use crate::tui::app::{App, Mode};` import with `ToolCardState, TranscriptItem`):

```rust
fn draw_status_row(f: &mut Frame, area: Rect, app: &App) {
    let lines = render_status_row(app);
    let row = Paragraph::new(lines)
        .block(Block::default().padding(Padding::horizontal(TRANSCRIPT_H_PAD)));
    f.render_widget(row, area);
}

/// Codex-style status: `⠋ Working (12s • esc to interrupt)` plus a
/// dim `  └ <tool> <args>` detail while a tool runs. Reserved blank
/// rows when idle or when an approval owns the bottom pane.
pub(super) fn render_status_row(app: &App) -> Vec<Line<'static>> {
    let blank = || vec![Line::raw(""), Line::raw("")];
    if app.approval().is_some() {
        return blank();
    }
    let (word, since) = match &app.mode {
        Mode::Idle => return blank(),
        Mode::Thinking { since } => ("Thinking", since),
        Mode::Streaming { since } => ("Working", since),
        Mode::ToolRunning { since, .. } => ("Working", since),
    };
    let secs = since.elapsed().as_secs();
    let theme = &app.theme;
    let status = Line::from(vec![
        Span::styled(
            format!("{} ", spinner_glyph(secs as f32)),
            Style::default().fg(theme.accent),
        ),
        Span::styled(
            word.to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" ({} • esc to interrupt)", fmt_elapsed(secs)),
            Style::default().fg(theme.dim),
        ),
    ]);
    let detail = match &app.mode {
        Mode::ToolRunning { tool, .. } => {
            let args = latest_running_args(app).unwrap_or_default();
            Line::from(vec![
                Span::styled("  └ ".to_string(), Style::default().fg(theme.dim)),
                Span::styled(format!("{} {}", tool, args), Style::default().fg(theme.dim)),
            ])
        }
        _ => Line::raw(""),
    };
    vec![status, detail]
}

/// Args preview of the most recent still-Running tool card.
fn latest_running_args(app: &App) -> Option<String> {
    app.transcript.iter().rev().find_map(|item| match item {
        TranscriptItem::ToolCard {
            args_preview,
            state: ToolCardState::Running { .. },
            ..
        } => Some(args_preview.clone()),
        _ => None,
    })
}

/// `0s`, `59s`, `1m 05s`, `1h 02m 03s`.
pub(super) fn fmt_elapsed(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h {:02}m {:02}s", secs / 3600, (secs % 3600) / 60, secs % 60)
    }
}
```

Borderless composer (replaces `draw_input` + `inner_for_input`):

```rust
fn draw_input(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    // Borderless composer on a tinted band; `›` lives in a 2-col
    // gutter left of the textarea (Codex composer shape).
    f.render_widget(
        Block::default().style(Style::default().bg(theme.user_band_bg)),
        area,
    );
    let busy = app.is_busy();
    let glyph_style = if busy {
        Style::default().fg(theme.dim).bg(theme.user_band_bg)
    } else {
        Style::default().bg(theme.user_band_bg).add_modifier(Modifier::BOLD)
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" ".to_string(), Style::default().bg(theme.user_band_bg)),
            Span::styled("›".to_string(), glyph_style),
        ])),
        Rect { x: area.x, y: area.y, width: 2, height: 1 },
    );
    let text_area = Rect {
        x: area.x + 2,
        y: area.y,
        width: area.width.saturating_sub(2),
        height: area.height,
    };
    if busy {
        let body = Paragraph::new(Line::from(Span::styled(
            "(waiting for response — Ctrl+C cancels)".to_string(),
            Style::default()
                .fg(theme.dim)
                .bg(theme.user_band_bg)
                .add_modifier(Modifier::ITALIC),
        )));
        f.render_widget(body, text_area);
        return;
    }
    f.render_widget(&app.input, text_area);
}
```

Footer:

```rust
fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let line = build_footer_line(app, area.width.saturating_sub(TRANSCRIPT_H_PAD * 2));
    let footer = Paragraph::new(line)
        .block(Block::default().padding(Padding::horizontal(TRANSCRIPT_H_PAD)));
    f.render_widget(footer, area);
}

/// Left: `? for shortcuts · <cwd> · <branch> · <model>` (or the
/// focused-card hint when one is focused); right: color-graded
/// context gauge. Collapse order: shortcuts → branch → cwd
/// (center-truncated) → model. The gauge never truncates.
pub(super) fn build_footer_line(app: &App, width: u16) -> Line<'static> {
    let theme = &app.theme;
    let right = context_gauge_spans(app);
    let right_w = spans_width(&right) as u16;

    let mut left: Vec<Span<'static>> = if app.focused_card_idx.is_some()
        && matches!(app.mode, Mode::Idle)
    {
        vec![Span::styled(
            "enter expand · {/} cards · esc clear".to_string(),
            Style::default().fg(theme.dim).add_modifier(Modifier::ITALIC),
        )]
    } else {
        footer_identity_spans(app, width.saturating_sub(right_w + 1))
    };

    let left_w = spans_width(&left) as u16;
    let pad = width.saturating_sub(left_w + right_w);
    if pad > 0 {
        left.push(Span::raw(" ".repeat(pad as usize)));
    }
    left.extend(right);
    Line::from(left)
}

/// Left-side identity spans, already collapsed to fit `budget` cols.
fn footer_identity_spans(app: &App, budget: u16) -> Vec<Span<'static>> {
    let theme = &app.theme;
    let mut cwd = app.status.cwd.clone();
    let mut show_shortcuts = true;
    let mut show_branch = app.status.branch.is_some();
    let mut show_model = !app.status.model.is_empty();

    let width_of = |sc: bool, cwd: &str, br: bool, mo: bool| -> u16 {
        let mut parts: Vec<usize> = Vec::new();
        if sc {
            parts.push(15); // "? for shortcuts"
        }
        if !cwd.is_empty() {
            parts.push(cwd.chars().count());
        }
        if br {
            parts.push(app.status.branch.as_ref().unwrap().chars().count());
        }
        if mo {
            parts.push(app.status.model.chars().count());
        }
        let seps = parts.len().saturating_sub(1) * 3; // " · "
        (parts.iter().sum::<usize>() + seps) as u16
    };

    if width_of(show_shortcuts, &cwd, show_branch, show_model) > budget {
        show_shortcuts = false;
    }
    if width_of(show_shortcuts, &cwd, show_branch, show_model) > budget {
        show_branch = false;
    }
    if width_of(show_shortcuts, &cwd, show_branch, show_model) > budget {
        let others = width_of(show_shortcuts, "", show_branch, show_model);
        let room = budget.saturating_sub(others) as usize;
        if room >= 4 {
            cwd = truncate_middle(&cwd, room);
        } else {
            cwd.clear();
        }
    }
    if width_of(show_shortcuts, &cwd, show_branch, show_model) > budget {
        show_model = false;
    }

    let mut spans: Vec<Span<'static>> = Vec::new();
    macro_rules! sep {
        () => {
            if !spans.is_empty() {
                spans.push(Span::styled(" · ".to_string(), Style::default().fg(theme.dim)));
            }
        };
    }
    if show_shortcuts {
        sep!();
        spans.push(Span::styled(
            "?".to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            " for shortcuts".to_string(),
            Style::default().fg(theme.dim),
        ));
    }
    if !cwd.is_empty() {
        sep!();
        spans.push(Span::styled(cwd, Style::default().fg(theme.dim)));
    }
    if show_branch {
        sep!();
        spans.push(Span::styled(
            app.status.branch.clone().unwrap(),
            Style::default().fg(theme.user),
        ));
    }
    if show_model {
        sep!();
        spans.push(Span::styled(app.status.model.clone(), Style::default().fg(theme.fg)));
    }
    spans
}

/// `abcdefghij` @ 6 → `ab…hij`. No-op when `s` fits or `max` < 4.
pub(super) fn truncate_middle(s: &str, max: usize) -> String {
    if s.chars().count() <= max || max < 4 {
        return s.to_string();
    }
    let keep = max - 1;
    let head = keep / 2;
    let tail = keep - head;
    let h: String = s.chars().take(head).collect();
    let t: String = s.chars().skip(s.chars().count() - tail).collect();
    format!("{}…{}", h, t)
}

/// Right side of the footer — `92% context left` color-graded by
/// usage ratio (green <60%, amber 60–85%, red >85% used), or
/// `<used> used` when the provider reported no context window.
fn context_gauge_spans(app: &App) -> Vec<Span<'static>> {
    let theme = &app.theme;
    let used = app
        .status
        .last_usage
        .map(|u| u.prompt_tokens as u32 + u.completion_tokens as u32)
        .unwrap_or(0);
    let ctx = app.status.ctx_window;
    if ctx == 0 {
        return vec![Span::styled(
            format!("{} used", format_count(used)),
            Style::default().fg(theme.dim),
        )];
    }
    let ratio = used as f32 / ctx as f32;
    let color = if ratio >= 0.85 {
        theme.gauge_danger
    } else if ratio >= 0.60 {
        theme.gauge_warn
    } else {
        theme.gauge_ok
    };
    let left = ((1.0 - ratio).max(0.0) * 100.0).round() as u32;
    vec![
        Span::styled(format!("{}%", left), Style::default().fg(color)),
        Span::styled(" context left".to_string(), Style::default().fg(theme.dim)),
    ]
}
```

In the tests mod: delete `token_gauge_picks_*`, `token_gauge_renders_dash_when_no_usage_recorded`, `build_status_fields_include_model_and_branch`, and every `activity_strip_*` test; keep `format_count_renders_units_at_thresholds`, the completion-line tests, and `app_with_status`. `user_band_bg` is added to `Theme` in Task 2 — for this task to compile standalone, add the field now instead (see note below).

**Note on ordering:** `draw_input` references `theme.user_band_bg`, which the spec assigns to the message-cells slice. Add the field in THIS task so the tree compiles; Task 2 only consumes it. Exact edit to `src/tui/theme.rs` — add to `Theme` (after `border`):

```rust
    /// Background tint for the user-message band and the composer.
    pub user_band_bg: Color,
```

Preset values (pre-computed blends over a black/white terminal bg — dark: white @12%, light: black @4%, dimmed: white @6%):

```rust
// dark():   user_band_bg: Color::Rgb(0x1f, 0x1f, 0x1f),
// light():  user_band_bg: Color::Rgb(0xf5, 0xf5, 0xf5),
// dimmed(): user_band_bg: Color::Rgb(0x10, 0x10, 0x10),
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib tui::ui::tests`
Expected: PASS (all new footer/gauge/status-row/elapsed tests).

- [ ] **Step 5: Full suite + build**

Run: `cargo test --lib && cargo build`
Expected: PASS; no warnings about unused `TITLE`/`draw_status` etc.

- [ ] **Step 6: Commit**

```bash
git add src/tui/app/mod.rs src/tui/mod.rs src/tui/ui/mod.rs src/tui/theme.rs
git commit -m "tui(codex): replace top bar + activity strip with status row, borderless composer, footer"
```

---

### Task 2: Message cells — user band, `•` assistant, notice glyphs

**Files:**
- Modify: `src/tui/ui/transcript.rs` (item renderers at :150-258, `user_turn_line_indices` at :352, `bubble_width_for`/`separator_rule` deletions, tests)

**Interfaces:**
- Consumes: `Theme.user_band_bg` (Task 1), `markdown::render(body, app.markdown_theme) -> Vec<Line<'static>>`, `wrap_to_width` (transcript.rs:275).
- Produces: band rendering contract — user lines are padded to full `rule_width` with `bg = user_band_bg`; `user_turn_line_indices` detects spans whose content is exactly `"› "`.

- [ ] **Step 1: Write the failing tests**

Replace the rule/bubble tests (`rule_appears_*`, `rule_does_not_*` ×2, `rule_width_matches_requested_width`, `user_prompt_renders_you_header_with_right_bar`, `user_prompt_body_is_right_aligned_with_2col_right_gutter`, `user_prompt_wraps_long_text_into_right_aligned_chunks`, `bubble_width_caps_at_60`) in `src/tui/ui/transcript.rs` tests with:

```rust
    #[test]
    fn user_prompt_renders_full_width_banded_block() {
        let mut app = empty_app();
        app.transcript
            .push(TranscriptItem::UserPrompt { body: "hi there".into() });
        let lines = build_transcript_lines(&app, 40);
        let band = Some(app.theme.user_band_bg);
        let body = lines
            .iter()
            .find(|l| line_text(l).contains("hi there"))
            .expect("body line");
        assert_eq!(line_text(body).chars().count(), 40);
        assert!(line_text(body).starts_with("› "));
        assert!(body.spans.iter().all(|s| s.style.bg == band));
        let first = &body.spans[0];
        assert_eq!(first.content, "› ");
        assert!(first.style.add_modifier.contains(Modifier::BOLD));
        assert!(first.style.add_modifier.contains(Modifier::DIM));
        // Tinted blank pad lines above and below the message.
        let idx = lines.iter().position(|l| std::ptr::eq(l, body)).unwrap();
        for neighbor in [&lines[idx - 1], &lines[idx + 1]] {
            assert!(line_text(neighbor).trim().is_empty());
            assert!(neighbor.spans.iter().all(|s| s.style.bg == band));
        }
    }

    #[test]
    fn user_prompt_wraps_with_indented_continuations() {
        let mut app = empty_app();
        app.transcript.push(TranscriptItem::UserPrompt {
            body: "one two three four five six seven eight nine ten".into(),
        });
        let lines = build_transcript_lines(&app, 20);
        let band = Some(app.theme.user_band_bg);
        let visual: Vec<&Line> = lines
            .iter()
            .filter(|l| !line_text(l).trim().is_empty())
            .collect();
        assert!(visual.len() >= 3, "expected wrap, got {}", visual.len());
        assert!(line_text(visual[0]).starts_with("› "));
        for cont in &visual[1..] {
            assert!(line_text(cont).starts_with("  "), "got: {:?}", line_text(cont));
        }
        for l in visual {
            assert_eq!(line_text(l).chars().count(), 20);
            assert!(l.spans.iter().all(|s| s.style.bg == band));
        }
    }

    #[test]
    fn assistant_renders_bullet_prefix_without_header_or_cursor() {
        let mut app = empty_app();
        app.transcript.push(TranscriptItem::Assistant {
            body: "hello world".into(),
            done: false,
        });
        app.active_assistant = Some(0);
        let lines = build_transcript_lines(&app, 40);
        let text = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(text.contains("• hello world"), "got: {text}");
        assert!(!text.contains('▌'), "old header glyph leaked: {text}");
        assert!(!text.contains('▍'), "streaming cursor leaked: {text}");
        let first = lines
            .iter()
            .find(|l| line_text(l).contains("hello world"))
            .expect("body line");
        assert_eq!(first.spans[0].content, "• ");
        assert_eq!(first.spans[0].style.fg, Some(app.theme.dim));
    }

    #[test]
    fn assistant_waiting_placeholder_while_empty_and_active() {
        let mut app = empty_app();
        app.on_turn_started();
        let lines = build_transcript_lines(&app, 40);
        let text = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(text.contains("waiting for first token"), "got: {text}");
    }

    #[test]
    fn system_note_stays_dim_italic() {
        let mut app = empty_app();
        app.on_system_note("just a note".into());
        let lines = build_transcript_lines(&app, 40);
        let note = lines
            .iter()
            .find(|l| line_text(l).contains("just a note"))
            .expect("note line");
        assert!(line_text(note).starts_with("  "));
        assert!(note.spans[0].style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn error_note_renders_block_glyph_in_error_color() {
        let mut app = empty_app();
        app.on_turn_error("boom");
        let lines = build_transcript_lines(&app, 40);
        let err = lines
            .iter()
            .find(|l| line_text(l).contains("boom"))
            .expect("error line");
        assert!(line_text(err).starts_with("■ "), "got: {:?}", line_text(err));
        assert_eq!(err.spans[0].style.fg, Some(app.theme.tool_err));
    }
```

Also update `user_turn_line_indices` tests (:1094-1133): assert each indexed row's text contains `"› first"` / `"› second"` instead of `"you "`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib tui::ui::transcript::tests`
Expected: FAIL — band assertions fail against the old bubble/header rendering (and `user_band_bg` unresolved if Task 1's note was skipped).

- [ ] **Step 3: Implement**

In `build_transcript_lines_range` replace the `UserPrompt`, `Assistant`, and `System` arms, and simplify the loop-end separator (no more rule — always a blank line; the band self-pads):

```rust
            TranscriptItem::UserPrompt { body } => {
                let band = Style::default().bg(theme.user_band_bg);
                let width = rule_width.max(1) as usize;
                let pad_line = || Line::from(Span::styled(" ".repeat(width), band));
                lines.push(pad_line());
                let body_w = rule_width.saturating_sub(2).max(1) as usize;
                let mut visual: Vec<String> = Vec::new();
                for body_line in body.lines() {
                    visual.extend(wrap_to_width(body_line, body_w));
                }
                if visual.is_empty() {
                    visual.push(String::new());
                }
                for (idx, chunk) in visual.iter().enumerate() {
                    let (prefix, prefix_style) = if idx == 0 {
                        (
                            "› ",
                            band.add_modifier(Modifier::BOLD).add_modifier(Modifier::DIM),
                        )
                    } else {
                        ("  ", band)
                    };
                    let used = prefix.chars().count() + chunk.chars().count();
                    let pad = width.saturating_sub(used);
                    lines.push(Line::from(vec![
                        Span::styled(prefix.to_string(), prefix_style),
                        Span::styled(chunk.clone(), band),
                        Span::styled(" ".repeat(pad), band),
                    ]));
                }
                lines.push(pad_line());
            }
            TranscriptItem::Assistant { body, .. } => {
                if body.is_empty() && is_active {
                    lines.push(Line::from(vec![
                        Span::styled("• ".to_string(), Style::default().fg(theme.dim)),
                        Span::styled(
                            "waiting for first token".to_string(),
                            Style::default().fg(theme.dim).add_modifier(Modifier::ITALIC),
                        ),
                    ]));
                } else {
                    let mut md_lines = markdown::render(body, app.markdown_theme);
                    if md_lines.is_empty() {
                        md_lines.push(Line::raw(""));
                    }
                    for (idx, md_line) in md_lines.into_iter().enumerate() {
                        let mut spans: Vec<Span<'static>> =
                            Vec::with_capacity(md_line.spans.len() + 1);
                        if idx == 0 {
                            spans.push(Span::styled(
                                "• ".to_string(),
                                Style::default().fg(theme.dim),
                            ));
                        } else {
                            spans.push(Span::raw("  "));
                        }
                        spans.extend(md_line.spans.into_iter());
                        lines.push(Line::from(spans));
                    }
                }
            }
            TranscriptItem::System { body } => {
                if let Some(msg) = body.strip_prefix("error: ") {
                    lines.push(Line::from(Span::styled(
                        format!("■ {}", msg),
                        Style::default().fg(theme.tool_err),
                    )));
                } else if body.starts_with("✔ ") {
                    lines.push(Line::from(Span::styled(
                        body.clone(),
                        Style::default().fg(theme.tool_ok),
                    )));
                } else if body.starts_with("✗ ") {
                    lines.push(Line::from(Span::styled(
                        body.clone(),
                        Style::default().fg(theme.tool_err),
                    )));
                } else {
                    for body_line in body.lines() {
                        lines.push(Line::from(Span::styled(
                            format!("  {}", body_line),
                            Style::default()
                                .fg(theme.dim)
                                .add_modifier(Modifier::ITALIC),
                        )));
                    }
                }
            }
```

Loop-end: delete the `user_to_assistant` special case and `separator_rule`; always `lines.push(Line::raw(""));`. Delete `bubble_width_for`. Keep `wrap_to_width`.

Update `user_turn_line_indices` (:352) — detect the `› ` band span instead of `▐`:

```rust
/// Line indices (post-layout) of the start of each user turn.
/// Detects them by the `› ` prefix span emitted only by the
/// UserPrompt band in `build_transcript_lines`.
pub(super) fn user_turn_line_indices(lines: &[Line<'_>]) -> Vec<u16> {
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if line.spans.iter().any(|s| s.content.as_ref() == "› ") {
            out.push(i as u16);
        }
    }
    out
}
```

(The `✔`/`✗` System branches are consumed by Task 5's decision cells; adding them here keeps Task 5 purely additive.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib tui::ui::transcript::tests`
Expected: PASS.

- [ ] **Step 5: Full suite**

Run: `cargo test --lib`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/tui/ui/transcript.rs
git commit -m "tui(codex): banded › user cells, • assistant prefix, ■ error glyph"
```

---

### Task 3: Tool cells — verb/bullet grammar, connectors, collapse

**Files:**
- Modify: `src/tui/ui/transcript.rs` (`render_tool_card_line` at :465, `render_tool_card_detail` at :542, `expanded_output_lines` at :595, tests :1249-1512)

**Interfaces:**
- Consumes: `fmt_elapsed` (Task 1), `spinner_glyph`, existing `ToolCardState`.
- Produces: `tool_verbs(&str) -> (&'static str, &'static str)`; `connector_lines(Vec<String>, Style, &Theme) -> Vec<Line<'static>>` (first line `  └ `, rest `    `).

- [ ] **Step 1: Write the failing tests**

Replace the `render_tool_card_*` tests (:1249-1512, keeping the image-marker chip tests as-is) with:

```rust
    #[test]
    fn tool_verbs_map_by_tool_kind() {
        assert_eq!(tool_verbs("Bash"), ("Running", "Ran"));
        assert_eq!(tool_verbs("Edit"), ("Editing", "Edited"));
        assert_eq!(tool_verbs("Write"), ("Writing", "Wrote"));
        assert_eq!(tool_verbs("Read"), ("Reading", "Read"));
        assert_eq!(tool_verbs("Grep"), ("Searching", "Searched"));
        assert_eq!(tool_verbs("github__search_code"), ("Calling", "Called"));
    }

    #[test]
    fn done_ok_card_renders_green_bullet_past_verb_and_summary_connector() {
        let theme = Theme::dark();
        let state = ToolCardState::Done {
            duration: Duration::from_millis(120),
            summary: "12 lines".into(),
            ok: true,
            full_output: String::new(),
            expanded: false,
        };
        let line = render_tool_card_line("Read", "a.rs", &state, &theme, false);
        assert_eq!(line.spans[0].content, "• ");
        assert_eq!(line.spans[0].style.fg, Some(theme.tool_ok));
        let text = line_text(&line);
        assert!(text.starts_with("• Read a.rs"), "got: {text}");
        assert!(text.contains("0.12s"), "duration suffix: {text}");
        let detail = render_tool_card_detail("Read", &state, &theme);
        assert_eq!(line_text(&detail[0]), "  └ 12 lines");
    }

    #[test]
    fn done_fail_card_renders_red_bullet_and_error_tail() {
        let theme = Theme::dark();
        let state = ToolCardState::Done {
            duration: Duration::from_millis(1),
            summary: "exit 1".into(),
            ok: false,
            full_output: "line one\nline two\nline three\nline four".into(),
            expanded: false,
        };
        let line = render_tool_card_line("Bash", "false", &state, &theme, false);
        assert_eq!(line.spans[0].style.fg, Some(theme.tool_err));
        assert!(line_text(&line).starts_with("• Ran false"));
        let detail = render_tool_card_detail("Bash", &state, &theme);
        // Tail capped at 3 lines, error-colored, connector-prefixed.
        assert_eq!(detail.len(), 3);
        assert_eq!(line_text(&detail[0]), "  └ line one");
        assert_eq!(line_text(&detail[1]), "    line two");
        assert_eq!(detail[0].spans[1].style.fg, Some(theme.diff_removed));
    }

    #[test]
    fn running_card_renders_spinner_active_verb_and_elapsed() {
        let theme = Theme::dark();
        let state = ToolCardState::Running {
            started_at: std::time::Instant::now(),
        };
        let line = render_tool_card_line("Bash", "cargo test", &state, &theme, false);
        let text = line_text(&line);
        assert!(text.contains("Running cargo test"), "got: {text}");
        assert!(text.contains("(0s)"), "got: {text}");
        assert!(render_tool_card_detail("Bash", &state, &theme).is_empty());
    }

    #[test]
    fn streaming_edit_card_renders_dim_peek_with_connector() {
        let theme = Theme::dark();
        let state = ToolCardState::Streaming {
            provider_tool_id: "tu_1".into(),
            accumulated_json: r#"{"file_path":"a.rs","new_string":"hello\nworld"}"#.into(),
        };
        let detail = render_tool_card_detail("Edit", &state, &theme);
        assert_eq!(line_text(&detail[0]), "  └ hello");
        assert_eq!(line_text(&detail[1]), "    world");
        assert_eq!(detail[0].spans[1].style.fg, Some(theme.dim));
    }

    #[test]
    fn focused_card_renders_chevron_leader() {
        let theme = Theme::dark();
        let state = ToolCardState::Done {
            duration: Duration::from_millis(1),
            summary: "ok".into(),
            ok: true,
            full_output: String::new(),
            expanded: false,
        };
        let unfocused = render_tool_card_line("Read", "x", &state, &theme, false);
        let focused = render_tool_card_line("Read", "x", &state, &theme, true);
        assert!(line_text(&unfocused).starts_with("• "));
        assert!(line_text(&focused).starts_with("› "));
        assert_eq!(focused.spans[0].style.fg, Some(theme.accent));
    }

    #[test]
    fn expanded_done_shows_full_output_with_connectors_and_cap_hint() {
        let theme = Theme::dark();
        let body: String = (1..=50).map(|i| format!("L{}\n", i)).collect();
        let state = ToolCardState::Done {
            duration: Duration::from_millis(1),
            summary: "50 lines".into(),
            ok: true,
            full_output: body,
            expanded: true,
        };
        let detail = render_tool_card_detail("Read", &state, &theme);
        assert_eq!(detail.len(), 41);
        assert_eq!(line_text(&detail[0]), "  └ L1");
        assert_eq!(line_text(&detail[1]), "    L2");
        assert_eq!(line_text(&detail[40]), "    … +10 more lines");
    }

    #[test]
    fn expanded_empty_output_shows_placeholder_connector() {
        let theme = Theme::dark();
        let state = ToolCardState::Done {
            duration: Duration::from_millis(1),
            summary: "ok".into(),
            ok: true,
            full_output: String::new(),
            expanded: true,
        };
        let detail = render_tool_card_detail("Bash", &state, &theme);
        assert_eq!(line_text(&detail[0]), "  └ (no output)");
    }
```

Update `build_transcript_lines_renders_focused_card_with_sidebar` (:1467) and `build_transcript_lines_renders_expanded_full_output_under_focused_card` (:1491) to the new leaders/connectors (`"• Read a.rs"` / `"› Read a.rs"`; `"  └ alpha"` … and summary hidden when expanded).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib tui::ui::transcript::tests`
Expected: FAIL — `tool_verbs`/`connector_lines` don't exist; old `→`/`+ ` renders mismatch.

- [ ] **Step 3: Implement**

Extend the import at `src/tui/ui/transcript.rs:17` to `use super::{fmt_elapsed, spinner_glyph};`.

Replace `render_tool_card_line` (:465) and `render_tool_card_detail` (:542), add `tool_verbs` + `connector_lines`, and re-prefix `expanded_output_lines`:

```rust
/// `(active, done)` verb pair per tool. MCP tools arrive namespaced
/// (`server__tool`); anything unrecognized reads as a call.
fn tool_verbs(tool: &str) -> (&'static str, &'static str) {
    match tool.to_ascii_lowercase().as_str() {
        "bash" | "subprocess" | "task" => ("Running", "Ran"),
        "edit" | "multiedit" => ("Editing", "Edited"),
        "write" => ("Writing", "Wrote"),
        "read" => ("Reading", "Read"),
        "grep" | "glob" => ("Searching", "Searched"),
        "notes" => ("Noting", "Noted"),
        _ => ("Calling", "Called"),
    }
}

/// Header line of a tool card:
/// `• Ran cargo test --lib 2.31s` — animated spinner bullet while
/// running, green/red bold bullet when done, `›` when focused.
fn render_tool_card_line(
    tool: &str,
    args_preview: &str,
    state: &ToolCardState,
    theme: &Theme,
    focused: bool,
) -> Line<'static> {
    let (active_verb, done_verb) = tool_verbs(tool);
    let mut spans: Vec<Span<'static>> = if focused {
        vec![Span::styled(
            "› ".to_string(),
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        )]
    } else {
        match state {
            ToolCardState::Streaming { .. } => {
                vec![Span::styled("• ".to_string(), Style::default().fg(theme.dim))]
            }
            ToolCardState::Running { started_at } => {
                let secs = started_at.elapsed().as_secs_f32();
                vec![Span::styled(
                    format!("{} ", spinner_glyph(secs)),
                    Style::default().fg(theme.tool_running),
                )]
            }
            ToolCardState::Done { ok, .. } => vec![Span::styled(
                "• ".to_string(),
                Style::default()
                    .fg(if *ok { theme.tool_ok } else { theme.tool_err })
                    .add_modifier(Modifier::BOLD),
            )],
        }
    };
    let verb = match state {
        ToolCardState::Done { .. } => done_verb,
        _ => active_verb,
    };
    spans.push(Span::styled(
        verb.to_string(),
        Style::default().add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        args_preview.to_string(),
        Style::default().fg(theme.fg),
    ));
    match state {
        ToolCardState::Running { started_at } => {
            spans.push(Span::styled(
                format!(" ({})", fmt_elapsed(started_at.elapsed().as_secs())),
                Style::default().fg(theme.dim),
            ));
        }
        ToolCardState::Done { duration, .. } => {
            spans.push(Span::styled(
                format!(" {:.2}s", duration.as_secs_f32()),
                Style::default().fg(theme.dim),
            ));
        }
        ToolCardState::Streaming { .. } => {}
    }
    Line::from(spans)
}

/// First line `  └ `, continuations `    ` — Codex's output connector.
fn connector_lines(body: Vec<String>, body_style: Style, theme: &Theme) -> Vec<Line<'static>> {
    body.into_iter()
        .enumerate()
        .map(|(i, s)| {
            let prefix = if i == 0 { "  └ " } else { "    " };
            Line::from(vec![
                Span::styled(prefix.to_string(), Style::default().fg(theme.dim)),
                Span::styled(s, body_style),
            ])
        })
        .collect()
}

/// Detail lines under a card. Streaming Edit/Write: dim peek.
/// Running: nothing. Done+ok: one dim summary connector. Done+fail:
/// ≤3-line error tail so failures survive the collapse. Expanded:
/// full output under connectors (40-line cap).
fn render_tool_card_detail(
    tool: &str,
    state: &ToolCardState,
    theme: &Theme,
) -> Vec<Line<'static>> {
    match state {
        ToolCardState::Streaming { accumulated_json, .. } => connector_lines(
            extract_streaming_peek(tool, accumulated_json),
            Style::default().fg(theme.dim),
            theme,
        ),
        ToolCardState::Running { .. } => Vec::new(),
        ToolCardState::Done {
            summary,
            ok,
            full_output,
            expanded,
            ..
        } => {
            if *expanded {
                expanded_output_lines(full_output, theme, *ok)
            } else if *ok {
                if summary.is_empty() {
                    Vec::new()
                } else {
                    connector_lines(
                        vec![summary.clone()],
                        Style::default().fg(theme.dim).add_modifier(Modifier::ITALIC),
                        theme,
                    )
                }
            } else {
                let tail: Vec<String> = if full_output.trim().is_empty() {
                    vec![summary.clone()]
                } else {
                    full_output
                        .trim_end()
                        .lines()
                        .take(3)
                        .map(String::from)
                        .collect()
                };
                connector_lines(tail, Style::default().fg(theme.diff_removed), theme)
            }
        }
    }
}
```

`expanded_output_lines` — re-prefix through `connector_lines` (keep `EXPANDED_LINE_CAP`, the `… +N more lines` hint at 4-space indent, and the image-chip branch untouched):

```rust
fn expanded_output_lines(full_output: &str, theme: &Theme, ok: bool) -> Vec<Line<'static>> {
    let trimmed = full_output.trim_end_matches('\n');
    if trimmed.is_empty() {
        return connector_lines(
            vec!["(no output)".to_string()],
            Style::default().fg(theme.dim).add_modifier(Modifier::ITALIC),
            theme,
        );
    }
    if let Some(marker) = parse_image_marker(trimmed) {
        return image_marker_lines(&marker, theme);
    }
    let all: Vec<&str> = trimmed.lines().collect();
    let total = all.len();
    let body_style = Style::default().fg(if ok { theme.fg } else { theme.diff_removed });
    let mut out = connector_lines(
        all.iter().take(EXPANDED_LINE_CAP).map(|s| s.to_string()).collect(),
        body_style,
        theme,
    );
    if total > EXPANDED_LINE_CAP {
        out.push(Line::from(Span::styled(
            format!("    … +{} more lines", total - EXPANDED_LINE_CAP),
            Style::default().fg(theme.dim).add_modifier(Modifier::ITALIC),
        )));
    }
    out
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib tui::ui::transcript::tests`
Expected: PASS.

- [ ] **Step 5: Full suite**

Run: `cargo test --lib`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/tui/ui/transcript.rs
git commit -m "tui(codex): verb+bullet tool cards with └ connectors and collapsed output"
```

---

### Task 4: Diff tinting in the approval preview

**Files:**
- Modify: `src/tui/theme.rs` (add `diff_add_bg`, `diff_del_bg`)
- Modify: `src/tui/ui/overlays.rs` (replace `diff_line_to_styled` at :119 with `styled_diff_line`)

**Interfaces:**
- Consumes: `policy::render_unified_diff` output format (`    + body` / `    - body` / `      body`).
- Produces: `styled_diff_line(&str, width: u16, &Theme) -> Line<'static>` — consumed by Task 5's approval pane.

- [ ] **Step 1: Write the failing tests**

New tests mod at the bottom of `src/tui/ui/overlays.rs` (none exists today):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn added_line_gets_tinted_full_width() {
        let theme = Theme::dark();
        let line = styled_diff_line("    + hello", 20, &theme);
        assert_eq!(line_text(&line).chars().count(), 20);
        assert!(line.spans.iter().all(|s| s.style.bg == Some(theme.diff_add_bg)));
        assert_eq!(line.spans[0].style.fg, Some(theme.diff_added));
    }

    #[test]
    fn removed_line_gets_del_tint() {
        let theme = Theme::dark();
        let line = styled_diff_line("    - gone", 20, &theme);
        assert!(line.spans.iter().all(|s| s.style.bg == Some(theme.diff_del_bg)));
        assert_eq!(line.spans[0].style.fg, Some(theme.diff_removed));
    }

    #[test]
    fn context_line_stays_dim_without_bg() {
        let theme = Theme::dark();
        let line = styled_diff_line("      same", 20, &theme);
        assert!(line.spans.iter().all(|s| s.style.bg.is_none()));
        assert_eq!(line.spans[0].style.fg, Some(theme.dim));
    }

    #[test]
    fn light_theme_uses_github_pastels() {
        assert_eq!(Theme::light().diff_add_bg, Color::Rgb(0xda, 0xfb, 0xe1));
        assert_eq!(Theme::light().diff_del_bg, Color::Rgb(0xff, 0xeb, 0xe9));
        assert_eq!(Theme::dark().diff_add_bg, Color::Rgb(0x21, 0x3a, 0x2b));
        assert_eq!(Theme::dimmed().diff_add_bg, Color::Rgb(0x1f, 0x2a, 0x1f));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib tui::ui::overlays::tests`
Expected: FAIL — `styled_diff_line` and the theme fields don't exist.

- [ ] **Step 3: Implement**

`src/tui/theme.rs` — add to `Theme` (after `diff_removed`):

```rust
    pub diff_add_bg: Color,
    pub diff_del_bg: Color,
```

Preset values — dark: `diff_add_bg: Color::Rgb(0x21, 0x3a, 0x2b)`, `diff_del_bg: Color::Rgb(0x4a, 0x22, 0x1d)`; light: `Color::Rgb(0xda, 0xfb, 0xe1)` / `Color::Rgb(0xff, 0xeb, 0xe9)`; dimmed: `Color::Rgb(0x1f, 0x2a, 0x1f)` / `Color::Rgb(0x2a, 0x1f, 0x1f)`.

`src/tui/ui/overlays.rs` — replace `diff_line_to_styled` (and its use in `draw_approval_modal`; Task 5 replaces that caller):

```rust
/// Codex-style diff row: sign-colored fg over a full-width bg tint
/// for +/- lines; context lines stay dim and untinted. `width` pads
/// the tint to the pane's inner width. Input is
/// `policy::render_unified_diff`'s `    + body` / `    - body` /
/// `      body` format.
pub(super) fn styled_diff_line(line: &str, width: u16, theme: &Theme) -> Line<'static> {
    let trimmed = line.trim_start();
    let (body_style, bg) = if trimmed.starts_with("+ ") {
        (Style::default().fg(theme.diff_added), Some(theme.diff_add_bg))
    } else if trimmed.starts_with("- ") {
        (Style::default().fg(theme.diff_removed), Some(theme.diff_del_bg))
    } else {
        (Style::default().fg(theme.dim), None)
    };
    let pad = (width as usize).saturating_sub(line.chars().count());
    match bg {
        Some(bg) => Line::from(vec![
            Span::styled(line.to_string(), body_style.bg(bg)),
            Span::styled(" ".repeat(pad), Style::default().bg(bg)),
        ]),
        None => Line::from(Span::styled(line.to_string(), body_style)),
    }
}
```

Keep `draw_approval_modal` compiling by mapping its preview loop to `styled_diff_line(l, inner.width, theme)` until Task 5 replaces it.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib tui::ui::overlays::tests`
Expected: PASS.

- [ ] **Step 5: Full suite**

Run: `cargo test --lib`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/tui/theme.rs src/tui/ui/overlays.rs
git commit -m "tui(codex): full-width bg tints for diff rows"
```

---

### Task 5: Inline approval pane + decision cells + overlay accent pass

**Files:**
- Modify: `src/tui/app/overlay.rs` (`ApprovalState.selected`, `APPROVAL_OPTIONS`, select methods)
- Modify: `src/tui/app/transcript.rs` (`note_approval_decision` — lives here, NOT in overlay.rs: `note_arrival` is a private method of the `app::transcript` module and siblings can't call it)
- Modify: `src/tui/app/tests.rs` (App-level approval tests)
- Modify: `src/tui/event.rs` (`approval_response_for`)
- Modify: `src/tui/mod.rs` (`handle_approval_key` at :1088 — Up/Down/Enter, decision cell)
- Modify: `src/tui/ui/mod.rs` (`draw` approval branch; `draw_completion_popup` accent + rounded border, drop title)
- Modify: `src/tui/ui/overlays.rs` (replace `draw_approval_modal` with `approval_pane_lines` + `draw_approval_pane`; accent pass on sessions picker, help browser, history search, inline help, copy fallback, wizard; delete the 💡 legend; extend the `ratatui::widgets` import with `Padding` and `BorderType`)

**Interfaces:**
- Consumes: `styled_diff_line` (Task 4), `ApprovalResponse` (`event.rs:133` — `Yes, No, AlwaysAllow, AlwaysDeny, PersistAllow`, `Copy`).
- Produces: `APPROVAL_OPTIONS: [(&str, &str); 5]`; `approval_response_for(usize) -> ApprovalResponse`; `approval_pane_lines(&ApprovalState, preview_rows: usize, inner_w: u16, &Theme) -> Vec<Line<'static>>`; `App::note_approval_decision(&ApprovalResponse)`.

- [ ] **Step 1: Write the failing tests**

`src/tui/app/tests.rs` (append; add `use crate::tui::app::overlay::APPROVAL_OPTIONS;`-adjacent imports as needed — `ApprovalResponse` and `approval_response_for` come from `crate::tui::event`):

```rust
    #[test]
    fn approval_selection_clamps_at_both_ends() {
        let mut app = App::new();
        app.on_approval_requested("Bash".into(), serde_json::json!({"command":"ls"}), "run".into());
        app.approval_select_prev();
        assert_eq!(app.approval().unwrap().selected, 0);
        for _ in 0..10 {
            app.approval_select_next();
        }
        assert_eq!(app.approval().unwrap().selected, 4);
    }

    #[test]
    fn approval_response_for_maps_list_order() {
        assert!(matches!(approval_response_for(0), ApprovalResponse::Yes));
        assert!(matches!(approval_response_for(1), ApprovalResponse::No));
        assert!(matches!(approval_response_for(2), ApprovalResponse::AlwaysAllow));
        assert!(matches!(approval_response_for(3), ApprovalResponse::PersistAllow));
        assert!(matches!(approval_response_for(4), ApprovalResponse::AlwaysDeny));
    }

    #[test]
    fn decision_cell_lands_in_transcript_with_glyph() {
        let mut app = App::new();
        app.on_approval_requested("Bash".into(), serde_json::json!({"command":"ls"}), "run".into());
        app.note_approval_decision(&ApprovalResponse::Yes);
        let last = app.transcript.last().unwrap();
        match last {
            TranscriptItem::System { body } => {
                assert!(body.starts_with("✔ "), "got: {body}");
                assert!(body.contains("Bash"), "got: {body}");
            }
            other => panic!("expected System, got {other:?}"),
        }
        app.note_approval_decision(&ApprovalResponse::AlwaysDeny);
        match app.transcript.last().unwrap() {
            TranscriptItem::System { body } => assert!(body.starts_with("✗ "), "got: {body}"),
            _ => panic!(),
        }
    }
```

`src/tui/ui/overlays.rs` tests:

```rust
    #[test]
    fn approval_pane_lines_render_title_options_and_selection() {
        let theme = Theme::dark();
        let state = ApprovalState {
            tool: "Bash".into(),
            reason: "run it".into(),
            preview: "    ls -la".into(),
            scroll: 0,
            selected: 1,
        };
        let lines = approval_pane_lines(&state, 5, 60, &theme);
        let text: Vec<String> = lines.iter().map(line_text).collect();
        assert!(text[0].contains("Would you like to run the following command?"));
        assert!(text.iter().any(|l| l == "› 2. No (n)"));
        assert!(text.iter().any(|l| l == "  1. Yes (y)"));
        assert!(text.iter().any(|l| l.contains("Press enter to confirm or esc to cancel")));
        let selected = lines.iter().find(|l| line_text(l).starts_with("› ")).unwrap();
        assert_eq!(selected.spans[0].style.fg, Some(theme.accent));
    }

    #[test]
    fn approval_pane_uses_edit_title_for_edit_tools() {
        let theme = Theme::dark();
        let state = ApprovalState {
            tool: "Edit".into(),
            reason: String::new(),
            preview: "    + x".into(),
            scroll: 0,
            selected: 0,
        };
        let lines = approval_pane_lines(&state, 5, 60, &theme);
        assert!(line_text(&lines[0]).contains("make the following edits"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib tui::`
Expected: FAIL — `selected`, `approval_select_*`, `approval_response_for`, `note_approval_decision`, `approval_pane_lines` don't exist.

- [ ] **Step 3: Implement**

`src/tui/app/overlay.rs`:

```rust
#[derive(Debug, Clone)]
pub struct ApprovalState {
    pub tool: String,
    pub reason: String,
    pub preview: String,
    pub scroll: u16,
    /// Cursor into `APPROVAL_OPTIONS` for the inline list.
    pub selected: usize,
}

/// `(label, key)` — the five approval responses in list order.
pub const APPROVAL_OPTIONS: [(&str, &str); 5] = [
    ("Yes", "y"),
    ("No", "n"),
    ("Allow for this session", "a"),
    ("Allow always, persisted", "A"),
    ("Deny for this session", "d"),
];
```

`on_approval_requested`: add `selected: 0,`. Add the select methods to the `impl App` block in overlay.rs:

```rust
    pub fn approval_select_prev(&mut self) {
        if let Some(a) = self.approval_mut() {
            a.selected = a.selected.saturating_sub(1);
        }
    }

    pub fn approval_select_next(&mut self) {
        if let Some(a) = self.approval_mut() {
            a.selected = (a.selected + 1).min(APPROVAL_OPTIONS.len() - 1);
        }
    }
```

`src/tui/app/transcript.rs` — the decision-cell method goes in the existing `impl App` block (it needs `note_arrival`, which is private to this module):

```rust
    /// Land the verdict in the transcript as a `✔`/`✗` cell before
    /// the pane closes (the System renderer colors the glyph).
    pub fn note_approval_decision(&mut self, resp: &crate::tui::event::ApprovalResponse) {
        use crate::tui::event::ApprovalResponse as R;
        let Some(tool) = self.approval().map(|a| a.tool.clone()) else {
            return;
        };
        let (glyph, verdict) = match resp {
            R::Yes => ("✔", format!("You approved {} this time", tool)),
            R::No => ("✗", format!("You did not approve {}", tool)),
            R::AlwaysAllow => ("✔", format!("You approved {} for this session", tool)),
            R::PersistAllow => ("✔", format!("You approved {} always", tool)),
            R::AlwaysDeny => ("✗", format!("You denied {} for this session", tool)),
        };
        self.transcript.push(TranscriptItem::System {
            body: format!("{} {}", glyph, verdict),
        });
        self.note_arrival(1);
    }
```

`src/tui/event.rs` (after the `ApprovalResponse` enum):

```rust
/// Map the inline approval list cursor to a response. Order must
/// match `app::APPROVAL_OPTIONS`.
pub fn approval_response_for(index: usize) -> ApprovalResponse {
    match index {
        0 => ApprovalResponse::Yes,
        1 => ApprovalResponse::No,
        2 => ApprovalResponse::AlwaysAllow,
        3 => ApprovalResponse::PersistAllow,
        _ => ApprovalResponse::AlwaysDeny,
    }
}
```

`src/tui/mod.rs` `handle_approval_key` — add selection keys and the decision cell (keep every existing single-key shortcut; PgUp/PgDn still scroll):

```rust
        KeyCode::Up => {
            app.approval_select_prev();
            None
        }
        KeyCode::Down => {
            app.approval_select_next();
            None
        }
        KeyCode::Enter => {
            let idx = app.approval().map(|a| a.selected).unwrap_or(0);
            Some(crate::tui::event::approval_response_for(idx))
        }
```

and in the `if let Some(resp) = response` block, before `tx.send` / `app.close_approval()`:

```rust
        app.note_approval_decision(&resp);
```

`src/tui/ui/overlays.rs` — extend the widgets import (`use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph, Wrap};`) and add `use ratatui::widgets::BorderType;`. Delete `draw_approval_modal` and the 💡 legend; add:

```rust
/// Height of the inline approval pane for the given preview.
/// Preview is capped at 10 rows; the pane never eats the whole
/// frame below 3 transcript rows.
pub(super) fn approval_pane_height(state: &ApprovalState, area: Rect) -> u16 {
    let preview_rows = state.preview.lines().count().clamp(3, 10) as u16;
    let reason_rows = u16::from(!state.reason.is_empty());
    // title + blank + reason? + preview + blank + options + blank + hint
    let want = 2 + reason_rows + preview_rows + 1 + APPROVAL_OPTIONS.len() as u16 + 2;
    want.min(area.height.saturating_sub(3)).max(10)
}

/// Pure line builder for the inline approval pane (Codex shape):
/// bold question, optional italic reason, tinted scrollable
/// preview, numbered options with `›` selection, dim confirm hint.
pub(super) fn approval_pane_lines(
    state: &ApprovalState,
    preview_rows: usize,
    inner_w: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let title = match state.tool.to_ascii_lowercase().as_str() {
        "edit" | "multiedit" | "write" => "Would you like to make the following edits?",
        _ => "Would you like to run the following command?",
    };
    let mut lines: Vec<Line<'static>> = vec![
        Line::from(Span::styled(
            format!(" {}", title),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
    ];
    if !state.reason.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(" Reason: ".to_string(), Style::default().fg(theme.dim)),
            Span::styled(
                state.reason.clone(),
                Style::default().add_modifier(Modifier::ITALIC),
            ),
        ]));
    }
    let total = state.preview.lines().count();
    let scroll = (state.scroll as usize).min(total.saturating_sub(preview_rows));
    for l in state.preview.lines().skip(scroll).take(preview_rows) {
        lines.push(styled_diff_line(l, inner_w, theme));
    }
    lines.push(Line::raw(""));
    for (i, (label, key)) in APPROVAL_OPTIONS.iter().enumerate() {
        let selected = i == state.selected;
        let prefix = if selected { "› " } else { "  " };
        let style = if selected {
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg)
        };
        lines.push(Line::from(Span::styled(
            format!("{}{}. {} ({})", prefix, i + 1, label, key),
            style,
        )));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "  Press enter to confirm or esc to cancel".to_string(),
        Style::default().fg(theme.dim).add_modifier(Modifier::ITALIC),
    )));
    lines
}

/// Inline bottom-pane approval (replaces the old centered modal).
pub(super) fn draw_approval_pane(f: &mut Frame, area: Rect, approval: &ApprovalState, app: &App) {
    let preview_rows = (area.height as usize)
        .saturating_sub(2 + usize::from(!approval.reason.is_empty()) + 1 + APPROVAL_OPTIONS.len() + 2)
        .max(1);
    let inner_w = area.width.saturating_sub(2);
    let lines = approval_pane_lines(approval, preview_rows, inner_w, &app.theme);
    let pane = Paragraph::new(lines).block(Block::default().padding(Padding::horizontal(1)));
    f.render_widget(pane, area);
}
```

`src/tui/ui/mod.rs` `draw` — approval gets its own layout, before the normal one:

```rust
pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    if let Some(crate::tui::app::Overlay::Approval(state)) = &app.overlay {
        let h = overlays::approval_pane_height(state, area);
        let chunks =
            Layout::vertical([Constraint::Min(3), Constraint::Length(h)]).split(area);
        transcript::draw_transcript(f, chunks[0], app);
        overlays::draw_approval_pane(f, chunks[1], state, app);
        return;
    }
    // … existing layout, with the Overlay::Approval arm removed from
    // the overlay match.
}
```

`src/tui/mod.rs` — delete the 💡/`mark_hint_shown(hints::ids::APPROVAL_ALLOW)` block in `handle_approval_key` (the new pane's options are self-explanatory).

Accent pass — mechanical, same three substitutions in `draw_sessions_picker`, `draw_help_browser`, `draw_history_search`, `draw_inline_help`, `draw_copy_fallback`, `draw_wizard` (all in `src/tui/ui/overlays.rs`) and `draw_completion_popup` (`src/tui/ui/mod.rs:798`):

1. Selection style `Style::default().fg(theme.selected_fg).bg(theme.selected_bg).add_modifier(Modifier::BOLD)` → `Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)`.
2. Selected-row prefix `"▌ "` → `"› "`.
3. Box borders → dim rounded: `.border_type(ratatui::widgets::BorderType::Rounded).border_style(Style::default().fg(theme.dim))`; drop the completion popup's `" complete "` title.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib tui::`
Expected: PASS.

- [ ] **Step 5: Full suite**

Run: `cargo test --lib`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/tui/app/overlay.rs src/tui/event.rs src/tui/mod.rs src/tui/ui/mod.rs src/tui/ui/overlays.rs
git commit -m "tui(codex): inline approval pane with › selection, decision cells, overlay accent pass"
```

---

### Task 6: Welcome header + turn separators

**Files:**
- Modify: `src/tui/app/transcript.rs` (`TranscriptItem::Welcome` / `TurnRule`, `is_final`, `App.turn_started_at`, `on_turn_finished`, `on_turn_started`)
- Modify: `src/tui/app/mod.rs` (`App::new` at :231, `turn_started_at` field + `Default`)
- Modify: `src/tui/ui/transcript.rs` (`render_welcome`, `welcome_row`, `turn_separator` + match arms, tests)

**Interfaces:**
- Consumes: `fmt_elapsed` (Task 1), `clip_to_width` (ui/mod.rs:91), `app.status.{model,cwd}` (Task 1).
- Produces: `TranscriptItem::Welcome`, `TranscriptItem::TurnRule { elapsed: Duration }` — both `is_final() == true`.

- [ ] **Step 1: Write the failing tests**

`src/tui/ui/transcript.rs` tests:

```rust
    #[test]
    fn welcome_renders_rounded_box_with_identity_and_tips() {
        let mut app = empty_app();
        app.set_status(crate::tui::app::StatusModel {
            model: "kimi-k3".into(),
            cwd: "~/dev/oli".into(),
            ..Default::default()
        });
        app.transcript.push(TranscriptItem::Welcome);
        let lines = build_transcript_lines(&app, 80);
        let text: Vec<String> = lines.iter().map(line_text).collect();
        assert!(text.iter().any(|l| l.starts_with('╭') && l.ends_with('╮')));
        assert!(text.iter().any(|l| l.starts_with('╰') && l.ends_with('╯')));
        assert!(text.iter().any(|l| l.contains("oli (v") && l.contains('│')));
        assert!(text.iter().any(|l| l.contains("model: kimi-k3")));
        assert!(text.iter().any(|l| l.contains("directory: ~/dev/oli")));
        assert!(text.iter().any(|l| l.contains("/help")));
        assert!(text.iter().any(|l| l.contains("/sessions")));
    }

    #[test]
    fn welcome_box_inner_width_caps_at_56() {
        let mut app = empty_app();
        app.transcript.push(TranscriptItem::Welcome);
        let lines = build_transcript_lines(&app, 200);
        let top = lines
            .iter()
            .find(|l| line_text(l).starts_with('╭'))
            .expect("top border");
        assert_eq!(line_text(top).chars().count(), 58);
    }

    #[test]
    fn turn_rule_emitted_after_tool_turn_but_not_chat_turn() {
        let mut app = empty_app();
        app.on_turn_started();
        app.on_content_chunk("just chatting");
        app.on_turn_finished("");
        let lines = build_transcript_lines(&app, 40);
        assert!(!lines.iter().any(|l| line_text(l).contains('─')));

        app.on_turn_started();
        app.on_tool_start(1, "Bash".into(), "ls".into());
        app.on_tool_done(1, Duration::from_millis(5), "ok".into(), true, String::new());
        app.on_turn_finished("");
        let lines = build_transcript_lines(&app, 40);
        let rule = lines
            .iter()
            .find(|l| line_text(l).contains('─'))
            .expect("rule after tool turn");
        assert_eq!(line_text(rule).chars().count(), 40);
    }

    #[test]
    fn turn_separator_embeds_worked_for_over_a_minute() {
        let theme = Theme::dark();
        let line = turn_separator(40, Duration::from_secs(95), &theme);
        let text = line_text(&line);
        assert!(text.starts_with("─ Worked for 1m 35s "), "got: {text}");
        assert_eq!(text.chars().count(), 40);
        let short = turn_separator(40, Duration::from_secs(12), &theme);
        assert!(!line_text(&short).contains("Worked for"));
    }
```

`src/tui/app/tests.rs` (append; `committable_count` and `TranscriptItem` are re-exported from `crate::tui::app`, add `use std::time::Duration;` if missing):

```rust
    #[test]
    fn welcome_and_turn_rule_are_immediately_committable() {
        let items = vec![
            TranscriptItem::Welcome,
            TranscriptItem::TurnRule { elapsed: Duration::ZERO },
        ];
        assert_eq!(committable_count(&items, 0), 2);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib tui::`
Expected: FAIL — `TranscriptItem::Welcome`/`TurnRule`, `turn_separator`, `render_welcome` don't exist.

- [ ] **Step 3: Implement**

`src/tui/app/transcript.rs`:

```rust
#[derive(Debug, Clone)]
pub enum TranscriptItem {
    UserPrompt {
        body: String,
    },
    Assistant {
        body: String,
        done: bool,
    },
    System {
        body: String,
    },
    ToolCard {
        tool: String,
        args_preview: String,
        state: ToolCardState,
    },
    /// Startup splash: rounded box + tips. Rendered from
    /// `app.status` (model, cwd) + the crate version.
    Welcome,
    /// Dim full-width rule emitted when a turn that ran at least one
    /// tool finishes; carries the wall-clock turn duration.
    TurnRule {
        elapsed: Duration,
    },
}
```

`is_final`: add `TranscriptItem::Welcome | TranscriptItem::TurnRule { .. } => true,` to the first match arm.

`on_turn_started` (:87): add `self.turn_started_at = Some(Instant::now());`.

`on_turn_finished` (:264) — append the rule when the turn did real work:

```rust
    pub fn on_turn_finished(&mut self, _final_content: &str) {
        if let Some(idx) = self.active_assistant.take() {
            if let Some(TranscriptItem::Assistant { done, .. }) = self.transcript.get_mut(idx) {
                *done = true;
            }
        }
        // Codex-style separator: only turns that did real work
        // (≥ 1 tool card since the last user prompt) earn the rule.
        if self.turn_since_last_prompt_ran_tools() {
            let elapsed = self.turn_started_at.map(|t| t.elapsed()).unwrap_or_default();
            self.transcript.push(TranscriptItem::TurnRule { elapsed });
            self.note_arrival(1);
        }
        self.mode = Mode::Idle;
        self.cancel_tx = None;
    }

    /// True when the closing turn emitted at least one tool card
    /// since the most recent user prompt.
    fn turn_since_last_prompt_ran_tools(&self) -> bool {
        self.transcript
            .iter()
            .rev()
            .take_while(|i| !matches!(i, TranscriptItem::UserPrompt { .. }))
            .any(|i| matches!(i, TranscriptItem::ToolCard { .. }))
    }
```

`src/tui/app/mod.rs` — add field `pub turn_started_at: Option<std::time::Instant>` (+ `None` in `Default`); `App::new` pushes the splash instead of the banner:

```rust
        app.transcript.push(TranscriptItem::Welcome);
```

`src/tui/ui/transcript.rs` — new match arms and helpers:

```rust
            TranscriptItem::Welcome => lines.extend(render_welcome(app, rule_width)),
            TranscriptItem::TurnRule { elapsed } => {
                lines.push(turn_separator(rule_width, *elapsed, theme))
            }
```

```rust
/// Codex-style startup splash: dim rounded box (`>_ oli (v…)` +
/// model + directory) followed by three tip lines. Box inner width
/// clamps to [20, 56] so wide terminals don't stretch it.
fn render_welcome(app: &App, width: u16) -> Vec<Line<'static>> {
    let theme = &app.theme;
    let inner = width.saturating_sub(2).clamp(20, 56) as usize;
    let dim = Style::default().fg(theme.dim);
    let mut out: Vec<Line<'static>> = Vec::new();
    out.push(Line::from(Span::styled(
        format!("╭{}╮", "─".repeat(inner)),
        dim,
    )));
    out.push(welcome_row(
        vec![
            Span::styled(">_ ".to_string(), dim),
            Span::styled("oli".to_string(), Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(format!(" (v{})", env!("CARGO_PKG_VERSION")), dim),
        ],
        inner,
        theme,
    ));
    out.push(welcome_row(vec![Span::raw("")], inner, theme));
    let model = if app.status.model.is_empty() {
        "unknown".to_string()
    } else {
        app.status.model.clone()
    };
    out.push(welcome_row(
        vec![
            Span::styled("model: ".to_string(), dim),
            Span::styled(model, Style::default().fg(theme.fg)),
            Span::styled("  ".to_string(), dim),
            Span::styled("/model".to_string(), Style::default().fg(theme.accent)),
            Span::styled(" to change".to_string(), dim),
        ],
        inner,
        theme,
    ));
    let cwd = super::clip_to_width(&app.status.cwd, inner.saturating_sub(11).max(1));
    out.push(welcome_row(
        vec![
            Span::styled("directory: ".to_string(), dim),
            Span::styled(cwd, Style::default().fg(theme.fg)),
        ],
        inner,
        theme,
    ));
    out.push(Line::from(Span::styled(
        format!("╰{}╯", "─".repeat(inner)),
        dim,
    )));
    out.push(Line::raw(""));
    out.push(Line::from(Span::styled(
        "  To get started, describe a task or try one of these commands:".to_string(),
        dim,
    )));
    let tips: [(&str, &str); 3] = [
        ("/help", "show key bindings and commands"),
        ("/sessions", "resume a previous session"),
        ("/paths", "resolved config and data locations"),
    ];
    for (cmd, desc) in tips {
        out.push(Line::from(vec![
            Span::styled(format!("  {:<10}", cmd), Style::default().fg(theme.fg)),
            Span::styled(format!("- {}", desc), dim),
        ]));
    }
    out
}

/// One box row: `│` + spans padded to `inner` cols + `│`.
fn welcome_row(spans: Vec<Span<'static>>, inner: usize, theme: &Theme) -> Line<'static> {
    let dim = Style::default().fg(theme.dim);
    let text_w: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let mut all = vec![Span::styled("│".to_string(), dim)];
    all.extend(spans);
    all.push(Span::raw(" ".repeat(inner.saturating_sub(text_w))));
    all.push(Span::styled("│".to_string(), dim));
    Line::from(all)
}

/// Dim full-width rule; turns over a minute embed `Worked for`.
fn turn_separator(width: u16, elapsed: Duration, theme: &Theme) -> Line<'static> {
    let w = width.max(1) as usize;
    let style = Style::default().fg(theme.dim);
    if elapsed.as_secs() > 60 {
        let label = format!("─ Worked for {} ", super::fmt_elapsed(elapsed.as_secs()));
        let fill = w.saturating_sub(label.chars().count());
        Line::from(Span::styled(
            format!("{}{}", label, "─".repeat(fill)),
            style,
        ))
    } else {
        Line::from(Span::styled("─".repeat(w), style))
    }
}
```

Add `use std::time::Duration;` to transcript.rs if not already imported (tests use `Duration`; the renderer now needs it too).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib tui::`
Expected: PASS.

- [ ] **Step 5: Full suite + real-terminal checklist**

Run: `cargo test --lib && cargo build`
Expected: PASS. Then `cargo run` in a real terminal and walk the checklist:

- [ ] Welcome box renders, footer shows `? for shortcuts · cwd · branch · model` + gauge
- [ ] Send a prompt: banded `›` user cell, `•` assistant, `Working` status row
- [ ] Run a tool (ask oli to grep something): spinner card → collapses to `• Searched …` + dim summary; turn rule appears
- [ ] Trigger an approval (ask it to edit a file): inline pane, arrows + enter work, `✔` cell lands
- [ ] `?` opens help; `/sessions` picker shows `›` selection
- [ ] Narrow terminal (~70 cols): footer drops shortcuts first

- [ ] **Step 6: Commit**

```bash
git add src/tui/app/transcript.rs src/tui/app/mod.rs src/tui/ui/transcript.rs
git commit -m "tui(codex): welcome splash box + worked-for turn separators"
```

---

## Self-Review Notes

- **Spec coverage:** shell (T1), message cells (T2), tool cells (T3), diffs (T4), approval + accent (T5), welcome + separators (T6). Theme fields land in T1 (`user_band_bg`) and T4 (diff tints) — the task that first needs each, per right-sizing.
- **Known intermediate state:** Task 1 references `theme.user_band_bg` before Task 2 consumes it; the field + presets are added in Task 1 (see its Step 3 note) so every commit compiles.
- **Type consistency:** `styled_diff_line(&str, u16, &Theme)` (T4) is the exact signature consumed by `approval_pane_lines` (T5). `fmt_elapsed(u64) -> String` (T1) is reused by `turn_separator` (T6). `approval_response_for` order matches `APPROVAL_OPTIONS` (T5).
