//! Transcript pane render — turns `App.transcript` into a
//! `Vec<Line<'static>>` and ships it through ratatui's
//! `Paragraph`. Owned strings everywhere so we can mutate scroll
//! state on `App` afterwards without fighting borrows.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Padding, Paragraph, Wrap};

use crate::tui::app::{App, ToolCardState, TranscriptItem};
use crate::tui::markdown;

use super::spinner_glyph;

/// Horizontal padding (cols) inset from each transcript edge.
/// Applied via `Block::padding`; the rule width below subtracts 2x.
pub(super) const TRANSCRIPT_H_PAD: u16 = 1;

pub(super) fn draw_transcript(f: &mut Frame, area: Rect, app: &mut App) {
    let inner_width = area.width.saturating_sub(TRANSCRIPT_H_PAD * 2);
    let lines = build_transcript_lines(app, inner_width);

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

    let block = Block::default().padding(Padding::horizontal(TRANSCRIPT_H_PAD));
    let para = Paragraph::new(lines)
        .block(block)
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

/// Render every transcript item to a flat `Vec<Line>` ready for
/// `Paragraph`. Pure over `&App` so V2 tests can inspect the
/// rendered structure (separators, padding gutter, etc.) without a
/// terminal. `rule_width` controls the horizontal-rule glyph count
/// inserted between user→assistant turn boundaries.
pub(super) fn build_transcript_lines(app: &App, rule_width: u16) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let items = &app.transcript;
    for (i, item) in items.iter().enumerate() {
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
                        let mut spans: Vec<Span<'static>> =
                            Vec::with_capacity(md_line.spans.len() + 1);
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
            }
        }
        // Trailing separator between items. User→Assistant
        // transitions get a dim horizontal rule (turn boundary);
        // everything else gets a blank line.
        let next = items.get(i + 1);
        let user_to_assistant = matches!(item, TranscriptItem::UserPrompt { .. })
            && matches!(next, Some(TranscriptItem::Assistant { .. }));
        if user_to_assistant {
            lines.push(separator_rule(rule_width));
        } else {
            lines.push(Line::raw(""));
        }
    }
    lines
}

/// Dim horizontal rule used between user→assistant turn
/// boundaries. Width is the inner width of the transcript pane so
/// the rule visually spans the full gutter.
fn separator_rule(width: u16) -> Line<'static> {
    let glyphs: String = std::iter::repeat('─').take(width.max(1) as usize).collect();
    Line::from(Span::styled(
        glyphs,
        Style::default().fg(Color::DarkGray),
    ))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::App;
    use std::time::Duration;

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn has_rule(lines: &[Line<'_>]) -> bool {
        lines.iter().any(|l| line_text(l).contains('─'))
    }

    #[test]
    fn rule_appears_between_user_prompt_and_assistant_response() {
        let mut app = App::new();
        app.transcript
            .push(TranscriptItem::UserPrompt { body: "q".into() });
        app.transcript.push(TranscriptItem::Assistant {
            body: "a".into(),
            done: true,
        });
        let lines = build_transcript_lines(&app, 40);
        assert!(has_rule(&lines), "expected a rule in {:?}",
            lines.iter().map(line_text).collect::<Vec<_>>());
    }

    #[test]
    fn rule_does_not_appear_between_assistant_and_tool_card() {
        let mut app = App::new();
        app.transcript.push(TranscriptItem::Assistant {
            body: "a".into(),
            done: true,
        });
        app.transcript.push(TranscriptItem::ToolCard {
            tool: "grep".into(),
            args_preview: "".into(),
            state: ToolCardState::Done {
                duration: Duration::ZERO,
                summary: "ok".into(),
                ok: true,
            },
        });
        let lines = build_transcript_lines(&app, 40);
        assert!(!has_rule(&lines));
    }

    #[test]
    fn rule_does_not_appear_between_assistant_and_subsequent_user_turn() {
        // Boundary at end of assistant → start of next user prompt
        // is just a blank line, not a rule.
        let mut app = App::new();
        app.transcript.push(TranscriptItem::Assistant {
            body: "a".into(),
            done: true,
        });
        app.transcript
            .push(TranscriptItem::UserPrompt { body: "q2".into() });
        let lines = build_transcript_lines(&app, 40);
        assert!(!has_rule(&lines));
    }

    #[test]
    fn rule_width_matches_requested_width() {
        let mut app = App::new();
        app.transcript
            .push(TranscriptItem::UserPrompt { body: "q".into() });
        app.transcript.push(TranscriptItem::Assistant {
            body: "a".into(),
            done: true,
        });
        let lines = build_transcript_lines(&app, 12);
        let rule_line = lines
            .iter()
            .find(|l| line_text(l).contains('─'))
            .expect("rule should be present");
        assert_eq!(line_text(rule_line).chars().count(), 12);
    }
}
