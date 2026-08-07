//! Per-frame render. Lays out the four-band shell:
//! - **transcript** (flex top): welcome splash, `›`-banded user
//!   prompts, `•`-bulleted assistant messages, tool cards, and
//!   system notices. In-flight state lives in the status row, not
//!   an inline cursor.
//! - **status row** (2 rows): spinner + elapsed while busy, a
//!   tool-detail line while a tool runs; blank when idle.
//! - **composer** (flex bottom, capped): borderless multi-line
//!   tui-textarea on a tinted band, inset from all four band edges.
//! - **footer** (1 row): identity left, context gauge right.
//!
//! Approval takes over the bottom pane (transcript + inline
//! approval) instead of floating a modal.
//!
//! Rendering is intentionally re-derived from `App` on every draw —
//! there's no diff machinery. ratatui handles the on-screen diff
//! against the previous frame.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Wrap};

use transcript::TRANSCRIPT_H_PAD;

use crate::tui::app::{App, Mode, ToolCardState, TranscriptItem};

mod overlays;
mod transcript;

/// Blank band rows above and below the composer text, so the input
/// doesn't sit flush against the status row or the footer.
const COMPOSER_V_PAD: u16 = 1;
/// Cols reserved left of the composer text: `TRANSCRIPT_H_PAD`, the
/// `›` glyph, and a space. Matches the transcript's user-turn indent
/// so typed text lines up with the prompt it becomes.
const COMPOSER_GUTTER: u16 = TRANSCRIPT_H_PAD + 2;

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();

    // Approval owns the bottom pane instead of floating a modal: the
    // transcript keeps the rest of the frame so the user can still
    // read context above the decision.
    if let Some(crate::tui::app::Overlay::Approval(state)) = &app.overlay {
        // Clone to release the immutable borrow of `app.overlay` so the
        // transcript/pane draws (which take `app`) can proceed.
        let state = state.clone();
        let h = overlays::approval_pane_height(&state, area);
        let chunks = Layout::vertical([Constraint::Min(3), Constraint::Length(h)]).split(area);
        transcript::draw_transcript(f, chunks[0], app);
        overlays::draw_approval_pane(f, chunks[1], &state, app);
        return;
    }

    let input_lines = app.input.lines().len().max(1).min(8) as u16;
    let input_height = input_lines + COMPOSER_V_PAD * 2;
    let chunks = Layout::vertical([
        Constraint::Min(3),               // transcript
        Constraint::Length(2),            // status row (+ tool detail)
        Constraint::Length(input_height), // composer (borderless, padded)
        Constraint::Length(1),            // footer
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
        // Approval is handled above as a bottom pane, not a modal.
        Some(Overlay::Approval(_)) => {}
        Some(Overlay::SessionsPicker(s)) => overlays::draw_sessions_picker(f, area, s, &app.theme),
        Some(Overlay::ModelPicker(s)) => {
            overlays::draw_model_picker(f, area, s, &app.status.model, &app.theme)
        }
        Some(Overlay::HelpBrowser(s)) => overlays::draw_help_browser(f, area, s, &app.theme),
        Some(Overlay::InlineHelp(s)) => overlays::draw_inline_help(f, area, s, &app.theme),
        Some(Overlay::HistorySearch(s)) => overlays::draw_history_search(f, area, s, app),
        Some(Overlay::CopyFallback(s)) => overlays::draw_copy_fallback(f, area, s, &app.theme),
        Some(Overlay::Wizard(s)) => overlays::draw_wizard(f, area, s, &app.theme),
        Some(Overlay::Search(_)) | None => {}
    }
}

/// Inline mode: build the lines + exact wrapped height for flushing
/// transcript items `range` to native scrollback via
/// `Terminal::insert_before`. Rendered at full width with no block
/// padding so `Paragraph::line_count(width)` matches the eventual
/// render wrap exactly — the scrollback buffer is sized to the row and
/// leaves neither trailing blank gaps nor clipped lines. Returns
/// `(lines, height)`; the caller renders the same `lines` with the
/// same `Wrap` inside the `insert_before` closure.
pub fn committed_scrollback(
    app: &App,
    width: u16,
    range: std::ops::Range<usize>,
) -> (Vec<Line<'static>>, u16) {
    let lines = transcript::build_transcript_lines_range(app, width, range);
    let height = Paragraph::new(lines.clone())
        .wrap(Wrap { trim: false })
        .line_count(width) as u16;
    (lines, height)
}

/// Truncate `s` to `max` Unicode characters, appending `…` when
/// it's clipped. Helper shared with the overlay drawers.
pub(super) fn clip_to_width(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Centered rectangle within `r`, sized as a percentage. Clamped
/// in both dimensions so the modal doesn't shrink past readability
/// or balloon past usefulness.
pub(super) fn centered_rect(r: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let target_w = ((r.width as u32 * percent_x as u32) / 100).clamp(60, 120) as u16;
    let target_h = ((r.height as u32 * percent_y as u32) / 100).clamp(10, 40) as u16;
    let w = target_w.min(r.width);
    let h = target_h.min(r.height);
    let x = r.x + r.width.saturating_sub(w) / 2;
    let y = r.y + r.height.saturating_sub(h) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|s| s.content.chars().count()).sum()
}

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
    let elapsed = since.elapsed();
    let secs = elapsed.as_secs();
    let theme = &app.theme;
    let status = Line::from(vec![
        Span::styled(
            // Sub-second time so the spinner cycles ~10fps. Feeding
            // whole seconds here would pin it to frame 0 forever.
            format!("{} ", spinner_glyph(elapsed.as_secs_f32())),
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
        format!(
            "{}h {:02}m {:02}s",
            secs / 3600,
            (secs % 3600) / 60,
            secs % 60
        )
    }
}

/// `12345 → 12.3k`, `200000 → 200k`. Keeps the gauge compact
/// without a generic humanize crate.
fn format_count(n: u32) -> String {
    if n < 10_000 {
        n.to_string()
    } else if n < 1_000_000 {
        let v = n as f32 / 1_000.0;
        if v >= 100.0 {
            format!("{:.0}k", v)
        } else {
            format!("{:.1}k", v)
        }
    } else {
        format!("{:.1}m", n as f32 / 1_000_000.0)
    }
}

/// Search bar — swaps in for the status row while the Search
/// overlay is active. Left side shows the query with a cursor; right
/// side shows match counter + key legend.
fn draw_search_bar(f: &mut Frame, area: Rect, app: &App) {
    let left = render_search_bar_left(app);
    let right = render_search_bar_right(app);
    let left_w = spans_width(&left) as u16;
    let right_w = spans_width(&right) as u16;
    let inner_width = area.width.saturating_sub(TRANSCRIPT_H_PAD * 2);
    let mut spans = left;
    let pad = inner_width.saturating_sub(left_w + right_w);
    if pad > 0 {
        spans.push(Span::raw(" ".repeat(pad as usize)));
    }
    spans.extend(right);
    let bar = Paragraph::new(Line::from(spans))
        .block(Block::default().padding(Padding::horizontal(TRANSCRIPT_H_PAD)));
    f.render_widget(bar, area);
}

/// Left half of the search bar — `🔍 <query>▏`. Returns no-op
/// spans (single space) if the overlay isn't active, so call sites
/// can rely on the function being total.
pub(super) fn render_search_bar_left(app: &App) -> Vec<Span<'static>> {
    let Some(state) = app.search() else {
        return Vec::new();
    };
    let theme = &app.theme;
    vec![
        Span::styled(
            " 🔍 ".to_string(),
            Style::default()
                .fg(theme.match_highlight)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(state.query.clone(), Style::default().fg(theme.fg)),
        Span::styled(
            "▏".to_string(),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
    ]
}

/// Right half of the search bar — `n/N · n/N next · Esc dismiss`.
/// When there are no matches, shows `no matches` instead of a counter.
pub(super) fn render_search_bar_right(app: &App) -> Vec<Span<'static>> {
    let Some(state) = app.search() else {
        return Vec::new();
    };
    let theme = &app.theme;
    let count = app.search_match_count;
    let counter = if count == 0 {
        if state.query.is_empty() {
            "type to search ".to_string()
        } else {
            "no matches ".to_string()
        }
    } else {
        let cur = state.current.min(count.saturating_sub(1)) + 1;
        format!("{}/{} ", cur, count)
    };
    vec![
        Span::styled(counter, Style::default().fg(theme.accent)),
        Span::styled(
            "· n/N next · Esc dismiss ".to_string(),
            Style::default()
                .fg(theme.dim)
                .add_modifier(Modifier::ITALIC),
        ),
    ]
}

/// Pick a frame of a 10-step braille spinner from *fractional*
/// elapsed seconds — 10 frames per second. Pass whole seconds and it
/// pins to frame 0. Hand-rolled so we don't drag in `indicatif`.
pub(super) fn spinner_glyph(secs: f32) -> char {
    const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    let idx = ((secs * 10.0) as usize) % FRAMES.len();
    FRAMES[idx]
}

// transcript_line_count was an approximation used while the
// renderer borrowed `app.transcript`. The current single-pass
// design (Vec<Line<'static>> built upfront) lets us use the
// real count, so the helper is gone.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::Usage;
    use crate::tui::app::StatusModel;
    use ratatui::style::Color;

    #[test]
    fn completion_line_with_no_positions_is_single_span() {
        let base = Style::default().fg(Color::White);
        let highlight = Style::default().fg(Color::Yellow);
        let line = build_completion_line("  ", "sessions", &[], base, highlight);
        // Prefix span + body span = 2.
        assert_eq!(line.spans.len(), 2);
        assert_eq!(line.spans[1].content, "sessions");
    }

    #[test]
    fn completion_line_splits_around_match_positions() {
        let base = Style::default().fg(Color::White);
        let highlight = Style::default().fg(Color::Yellow);
        // Highlight chars 0 ("s") and 4 ("i") in "sessions" → "s" + "essi" + "ons"... but
        // actually 4 maps to "i" → so we expect:
        //   prefix("  ") + match("s") + base("ess") + match("i") + base("ons")
        let line = build_completion_line("  ", "sessions", &[0, 4], base, highlight);
        assert_eq!(line.spans.len(), 5);
        assert_eq!(line.spans[0].content, "  ");
        assert_eq!(line.spans[1].content, "s");
        assert_eq!(line.spans[1].style, highlight);
        assert_eq!(line.spans[2].content, "ess");
        assert_eq!(line.spans[3].content, "i");
        assert_eq!(line.spans[3].style, highlight);
        assert_eq!(line.spans[4].content, "ons");
    }

    #[test]
    fn format_count_renders_units_at_thresholds() {
        assert_eq!(format_count(42), "42");
        assert_eq!(format_count(9_999), "9999");
        assert_eq!(format_count(12_345), "12.3k");
        assert_eq!(format_count(123_000), "123k");
        assert_eq!(format_count(2_500_000), "2.5m");
    }

    fn app_with_status(s: StatusModel) -> App {
        let mut app = App::new();
        app.set_status(s);
        app
    }

    /// Render `draw_input` into a 20x3 buffer and return its rows.
    fn render_composer(app: &App) -> Vec<String> {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut term = Terminal::new(TestBackend::new(20, 3)).unwrap();
        term.draw(|f| draw_input(f, f.area(), app)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn composer_pads_text_on_all_four_sides() {
        let mut app = App::new();
        app.input = {
            let mut t = tui_textarea::TextArea::new(vec!["hello".to_string()]);
            t.set_placeholder_text("");
            t
        };
        let rows = render_composer(&app);
        assert_eq!(rows[0].trim(), "", "top row must be blank padding");
        assert_eq!(rows[2].trim(), "", "bottom row must be blank padding");
        // `›` sits one col in; text starts after the gutter, matching
        // the transcript's user-turn indent.
        let gutter: String = rows[1].chars().take(COMPOSER_GUTTER as usize).collect();
        assert_eq!(gutter, " › ");
        assert!(rows[1].starts_with(" › hello"), "got: {:?}", rows[1]);
        assert!(rows[1].ends_with(' '), "right edge must stay padded");
    }

    #[test]
    fn search_bar_left_is_empty_when_overlay_is_closed() {
        let app = app_with_status(StatusModel::default());
        let spans = render_search_bar_left(&app);
        assert!(spans.is_empty());
    }

    #[test]
    fn search_bar_left_renders_glyph_query_and_cursor() {
        let mut app = app_with_status(StatusModel::default());
        app.open_search();
        app.search_push_char('h');
        app.search_push_char('i');
        let spans = render_search_bar_left(&app);
        let combined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(combined.contains("🔍"));
        assert!(combined.contains("hi"));
        // Cursor glyph appears at the end of the query.
        assert!(combined.contains("▏"));
    }

    #[test]
    fn search_bar_right_shows_type_to_search_when_query_empty() {
        let mut app = app_with_status(StatusModel::default());
        app.open_search();
        let spans = render_search_bar_right(&app);
        let combined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(combined.contains("type to search"));
        assert!(combined.contains("Esc dismiss"));
    }

    #[test]
    fn search_bar_right_shows_no_matches_when_count_is_zero() {
        let mut app = app_with_status(StatusModel::default());
        app.open_search();
        app.search_push_char('z');
        // search_match_count stays 0 — renderer caches it; without a render
        // pass, we simulate the empty-results case.
        let spans = render_search_bar_right(&app);
        let combined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(combined.contains("no matches"));
    }

    #[test]
    fn search_bar_right_shows_one_indexed_counter_when_matches_exist() {
        let mut app = app_with_status(StatusModel::default());
        app.open_search();
        app.search_push_char('x');
        app.search_match_count = 3;
        // current is zero-indexed internally; should display as 1/3.
        let spans = render_search_bar_right(&app);
        let combined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(combined.contains("1/3"), "got: {}", combined);
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn spinner_animates_within_a_second() {
        // Guard against the frozen-spinner regression: feeding whole
        // seconds pins the glyph, so distinct sub-second offsets must
        // yield distinct frames.
        assert_ne!(spinner_glyph(0.0), spinner_glyph(0.5));
        assert_ne!(spinner_glyph(0.1), spinner_glyph(0.2));
        // Cycles back after 1s.
        assert_eq!(spinner_glyph(0.0), spinner_glyph(1.0));
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
            last_usage: Some(Usage {
                prompt_tokens: 30_000,
                completion_tokens: 5_000,
                total_tokens: 35_000,
            }),
            ..Default::default()
        });
        let spans = context_gauge_spans(&green);
        assert_eq!(spans[0].content, "65%");
        assert_eq!(spans[0].style.fg, Some(Color::Green));
        assert_eq!(spans[1].content, " context left");

        let amber = app_with_status(StatusModel {
            ctx_window: 100_000,
            last_usage: Some(Usage {
                prompt_tokens: 70_000,
                completion_tokens: 0,
                total_tokens: 70_000,
            }),
            ..Default::default()
        });
        assert_eq!(context_gauge_spans(&amber)[0].style.fg, Some(Color::Yellow));

        let red = app_with_status(StatusModel {
            ctx_window: 100_000,
            last_usage: Some(Usage {
                prompt_tokens: 90_000,
                completion_tokens: 0,
                total_tokens: 90_000,
            }),
            ..Default::default()
        });
        assert_eq!(context_gauge_spans(&red)[0].style.fg, Some(Color::Red));
    }

    #[test]
    fn context_gauge_falls_back_to_used_count_when_window_unknown() {
        let app = app_with_status(StatusModel {
            ctx_window: 0,
            last_usage: Some(Usage {
                prompt_tokens: 12_345,
                completion_tokens: 0,
                total_tokens: 12_345,
            }),
            ..Default::default()
        });
        let spans = context_gauge_spans(&app);
        assert_eq!(line_text(&Line::from(spans)), "12.3k used");
    }

    #[test]
    fn status_row_blank_when_idle_and_when_approval_pending() {
        let app = app_with_status(StatusModel::default());
        assert!(
            render_status_row(&app)
                .iter()
                .all(|l| line_text(l).is_empty())
        );

        let mut app = app_with_status(StatusModel::default());
        app.on_approval_requested(
            "Edit".into(),
            serde_json::json!({"file_path":"x"}),
            "edit".into(),
        );
        assert!(
            render_status_row(&app)
                .iter()
                .all(|l| line_text(l).is_empty())
        );
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

    #[test]
    fn footer_truncates_cwd_around_surviving_model() {
        let app = app_with_status(StatusModel {
            model: "kimi-k3".into(),
            ctx_window: 100_000,
            branch: Some("main".into()),
            cwd: "~/dev/devenv/oli".into(),
            ..Default::default()
        });
        // 40 cols: shortcuts and branch are gone, cwd is
        // center-truncated, and the model still survives.
        let line = line_text(&build_footer_line(&app, 40));
        assert!(!line.contains("? for shortcuts"), "got: {line}");
        assert!(!line.contains("main"), "got: {line}");
        assert!(line.contains('…'), "cwd should be truncated: {line}");
        assert!(line.contains("kimi-k3"), "model must survive: {line}");
    }

    #[test]
    fn status_row_shows_thinking_before_first_token() {
        let mut app = app_with_status(StatusModel::default());
        app.on_turn_started();
        let lines = render_status_row(&app);
        assert!(line_text(&lines[0]).contains("Thinking"));
    }
}

fn draw_input(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    // Borderless composer on a tinted band. The band is padded on all
    // four sides: `COMPOSER_V_PAD` blank rows above/below the text and
    // `COMPOSER_GUTTER` / `TRANSCRIPT_H_PAD` cols left/right. The `›`
    // sits in the left gutter at the same column as the transcript's
    // user-turn prefix, so the composer reads as the next user turn.
    f.render_widget(
        Block::default().style(Style::default().bg(theme.user_band_bg)),
        area,
    );
    let busy = app.is_busy();
    let glyph_style = if busy {
        Style::default().fg(theme.dim).bg(theme.user_band_bg)
    } else {
        Style::default()
            .bg(theme.user_band_bg)
            .add_modifier(Modifier::BOLD)
    };
    let text_y = area.y + COMPOSER_V_PAD;
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " ".repeat(TRANSCRIPT_H_PAD as usize),
                Style::default().bg(theme.user_band_bg),
            ),
            Span::styled("›".to_string(), glyph_style),
        ])),
        Rect {
            x: area.x,
            y: text_y,
            width: COMPOSER_GUTTER,
            height: 1,
        },
    );
    let text_area = Rect {
        x: area.x + COMPOSER_GUTTER,
        y: text_y,
        width: area
            .width
            .saturating_sub(COMPOSER_GUTTER + TRANSCRIPT_H_PAD),
        height: area.height.saturating_sub(COMPOSER_V_PAD * 2).max(1),
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

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let line = build_footer_line(app, area.width.saturating_sub(TRANSCRIPT_H_PAD * 2));
    let footer =
        Paragraph::new(line).block(Block::default().padding(Padding::horizontal(TRANSCRIPT_H_PAD)));
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

    let mut left: Vec<Span<'static>> =
        if app.focused_card_idx.is_some() && matches!(app.mode, Mode::Idle) {
            vec![Span::styled(
                "enter expand · {/} cards · esc clear".to_string(),
                Style::default()
                    .fg(theme.dim)
                    .add_modifier(Modifier::ITALIC),
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
        let sep = if others > 0 { 3 } else { 0 };
        let room = budget.saturating_sub(others + sep) as usize;
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
                spans.push(Span::styled(
                    " · ".to_string(),
                    Style::default().fg(theme.dim),
                ));
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
        spans.push(Span::styled(
            app.status.model.clone(),
            Style::default().fg(theme.fg),
        ));
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

/// Slash / `@path` completion popup. Drawn just above the input
/// box, overlapping the bottom of the transcript pane. Caps at 8
/// visible rows; arrow keys cycle through candidates regardless.
fn draw_completion_popup(f: &mut Frame, transcript_area: Rect, input_area: Rect, app: &App) {
    let menu = match app.completion.as_ref() {
        Some(m) => m,
        None => return,
    };
    if menu.candidates.is_empty() {
        return;
    }
    let visible = menu.candidates.len().min(8) as u16;
    let popup_height = visible + 2; // borders
    let width = (menu
        .candidates
        .iter()
        .map(|c| c.chars().count())
        .max()
        .unwrap_or(20)
        + 4) as u16;
    let width = width.min(input_area.width).max(20);
    // Anchor: align the popup's left edge with the input box,
    // bottom edge sitting one row above the input box's top.
    let y = input_area.y.saturating_sub(popup_height);
    let y = y.max(transcript_area.y);
    let popup_area = Rect {
        x: input_area.x,
        y,
        width: width.min(input_area.width),
        height: popup_height,
    };

    let theme = &app.theme;
    f.render_widget(Clear, popup_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.dim));
    let inner = block.inner(popup_area);
    f.render_widget(block, popup_area);

    // Render candidates with the selected one highlighted. We
    // paint the visible window starting at the closest entry to
    // the selection so a wraparound from the bottom is visible.
    let start = menu
        .selected
        .saturating_sub(visible as usize - 1)
        .min(menu.candidates.len().saturating_sub(visible as usize));
    let lines: Vec<Line> = menu
        .candidates
        .iter()
        .enumerate()
        .skip(start)
        .take(visible as usize)
        .map(|(i, name)| {
            let is_selected = i == menu.selected;
            let base_style = if is_selected {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.fg)
            };
            let match_style = base_style
                .fg(theme.match_highlight)
                .add_modifier(Modifier::BOLD);
            let prefix = if is_selected { "› " } else { "  " };
            let positions = menu.match_positions.get(i).cloned().unwrap_or_default();
            build_completion_line(prefix, name, &positions, base_style, match_style)
        })
        .collect();
    let para = Paragraph::new(lines);
    f.render_widget(para, inner);
}

/// Build a `Line` for one completion row, with characters at
/// `match_positions` (char indices into `name`) styled with
/// `match_style` and everything else with `base_style`.
fn build_completion_line<'a>(
    prefix: &'a str,
    name: &'a str,
    match_positions: &[u32],
    base_style: Style,
    match_style: Style,
) -> Line<'a> {
    let mut spans: Vec<Span<'a>> = Vec::new();
    spans.push(Span::styled(prefix.to_string(), base_style));
    if match_positions.is_empty() {
        spans.push(Span::styled(name.to_string(), base_style));
        return Line::from(spans);
    }
    let mut buf = String::new();
    let mut buf_style = base_style;
    for (ci, ch) in name.chars().enumerate() {
        let is_match = match_positions.contains(&(ci as u32));
        let style = if is_match { match_style } else { base_style };
        if style != buf_style && !buf.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut buf), buf_style));
        }
        buf_style = style;
        buf.push(ch);
    }
    if !buf.is_empty() {
        spans.push(Span::styled(buf, buf_style));
    }
    Line::from(spans)
}
