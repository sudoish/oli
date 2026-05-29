//! Per-frame render. Lays out the three-pane shell:
//! - **status bar** (1 row, top): identity + live mode indicator.
//! - **transcript** (flex middle): user prompts, assistant
//!   messages (with a live cursor while streaming), system
//!   notices.
//! - **input** (flex bottom, capped): single-line Phase F/G; Phase K
//!   replaces with multi-line tui-textarea.
//!
//! Rendering is intentionally re-derived from `App` on every draw —
//! there's no diff machinery. ratatui handles the on-screen diff
//! against the previous frame.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph};

use transcript::TRANSCRIPT_H_PAD;

use crate::tui::app::{App, Mode};

mod overlays;
mod transcript;

const TITLE: &str = "oli";

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let input_lines = app.input.lines().len().max(1).min(8) as u16;
    let input_height = input_lines + 2; // borders
    let chunks = Layout::vertical([
        Constraint::Length(1),               // status bar (identity)
        Constraint::Min(3),                  // transcript
        Constraint::Length(1),               // activity strip
        Constraint::Length(input_height),    // input
    ])
    .split(area);

    draw_status(f, chunks[0], app);
    transcript::draw_transcript(f, chunks[1], app);
    if app.search().is_some() {
        draw_search_bar(f, chunks[2], app);
    } else {
        draw_activity_strip(f, chunks[2], app);
    }
    draw_input(f, chunks[3], app);

    if app.completion.is_some() {
        draw_completion_popup(f, chunks[1], chunks[3], app);
    }
    use crate::tui::app::Overlay;
    match &app.overlay {
        Some(Overlay::Approval(s)) => overlays::draw_approval_modal(f, area, s, app),
        Some(Overlay::SessionsPicker(s)) => {
            overlays::draw_sessions_picker(f, area, s, &app.theme)
        }
        Some(Overlay::HelpBrowser(s)) => overlays::draw_help_browser(f, area, s, &app.theme),
        Some(Overlay::InlineHelp(s)) => overlays::draw_inline_help(f, area, s, &app.theme),
        Some(Overlay::HistorySearch(s)) => overlays::draw_history_search(f, area, s, app),
        Some(Overlay::CopyFallback(s)) => overlays::draw_copy_fallback(f, area, s, &app.theme),
        Some(Overlay::Wizard(s)) => overlays::draw_wizard(f, area, s),
        // Search bar replaces the activity strip; rendered above.
        Some(Overlay::Search(_)) => {}
        None => {}
    }
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

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    // Identity-only status bar: title chip + model + tokens +
    // branch + session, dropped right-to-left when the terminal
    // narrows. The live activity indicator lives in its own row
    // above the input (see `draw_activity_strip`).
    let theme = &app.theme;

    let mut left = vec![Span::styled(
        format!(" {} ", TITLE),
        Style::default()
            .fg(theme.selected_fg)
            .bg(theme.accent)
            .add_modifier(Modifier::BOLD),
    )];

    let fields = build_status_fields(app);
    let inner_width = area.width.saturating_sub(TRANSCRIPT_H_PAD * 2);
    let mut budget = inner_width.saturating_sub(visible_width(&Line::from(left.clone())) as u16);

    // Drop fields right-to-left until we fit. Each field is "  • <body>".
    let mut visible: Vec<Vec<Span<'static>>> = Vec::new();
    for f in fields {
        let w = spans_width(&f) as u16 + 4; // sep "  • "
        if w <= budget {
            visible.push(f);
            budget = budget.saturating_sub(w);
        }
    }

    for field in visible {
        left.push(Span::styled(
            "  • ",
            Style::default().fg(theme.dim),
        ));
        left.extend(field);
    }

    let bar = Paragraph::new(Line::from(left))
        .block(Block::default().padding(Padding::horizontal(TRANSCRIPT_H_PAD)))
        .style(Style::default().bg(theme.bg));
    f.render_widget(bar, area);
}

/// Visual width of a `Line` — counts chars, not bytes. Wide
/// (CJK / emoji) chars under-count, but every status field is
/// ASCII / Latin-1 + a few box-drawing chars (1 cell each), so
/// this is accurate in practice.
fn visible_width(line: &Line<'_>) -> usize {
    line.spans.iter().map(|s| s.content.chars().count()).sum()
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|s| s.content.chars().count()).sum()
}

/// Identity strip fields, ordered left-to-right (drop priority
/// is right-to-left so the *last* field is the first to be
/// pushed off when the terminal narrows). Each field is a
/// `Vec<Span>` so it can carry styled sub-fragments (e.g. the
/// token gauge's color-graded number).
fn build_status_fields(app: &App) -> Vec<Vec<Span<'static>>> {
    let theme = &app.theme;
    let mut out: Vec<Vec<Span<'static>>> = Vec::new();
    // Model (highest priority — kept on the narrowest terminals).
    if !app.status.model.is_empty() {
        out.push(vec![Span::styled(
            app.status.model.clone(),
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
        )]);
    }
    // Token gauge with color thresholds.
    out.push(token_gauge_field(app));
    // Branch.
    if let Some(branch) = &app.status.branch {
        out.push(vec![Span::styled(
            branch.clone(),
            Style::default().fg(theme.user),
        )]);
    }
    // Session id (truncated; the full id is from
    // `new_session_id()` which is a 13-digit unix-millis string).
    if let Some(id) = &app.status.session_id {
        let short: String = id.chars().rev().take(6).collect::<String>();
        let short: String = short.chars().rev().collect();
        out.push(vec![Span::styled(
            format!("session …{}", short),
            Style::default().fg(theme.dim),
        )]);
    }
    out
}

/// Tokens-used / context-window with a color graded by ratio:
/// green < 60%, amber 60–85%, red > 85%. Falls through to a
/// plain dash when no usage is recorded yet.
fn token_gauge_field(app: &App) -> Vec<Span<'static>> {
    let theme = &app.theme;
    let used = app
        .status
        .last_usage
        .map(|u| u.prompt_tokens as u32 + u.completion_tokens as u32)
        .unwrap_or(0);
    let ctx = app.status.ctx_window.max(1);
    let ratio = used as f32 / ctx as f32;
    let color = if ratio >= 0.85 {
        theme.gauge_danger
    } else if ratio >= 0.60 {
        theme.gauge_warn
    } else {
        theme.gauge_ok
    };
    let used_label = format_count(used);
    let ctx_label = format_count(ctx);
    if used == 0 {
        vec![Span::styled(
            format!("— / {} tok", ctx_label),
            Style::default().fg(theme.dim),
        )]
    } else {
        vec![
            Span::styled(used_label, Style::default().fg(color)),
            Span::styled(
                format!(" / {} tok", ctx_label),
                Style::default().fg(theme.dim),
            ),
        ]
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

/// Live activity row above the input. Left side: mode +
/// elapsed time (or a dim em-dash when idle). Right side:
/// `Esc to cancel` whenever the harness is busy. Approval modal
/// up trumps everything.
fn draw_activity_strip(f: &mut Frame, area: Rect, app: &App) {
    let left = render_activity_strip_left(app);
    let right = render_activity_strip_right(app);
    let left_w = spans_width(&left) as u16;
    let right_w = spans_width(&right) as u16;
    let inner_width = area.width.saturating_sub(TRANSCRIPT_H_PAD * 2);
    let mut spans = left;
    let pad = inner_width.saturating_sub(left_w + right_w);
    if pad > 0 {
        spans.push(Span::raw(" ".repeat(pad as usize)));
    }
    spans.extend(right);
    let strip = Paragraph::new(Line::from(spans))
        .block(Block::default().padding(Padding::horizontal(TRANSCRIPT_H_PAD)));
    f.render_widget(strip, area);
}

/// Left half of the activity strip — the mode label.
pub(super) fn render_activity_strip_left(app: &App) -> Vec<Span<'static>> {
    let theme = &app.theme;
    if app.approval().is_some() {
        return vec![Span::styled(
            " ⏸ awaiting approval ".to_string(),
            Style::default()
                .fg(theme.selected_fg)
                .bg(theme.tool_running)
                .add_modifier(Modifier::BOLD),
        )];
    }
    match &app.mode {
        Mode::Idle => vec![Span::styled(
            " — ".to_string(),
            Style::default().fg(theme.dim),
        )],
        Mode::Thinking { since } => {
            let secs = since.elapsed().as_secs_f32();
            vec![Span::styled(
                format!(" {} thinking · {:.1}s ", spinner_glyph(secs), secs),
                Style::default().fg(theme.tool_running),
            )]
        }
        Mode::Streaming { since } => {
            let secs = since.elapsed().as_secs_f32();
            vec![Span::styled(
                format!(" ▶ streaming · {:.1}s ", secs),
                Style::default()
                    .fg(theme.tool_ok)
                    .add_modifier(Modifier::BOLD),
            )]
        }
        Mode::ToolRunning { tool, since } => {
            let secs = since.elapsed().as_secs_f32();
            vec![Span::styled(
                format!(" {} running {} · {:.1}s ", spinner_glyph(secs), tool, secs),
                Style::default()
                    .fg(theme.user)
                    .add_modifier(Modifier::BOLD),
            )]
        }
    }
}

/// Right half of the activity strip — cancel hint while busy,
/// navigation hints while idle, suppressed while an approval modal
/// is up (the modal owns the keyboard).
pub(super) fn render_activity_strip_right(app: &App) -> Vec<Span<'static>> {
    if app.approval().is_some() {
        return Vec::new();
    }
    let text = context_hint_text(app);
    if text.is_empty() {
        return Vec::new();
    }
    vec![Span::styled(
        format!("{} ", text),
        Style::default()
            .fg(app.theme.dim)
            .add_modifier(Modifier::ITALIC),
    )]
}

/// Pick a context-appropriate hint line for the right-side of the
/// activity strip (X4). Idle shows navigation hints; Streaming /
/// ToolRunning surface the cancel keys. Empty string suppresses.
///
/// Y4: when a tool card is focused, swap the Idle hint for one
/// that surfaces the expand/dismiss keys.
pub(super) fn context_hint_text(app: &App) -> &'static str {
    if app.focused_card_idx.is_some() && matches!(app.mode, Mode::Idle) {
        return "Enter: expand · {/}: cards · Esc: clear focus";
    }
    match app.mode {
        Mode::Idle => "[/]: turns · {/}: cards · Ctrl+F: search",
        Mode::Thinking { .. } | Mode::Streaming { .. } => "Esc to cancel",
        Mode::ToolRunning { .. } => "Esc cancel · Ctrl+C hard cancel",
    }
}

/// Search bar — swaps in for the activity strip while the Search
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

/// Pick a frame of a 10-step braille spinner from elapsed seconds.
/// Hand-rolled so we don't drag in `indicatif`.
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

    #[test]
    fn token_gauge_picks_green_under_60_percent() {
        let app = app_with_status(StatusModel {
            ctx_window: 100_000,
            last_usage: Some(Usage {
                prompt_tokens: 30_000,
                completion_tokens: 5_000,
                total_tokens: 35_000,
            }),
            ..Default::default()
        });
        let spans = token_gauge_field(&app);
        // First span carries the count + green color.
        assert_eq!(spans[0].style.fg, Some(Color::Green));
    }

    #[test]
    fn token_gauge_picks_amber_at_60_to_85_percent() {
        let app = app_with_status(StatusModel {
            ctx_window: 100_000,
            last_usage: Some(Usage {
                prompt_tokens: 70_000,
                completion_tokens: 0,
                total_tokens: 70_000,
            }),
            ..Default::default()
        });
        let spans = token_gauge_field(&app);
        assert_eq!(spans[0].style.fg, Some(Color::Yellow));
    }

    #[test]
    fn token_gauge_picks_red_above_85_percent() {
        let app = app_with_status(StatusModel {
            ctx_window: 100_000,
            last_usage: Some(Usage {
                prompt_tokens: 90_000,
                completion_tokens: 0,
                total_tokens: 90_000,
            }),
            ..Default::default()
        });
        let spans = token_gauge_field(&app);
        assert_eq!(spans[0].style.fg, Some(Color::Red));
    }

    #[test]
    fn token_gauge_renders_dash_when_no_usage_recorded() {
        let app = app_with_status(StatusModel {
            ctx_window: 100_000,
            ..Default::default()
        });
        let spans = token_gauge_field(&app);
        let combined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(combined.starts_with("—"), "got: {}", combined);
    }

    #[test]
    fn build_status_fields_include_model_and_branch() {
        let app = app_with_status(StatusModel {
            model: "claude-haiku-4.5".into(),
            ctx_window: 200_000,
            branch: Some("main *".into()),
            session_id: Some("1714411234567".into()),
            ..Default::default()
        });
        let fields = build_status_fields(&app);
        // Model present.
        let model_seen = fields
            .iter()
            .any(|f| f.iter().any(|s| s.content.contains("claude-haiku-4.5")));
        assert!(model_seen);
        // Branch present.
        let branch_seen = fields
            .iter()
            .any(|f| f.iter().any(|s| s.content.contains("main *")));
        assert!(branch_seen);
        // Session id rendered with truncated tail.
        let session_seen = fields
            .iter()
            .any(|f| f.iter().any(|s| s.content.contains("session …")));
        assert!(session_seen);
    }

    #[test]
    fn activity_strip_overrides_to_awaiting_when_modal_is_up() {
        let mut app = app_with_status(StatusModel::default());
        app.on_approval_requested(
            "Edit".into(),
            serde_json::json!({"file_path":"x"}),
            "edit".into(),
        );
        let spans = render_activity_strip_left(&app);
        let combined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(combined.contains("awaiting approval"));
    }

    #[test]
    fn activity_strip_renders_dim_dash_when_idle() {
        let app = app_with_status(StatusModel::default());
        let spans = render_activity_strip_left(&app);
        let combined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(combined.contains("—"));
    }

    #[test]
    fn activity_strip_renders_streaming_label_with_elapsed() {
        let mut app = app_with_status(StatusModel::default());
        app.on_turn_started();
        app.on_content_chunk("hi");
        let spans = render_activity_strip_left(&app);
        let combined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(combined.contains("streaming"));
        assert!(combined.contains("s")); // "0.0s" / "1.2s" etc.
    }

    #[test]
    fn activity_strip_renders_tool_running_label() {
        let mut app = app_with_status(StatusModel::default());
        app.on_turn_started();
        app.on_tool_start(1, "grep".into(), "".into());
        let spans = render_activity_strip_left(&app);
        let combined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(combined.contains("running grep"), "got: {}", combined);
    }

    #[test]
    fn activity_strip_right_shows_cancel_hint_while_busy() {
        let mut app = app_with_status(StatusModel::default());
        app.on_turn_started();
        let spans = render_activity_strip_right(&app);
        let combined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(combined.contains("Esc"));
    }

    #[test]
    fn activity_strip_right_shows_idle_nav_hints() {
        // X4: idle mode advertises the nav keybindings on the right
        // side of the activity strip.
        let app = app_with_status(StatusModel::default());
        let spans = render_activity_strip_right(&app);
        let combined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(combined.contains("[/]"), "got: {}", combined);
        assert!(combined.contains("search"), "got: {}", combined);
    }

    #[test]
    fn activity_strip_right_shows_hard_cancel_hint_while_tool_running() {
        let mut app = app_with_status(StatusModel::default());
        app.on_turn_started();
        app.on_tool_start(1, "grep".into(), "".into());
        let spans = render_activity_strip_right(&app);
        let combined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(combined.contains("Ctrl+C"), "got: {}", combined);
    }

    #[test]
    fn activity_strip_right_is_empty_when_modal_is_up() {
        let mut app = app_with_status(StatusModel::default());
        app.on_approval_requested(
            "Edit".into(),
            serde_json::json!({"file_path":"x"}),
            "edit".into(),
        );
        let spans = render_activity_strip_right(&app);
        assert!(spans.is_empty());
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

    #[test]
    fn context_hint_idle_advertises_card_keys() {
        let app = app_with_status(StatusModel::default());
        let hint = context_hint_text(&app);
        assert!(hint.contains("{/}"), "got: {}", hint);
    }

    #[test]
    fn context_hint_with_focused_card_swaps_to_expand_legend() {
        let mut app = app_with_status(StatusModel::default());
        app.focused_card_idx = Some(0);
        let hint = context_hint_text(&app);
        assert!(hint.contains("Enter: expand"), "got: {}", hint);
        assert!(hint.contains("Esc"), "got: {}", hint);
    }
}

fn draw_input(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let busy = app.is_busy();
    let (border_color, title) = if busy {
        (theme.dim, " ▶ input (busy — Ctrl+C to cancel) ")
    } else {
        (theme.border, " ▶ input ")
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(Line::from(vec![Span::styled(
            title,
            Style::default().fg(border_color),
        )]));

    if busy {
        // While busy, replace the textarea with a hint paragraph
        // — no cursor, no typing. Restored on TurnFinished.
        let inner = block.inner(area);
        f.render_widget(block, area);
        let body = Paragraph::new(Line::from(Span::styled(
            "(waiting for response — Ctrl+C cancels)".to_string(),
            Style::default()
                .fg(theme.dim)
                .add_modifier(Modifier::ITALIC),
        )));
        f.render_widget(body, inner);
        return;
    }

    // Defer to tui-textarea: it owns cursor placement, multi-line
    // wrapping, and selection. We render the block separately so
    // the title stays consistent with the rest of the layout.
    f.render_widget(block, area);
    let inner = inner_for_input(area);
    f.render_widget(&app.input, inner);
}

/// Inner area for the input box matching the bordered block. We
/// can't reuse `Block::inner` after rendering because we already
/// consumed the block; recompute. The block above borders with 1
/// row top + 1 row bottom + 1 col left + 1 col right.
fn inner_for_input(area: Rect) -> Rect {
    Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    }
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
        .border_style(Style::default().fg(theme.border))
        .title(Line::from(Span::styled(
            " complete ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )));
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
                    .fg(theme.selected_fg)
                    .bg(theme.selected_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.fg)
            };
            let match_style = base_style
                .fg(theme.match_highlight)
                .add_modifier(Modifier::BOLD);
            let prefix = if is_selected { "▌ " } else { "  " };
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
