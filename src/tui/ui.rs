//! Per-frame render. Lays out the three-pane shell:
//! - **status bar** (1 row, top): identity strip — Phase F has a
//!   placeholder; Phase M wires real model / session / token data.
//! - **transcript** (flex middle): user prompts, system notices,
//!   eventually assistant messages and tool cards.
//! - **input** (flex bottom, capped): single-line Phase F; multi-
//!   line via tui-textarea in Phase K.
//!
//! Rendering is intentionally re-derived from `App` on every draw —
//! there's no diff machinery. ratatui handles the on-screen diff
//! against the previous frame.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::tui::app::{App, TranscriptItem};

const TITLE: &str = "oli";

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::vertical([
        Constraint::Length(1),  // status bar
        Constraint::Min(3),     // transcript
        Constraint::Length(3),  // input box (1 line + borders)
    ])
    .split(area);

    draw_status(f, chunks[0]);
    draw_transcript(f, chunks[1], app);
    draw_input(f, chunks[2], app);
}

fn draw_status(f: &mut Frame, area: Rect) {
    // Phase F placeholder. Phase M expands this with model,
    // session, token gauge, branch, cost.
    let bar = Paragraph::new(Line::from(vec![
        Span::styled(
            format!(" {} ", TITLE),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            "ready",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ),
    ]))
    .style(Style::default().bg(Color::Reset));
    f.render_widget(bar, area);
}

fn draw_transcript(f: &mut Frame, area: Rect, app: &App) {
    let mut lines: Vec<Line> = Vec::new();
    for item in &app.transcript {
        match item {
            TranscriptItem::UserPrompt { body } => {
                lines.push(Line::from(vec![
                    Span::styled(
                        "▌ you  ",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(""),
                ]));
                for body_line in body.lines() {
                    lines.push(Line::from(Span::styled(
                        format!("  {}", body_line),
                        Style::default().fg(Color::White),
                    )));
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
        // Stick to the bottom: scroll so the last line is in view.
        // Phase L replaces this with a real ScrollState that lets
        // the user scroll up and lock there.
        .scroll((scroll_offset(area, app), 0));
    f.render_widget(para, area);
}

/// Compute a vertical scroll offset that pins the last line of
/// content to the bottom of the area. Crude approximation in
/// Phase F — counts logical lines and subtracts the visible
/// height. Wrap is treated optimistically (we under-count; a
/// long unwrapped line still scrolls roughly correctly). Phase L
/// replaces this with a proper line-aware scroll model.
fn scroll_offset(area: Rect, app: &App) -> u16 {
    let mut total_lines: usize = 0;
    for item in &app.transcript {
        match item {
            TranscriptItem::UserPrompt { body } => {
                total_lines += 1; // header
                total_lines += body.lines().count().max(1);
                total_lines += 1; // blank
            }
            TranscriptItem::System { body } => {
                total_lines += body.lines().count().max(1);
                total_lines += 1; // blank
            }
        }
    }
    let height = area.height as usize;
    if total_lines > height {
        (total_lines - height) as u16
    } else {
        0
    }
}

fn draw_input(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Line::from(vec![Span::styled(
            " ▶ input ",
            Style::default().fg(Color::Cyan),
        )]));

    // Render the input string. Cursor goes on the matching column.
    let inner = block.inner(area);
    f.render_widget(block, area);

    let para = Paragraph::new(Line::from(Span::raw(app.input.as_str())))
        .style(Style::default().fg(Color::White));
    f.render_widget(para, inner);

    // Place the terminal cursor at the byte-offset visible column.
    // We display the input in a 1-row inner area; the column is
    // the displayed-width prefix of the input up to the cursor.
    // Phase F treats one byte == one column (good enough for ASCII
    // + most accented Latin); Phase K's tui-textarea swap handles
    // wide chars properly.
    let visible_col = utf8_display_width(&app.input[..app.cursor]).min(inner.width as usize) as u16;
    f.set_cursor_position(Position::new(inner.x + visible_col, inner.y));
}

/// Crude display-width approximation: counts grapheme-ish chars,
/// not bytes. East-Asian wide chars and emoji aren't doubled (we
/// don't want to pull `unicode-width` for Phase F). Phase K
/// replaces this when tui-textarea owns the input.
fn utf8_display_width(s: &str) -> usize {
    s.chars().count()
}
