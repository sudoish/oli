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

use crate::tui::app::{App, Mode, TranscriptItem};

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
        }
    }

    let para = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll_offset(area, app), 0));
    f.render_widget(para, area);
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
