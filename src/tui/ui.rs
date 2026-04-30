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
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::tui::app::{App, Mode, ToolCardState, TranscriptItem};

const TITLE: &str = "oli";

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::vertical([
        Constraint::Length(1), // status bar
        Constraint::Min(3),    // transcript
        Constraint::Length(3), // input box
    ])
    .split(area);

    draw_status(f, chunks[0], app);
    draw_transcript(f, chunks[1], app);
    draw_input(f, chunks[2], app);
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let (mode_label, mode_style) = match &app.mode {
        Mode::Idle => (
            " idle ".to_string(),
            Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
        ),
        Mode::Thinking { since } => {
            let secs = since.elapsed().as_secs_f32();
            (
                format!(" {} thinking · {:.1}s ", spinner_glyph(secs), secs),
                Style::default().fg(Color::Yellow),
            )
        }
        Mode::Streaming => (
            " ▶ streaming ".to_string(),
            Style::default().fg(Color::Green),
        ),
    };

    let bar = Paragraph::new(Line::from(vec![
        Span::styled(
            format!(" {} ", TITLE),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(mode_label, mode_style),
    ]))
    .style(Style::default().bg(Color::Reset));
    f.render_widget(bar, area);
}

/// Pick a frame of a 10-step braille spinner from elapsed seconds.
/// Hand-rolled so we don't drag in `indicatif`.
fn spinner_glyph(secs: f32) -> char {
    const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    let idx = ((secs * 10.0) as usize) % FRAMES.len();
    FRAMES[idx]
}

fn draw_transcript(f: &mut Frame, area: Rect, app: &App) {
    let mut lines: Vec<Line> = Vec::new();
    for (i, item) in app.transcript.iter().enumerate() {
        let is_active = app.active_assistant == Some(i);
        match item {
            TranscriptItem::UserPrompt { body } => {
                lines.push(Line::from(Span::styled(
                    "▌ you",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )));
                for body_line in body.lines() {
                    lines.push(Line::from(Span::styled(
                        format!("  {}", body_line),
                        Style::default().fg(Color::White),
                    )));
                }
                lines.push(Line::raw(""));
            }
            TranscriptItem::Assistant { body, done } => {
                lines.push(Line::from(Span::styled(
                    "▌ oli",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )));
                if body.is_empty() && is_active {
                    // Reserve a row so the user sees something
                    // happening even before the first chunk lands.
                    lines.push(Line::from(Span::styled(
                        "  · waiting for first token",
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::ITALIC),
                    )));
                } else {
                    for body_line in body.lines() {
                        lines.push(Line::from(Span::styled(
                            format!("  {}", body_line),
                            Style::default().fg(Color::White),
                        )));
                    }
                }
                if !done && is_active {
                    // Subtle live-cursor block at the end of the
                    // active streaming message so the user can
                    // tell content is still arriving.
                    let last = lines
                        .last_mut()
                        .expect("at least the header was pushed");
                    last.spans.push(Span::styled(
                        "▍",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::SLOW_BLINK),
                    ));
                }
                lines.push(Line::raw(""));
            }
            TranscriptItem::System { body } => {
                for body_line in body.lines() {
                    lines.push(Line::from(Span::styled(
                        format!("  {}", body_line),
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::ITALIC),
                    )));
                }
                lines.push(Line::raw(""));
            }
            TranscriptItem::ToolCard {
                tool,
                args_preview,
                state,
                ..
            } => {
                lines.push(render_tool_card_line(tool, args_preview, state));
                if let Some(detail) = render_tool_card_detail(state) {
                    lines.push(detail);
                }
                lines.push(Line::raw(""));
            }
        }
    }

    let para = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll_offset(area, app), 0));
    f.render_widget(para, area);
}

/// Header line of a tool card:
/// `→ Read   src/main.rs                            0.04s ✓`
fn render_tool_card_line<'a>(
    tool: &'a str,
    args_preview: &'a str,
    state: &ToolCardState,
) -> Line<'a> {
    let mut spans: Vec<Span<'a>> = Vec::new();
    let arrow_color = match state {
        ToolCardState::Running { .. } => Color::Yellow,
        ToolCardState::Done { ok: true, .. } => Color::Green,
        ToolCardState::Done { ok: false, .. } => Color::Red,
    };
    spans.push(Span::styled(
        "  → ",
        Style::default()
            .fg(arrow_color)
            .add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(
        format!("{:<7}", tool),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        args_preview.to_string(),
        Style::default().fg(Color::White),
    ));
    spans.push(Span::raw("  "));
    match state {
        ToolCardState::Running { started_at } => {
            let elapsed = started_at.elapsed().as_secs_f32();
            spans.push(Span::styled(
                format!("{} {:.1}s", spinner_glyph(elapsed), elapsed),
                Style::default().fg(Color::Yellow),
            ));
        }
        ToolCardState::Done { duration, ok, .. } => {
            spans.push(Span::styled(
                format!("{:.2}s", duration.as_secs_f32()),
                Style::default().fg(Color::DarkGray),
            ));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                if *ok { "✓" } else { "✗" },
                Style::default().fg(if *ok { Color::Green } else { Color::Red }),
            ));
        }
    }
    Line::from(spans)
}

/// Optional detail line under a card. Running cards show no
/// detail; done cards show their summary indented under the
/// header.
fn render_tool_card_detail<'a>(state: &'a ToolCardState) -> Option<Line<'a>> {
    match state {
        ToolCardState::Running { .. } => None,
        ToolCardState::Done { summary, ok, .. } => {
            if summary.is_empty() {
                return None;
            }
            Some(Line::from(Span::styled(
                format!("    {}", summary),
                Style::default()
                    .fg(if *ok { Color::DarkGray } else { Color::Red })
                    .add_modifier(Modifier::ITALIC),
            )))
        }
    }
}

/// Pin the last logical line to the bottom of the viewport. Crude
/// in Phase F — counts logical lines without accounting for wrap;
/// good enough until Phase L's proper line-aware scroll model.
fn scroll_offset(area: Rect, app: &App) -> u16 {
    let mut total: usize = 0;
    for item in &app.transcript {
        match item {
            TranscriptItem::UserPrompt { body } => {
                total += 1; // header
                total += body.lines().count().max(1);
                total += 1; // blank
            }
            TranscriptItem::Assistant { body, .. } => {
                total += 1; // header
                total += body.lines().count().max(1);
                total += 1; // blank
            }
            TranscriptItem::System { body } => {
                total += body.lines().count().max(1);
                total += 1; // blank
            }
            TranscriptItem::ToolCard { state, .. } => {
                total += 1; // header
                if matches!(state, ToolCardState::Done { .. }) {
                    total += 1; // detail line
                }
                total += 1; // blank
            }
        }
    }
    let height = area.height as usize;
    if total > height {
        (total - height) as u16
    } else {
        0
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

    let inner = block.inner(area);
    f.render_widget(block, area);

    let body = if busy && app.input.is_empty() {
        Span::styled(
            "(waiting for response — Ctrl+C cancels)".to_string(),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )
    } else {
        Span::raw(app.input.as_str())
    };
    let para = Paragraph::new(Line::from(body)).style(Style::default().fg(Color::White));
    f.render_widget(para, inner);

    // Hide the cursor while busy — there's nothing to type into.
    if !busy {
        let visible_col =
            utf8_display_width(&app.input[..app.cursor]).min(inner.width as usize) as u16;
        f.set_cursor_position(Position::new(inner.x + visible_col, inner.y));
    }
}

fn utf8_display_width(s: &str) -> usize {
    s.chars().count()
}
