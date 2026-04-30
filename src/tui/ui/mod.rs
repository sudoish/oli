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
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::tui::app::{App, Mode};

mod overlays;
mod transcript;

const TITLE: &str = "oli";

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let input_lines = app.input.lines().len().max(1).min(8) as u16;
    let input_height = input_lines + 2; // borders
    let chunks = Layout::vertical([
        Constraint::Length(1),               // status bar
        Constraint::Min(3),                  // transcript
        Constraint::Length(input_height),    // input
    ])
    .split(area);

    draw_status(f, chunks[0], app);
    transcript::draw_transcript(f, chunks[1], app);
    draw_input(f, chunks[2], app);

    if app.completion.is_some() {
        draw_completion_popup(f, chunks[1], chunks[2], app);
    }
    use crate::tui::app::Overlay;
    match &app.overlay {
        Some(Overlay::Approval(s)) => overlays::draw_approval_modal(f, area, s, app),
        Some(Overlay::SessionsPicker(s)) => overlays::draw_sessions_picker(f, area, s),
        Some(Overlay::HelpBrowser(s)) => overlays::draw_help_browser(f, area, s),
        Some(Overlay::InlineHelp(s)) => overlays::draw_inline_help(f, area, s),
        Some(Overlay::HistorySearch(s)) => overlays::draw_history_search(f, area, s, app),
        Some(Overlay::Wizard(s)) => overlays::draw_wizard(f, area, s),
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
    // Left-aligned identity strip + right-aligned mode indicator.
    // Width-aware collapse: the identity fields drop right-to-
    // left when the terminal narrows. Priority (most important
    // last to drop): model > tokens > branch > session.

    let mode = render_mode_indicator(app);
    let mode_w = spans_width(&mode) as u16;

    let mut left = vec![Span::styled(
        format!(" {} ", TITLE),
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )];

    let fields = build_status_fields(app);
    let mut budget = area.width.saturating_sub(mode_w + 2); // +2 for spacing
    // Subtract the title badge.
    budget = budget.saturating_sub(visible_width(&Line::from(left.clone())) as u16);

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
            Style::default().fg(Color::DarkGray),
        ));
        left.extend(field);
    }

    // Pad and append mode on the right. We compute remaining
    // width and pad with spaces so mode lands at the far right.
    let line_w = visible_width(&Line::from(left.clone())) as u16;
    let pad = area.width.saturating_sub(line_w + mode_w);
    if pad > 0 {
        left.push(Span::raw(" ".repeat(pad as usize)));
    }
    left.extend(mode);

    let bar = Paragraph::new(Line::from(left)).style(Style::default().bg(Color::Reset));
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
    let mut out: Vec<Vec<Span<'static>>> = Vec::new();
    // Model (highest priority — kept on the narrowest terminals).
    if !app.status.model.is_empty() {
        out.push(vec![Span::styled(
            app.status.model.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )]);
    }
    // Token gauge with color thresholds.
    out.push(token_gauge_field(app));
    // Branch.
    if let Some(branch) = &app.status.branch {
        out.push(vec![Span::styled(
            branch.clone(),
            Style::default().fg(Color::Magenta),
        )]);
    }
    // Session id (truncated; the full id is from
    // `new_session_id()` which is a 13-digit unix-millis string).
    if let Some(id) = &app.status.session_id {
        let short: String = id.chars().rev().take(6).collect::<String>();
        let short: String = short.chars().rev().collect();
        out.push(vec![Span::styled(
            format!("session …{}", short),
            Style::default().fg(Color::DarkGray),
        )]);
    }
    out
}

/// Tokens-used / context-window with a color graded by ratio:
/// green < 60%, amber 60–85%, red > 85%. Falls through to a
/// plain dash when no usage is recorded yet.
fn token_gauge_field(app: &App) -> Vec<Span<'static>> {
    let used = app
        .status
        .last_usage
        .map(|u| u.prompt_tokens as u32 + u.completion_tokens as u32)
        .unwrap_or(0);
    let ctx = app.status.ctx_window.max(1);
    let ratio = used as f32 / ctx as f32;
    let color = if ratio >= 0.85 {
        Color::Red
    } else if ratio >= 0.60 {
        Color::Yellow
    } else {
        Color::Green
    };
    let used_label = format_count(used);
    let ctx_label = format_count(ctx);
    if used == 0 {
        vec![Span::styled(
            format!("— / {} tok", ctx_label),
            Style::default().fg(Color::DarkGray),
        )]
    } else {
        vec![
            Span::styled(used_label, Style::default().fg(color)),
            Span::styled(
                format!(" / {} tok", ctx_label),
                Style::default().fg(Color::DarkGray),
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

/// Right-aligned mode indicator: the live signal of "what is
/// the loop doing now?" Approval modal up trumps everything;
/// otherwise renders the agent's mode with a spinner / arrow
/// / dot / pause glyph.
fn render_mode_indicator(app: &App) -> Vec<Span<'static>> {
    if app.approval().is_some() {
        return vec![Span::styled(
            " ⏸ awaiting approval ".to_string(),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )];
    }
    match &app.mode {
        Mode::Idle => vec![Span::styled(
            " · idle ".to_string(),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )],
        Mode::Thinking { since } => {
            let secs = since.elapsed().as_secs_f32();
            vec![Span::styled(
                format!(" {} thinking · {:.1}s ", spinner_glyph(secs), secs),
                Style::default().fg(Color::Yellow),
            )]
        }
        Mode::Streaming => vec![Span::styled(
            " ▶ streaming ".to_string(),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )],
    }
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
    fn mode_indicator_overrides_to_awaiting_when_modal_is_up() {
        let mut app = app_with_status(StatusModel::default());
        app.on_approval_requested(
            "Edit".into(),
            serde_json::json!({"file_path":"x"}),
            "edit".into(),
        );
        let spans = render_mode_indicator(&app);
        let combined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(combined.contains("awaiting approval"));
    }

    #[test]
    fn mode_indicator_idle_when_nothing_is_happening() {
        let app = app_with_status(StatusModel::default());
        let spans = render_mode_indicator(&app);
        let combined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(combined.contains("idle"));
    }

    #[test]
    fn mode_indicator_streaming_when_streaming() {
        let mut app = app_with_status(StatusModel::default());
        app.on_turn_started();
        app.on_content_chunk("hi");
        let spans = render_mode_indicator(&app);
        let combined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(combined.contains("streaming"));
    }
}

fn draw_input(f: &mut Frame, area: Rect, app: &App) {
    let busy = app.is_busy();
    let (border_color, title) = if busy {
        (Color::DarkGray, " ▶ input (busy — Ctrl+C to cancel) ")
    } else {
        (Color::Cyan, " ▶ input ")
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
                .fg(Color::DarkGray)
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

    f.render_widget(Clear, popup_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Line::from(Span::styled(
            " complete ",
            Style::default()
                .fg(Color::Cyan)
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
            let style = if is_selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            Line::from(Span::styled(
                if is_selected {
                    format!("▌ {}", name)
                } else {
                    format!("  {}", name)
                },
                style,
            ))
        })
        .collect();
    let para = Paragraph::new(lines);
    f.render_widget(para, inner);
}
