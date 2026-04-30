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
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::tui::app::{App, ApprovalState, Mode, ToolCardState, TranscriptItem};
use crate::tui::markdown;

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
    draw_transcript(f, chunks[1], app);
    draw_input(f, chunks[2], app);

    if app.completion.is_some() {
        draw_completion_popup(f, chunks[1], chunks[2], app);
    }
    if let Some(approval) = &app.approval {
        draw_approval_modal(f, area, approval);
    }
}

/// Centered overlay above the transcript. Width 80% of the
/// terminal (clamped to [60, 120] cols), height 60% of the
/// terminal (clamped to [10, 40] rows). `Clear` blanks the area
/// underneath so the transcript bleed-through doesn't make the
/// modal hard to read.
fn draw_approval_modal(f: &mut Frame, full_area: Rect, approval: &ApprovalState) {
    let modal = centered_rect(full_area, 80, 60).intersection(full_area);
    f.render_widget(Clear, modal);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .title(Line::from(vec![
            Span::styled(
                " ⚠ approve ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {} ", approval.tool),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
    let inner = block.inner(modal);
    f.render_widget(block, modal);

    // Layout inside the modal: reason (1 line), preview (flex),
    // legend (1 line).
    let parts = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(2),
    ])
    .split(inner);

    // Reason header.
    let reason = Paragraph::new(Line::from(vec![
        Span::styled(
            "  reason: ",
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            approval.reason.as_str(),
            Style::default().fg(Color::White),
        ),
    ]));
    f.render_widget(reason, parts[0]);

    // Diff / preview body. Lines starting with `+`/`-` (after the
    // 4-space indent the diff renderer emits) get colored;
    // everything else stays white.
    let lines: Vec<Line> = approval
        .preview
        .lines()
        .map(diff_line_to_styled)
        .collect();
    let preview = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((approval.scroll, 0));
    f.render_widget(preview, parts[1]);

    // Legend / single-key affordances.
    let legend = Paragraph::new(Line::from(vec![
        Span::styled(
            "  [y]es  [n]o  [a]llow this session  [d]eny session  [PgUp/Dn] scroll  [Esc] cancel",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ),
    ]));
    f.render_widget(legend, parts[2]);
}

/// Color a unified-diff line by its sign char. Renderer in
/// `policy::render_unified_diff` emits `    + body` / `    - body`
/// / `      body` (4 spaces indent + sign + space + body). Others
/// (the "file: …", "(replace_all)" header lines etc) stay white.
fn diff_line_to_styled(line: &str) -> Line<'_> {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix("+ ") {
        Line::from(vec![
            Span::raw("    "),
            Span::styled(
                format!("+ {}", rest),
                Style::default().fg(Color::Green),
            ),
        ])
    } else if let Some(rest) = trimmed.strip_prefix("- ") {
        Line::from(vec![
            Span::raw("    "),
            Span::styled(
                format!("- {}", rest),
                Style::default().fg(Color::Red),
            ),
        ])
    } else {
        Line::from(Span::styled(
            line.to_string(),
            Style::default().fg(Color::White),
        ))
    }
}

/// Centered rectangle within `r`, sized as a percentage. Clamped
/// in both dimensions so the modal doesn't shrink past readability
/// or balloon past usefulness.
fn centered_rect(r: Rect, percent_x: u16, percent_y: u16) -> Rect {
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

fn draw_transcript(f: &mut Frame, area: Rect, app: &mut App) {
    // Build the Vec<Line> first. Each line carries owned content
    // so it doesn't borrow from `app.transcript` — we need that
    // freedom to mutate `app.scroll_*` afterwards.
    let mut lines: Vec<Line<'static>> = Vec::new();
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
                    lines.push(Line::from(Span::styled(
                        "  · waiting for first token",
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::ITALIC),
                    )));
                } else {
                    // Re-parse the current body each frame.
                    // pulldown-cmark on a few KB of prose is
                    // microsecond-fast; mid-stream un-closed
                    // tokens render as literal text. Each
                    // markdown line gets a 2-space gutter so it
                    // visually nests under the `▌ oli` header.
                    for md_line in markdown::render(body, app.theme) {
                        let mut spans: Vec<Span<'static>> = Vec::with_capacity(md_line.spans.len() + 1);
                        spans.push(Span::raw("  "));
                        spans.extend(md_line.spans.into_iter());
                        lines.push(Line::from(spans));
                    }
                }
                if !done && is_active {
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

    // Now that the borrow on `app.transcript` is dropped, settle
    // the scroll metrics from the actual rendered line count.
    let total = lines.len() as u16;
    let height = area.height;
    let max = total.saturating_sub(height);
    app.note_scroll_metrics(max, height);
    let offset = match app.scroll_manual {
        None => max,
        Some(o) => o.min(max),
    };
    let detached = app.is_scroll_detached();
    let unread = app.unread_lines;

    let para = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((offset, 0));
    f.render_widget(para, area);

    // Floating "↓ N new" indicator on the bottom-right of the
    // transcript pane while scrolled away from the bottom and
    // new content has arrived.
    if detached && unread > 0 && area.height >= 1 {
        let label = format!(" ↓ {} new — End to catch up ", unread);
        let label_w = label.chars().count() as u16;
        if label_w + 2 < area.width {
            let badge_area = Rect {
                x: area.x + area.width - label_w - 1,
                y: area.y + area.height - 1,
                width: label_w,
                height: 1,
            };
            f.render_widget(Clear, badge_area);
            let badge = Paragraph::new(Line::from(Span::styled(
                label,
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
            f.render_widget(badge, badge_area);
        }
    }
}

/// Header line of a tool card:
/// `→ Read   src/main.rs                            0.04s ✓`
fn render_tool_card_line(
    tool: &str,
    args_preview: &str,
    state: &ToolCardState,
) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
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
fn render_tool_card_detail(state: &ToolCardState) -> Option<Line<'static>> {
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

// transcript_line_count was an approximation used while the
// renderer borrowed `app.transcript`. The current single-pass
// design (Vec<Line<'static>> built upfront) lets us use the
// real count, so the helper is gone.

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
