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
/// Bottom padding (rows) between transcript content and the
/// activity strip / input box. Gives messages visual breathing
/// room above the prompt area.
pub(super) const TRANSCRIPT_BOTTOM_PAD: u16 = 2;

pub(super) fn draw_transcript(f: &mut Frame, area: Rect, app: &mut App) {
    let inner_width = area.width.saturating_sub(TRANSCRIPT_H_PAD * 2);
    let lines = build_transcript_lines(app, inner_width);

    // Now that the borrow on `app.transcript` is dropped, settle
    // the scroll metrics from the actual rendered line count.
    let total = lines.len() as u16;
    let height = area.height.saturating_sub(TRANSCRIPT_BOTTOM_PAD);
    let max = total.saturating_sub(height);
    app.note_scroll_metrics(max, height);
    let offset = match app.scroll_manual {
        None => max,
        Some(o) => o.min(max),
    };
    let detached = app.is_scroll_detached();
    let unread = app.unread_lines;

    let block = Block::default().padding(Padding::new(
        TRANSCRIPT_H_PAD,
        TRANSCRIPT_H_PAD,
        0,
        TRANSCRIPT_BOTTOM_PAD,
    ));
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
                // Header `you ▐` — mirror of the assistant's `▌ oli`,
                // pinned to the right edge of the inner pane.
                let header_text = "you ";
                let header_chars = header_text.chars().count() as u16 + 1; // +1 for ▐
                let header_pad = rule_width.saturating_sub(header_chars) as usize;
                let mut header_spans: Vec<Span<'static>> = Vec::with_capacity(3);
                if header_pad > 0 {
                    header_spans.push(Span::raw(" ".repeat(header_pad)));
                }
                header_spans.push(Span::styled(
                    header_text.to_string(),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ));
                header_spans.push(Span::styled(
                    "▐".to_string(),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ));
                lines.push(Line::from(header_spans));

                // Body chunks right-aligned with a 2-col right
                // gutter — mirrors the 2-col left gutter assistant
                // body uses under `▌ oli`.
                let body_right_gutter: u16 = 2;
                let body_total_w = rule_width.saturating_sub(body_right_gutter);
                let bubble_width = bubble_width_for(rule_width);
                for body_line in body.lines() {
                    for chunk in wrap_to_width(body_line, bubble_width as usize) {
                        let chunk_w = chunk.chars().count() as u16;
                        let pad = body_total_w.saturating_sub(chunk_w) as usize;
                        let mut spans: Vec<Span<'static>> = Vec::with_capacity(2);
                        if pad > 0 {
                            spans.push(Span::raw(" ".repeat(pad)));
                        }
                        spans.push(Span::styled(
                            chunk,
                            Style::default().fg(Color::White),
                        ));
                        lines.push(Line::from(spans));
                    }
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

/// Maximum width of a right-aligned user "bubble" given the
/// transcript inner width. Caps at 60 cols so very wide terminals
/// don't stretch user prompts edge-to-edge, with a 4-col gutter on
/// the left so the bubble visibly hugs the right side. Floors at
/// 20 cols so tiny terminals still wrap reasonably.
fn bubble_width_for(inner_width: u16) -> u16 {
    inner_width.saturating_sub(4).min(60).max(20)
}

/// Word-wrap `text` into chunks of at most `width` chars. Splits
/// on whitespace; words longer than `width` are emitted on their
/// own line (no mid-word breaking — terminal-pasted URLs survive).
fn wrap_to_width(text: &str, width: usize) -> Vec<String> {
    if width == 0 || text.chars().count() <= width {
        return vec![text.to_string()];
    }
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let word_w = word.chars().count();
        if current.is_empty() {
            current.push_str(word);
        } else if current.chars().count() + 1 + word_w <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            out.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    if out.is_empty() {
        out.push(text.to_string());
    }
    out
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

    fn empty_app() -> App {
        let mut app = App::new();
        app.transcript.clear(); // drop the welcome-system banner
        app
    }

    #[test]
    fn user_prompt_renders_you_header_with_right_bar() {
        let mut app = empty_app();
        app.transcript
            .push(TranscriptItem::UserPrompt { body: "hi".into() });
        let inner = 40u16;
        let lines = build_transcript_lines(&app, inner);
        let header = lines
            .iter()
            .find(|l| line_text(l).contains("you"))
            .expect("you header should be in output");
        let s = line_text(header);
        assert!(s.ends_with("you ▐"), "got: {:?}", s);
        assert_eq!(s.chars().count(), inner as usize);
    }

    #[test]
    fn user_prompt_body_is_right_aligned_with_2col_right_gutter() {
        let mut app = empty_app();
        app.transcript
            .push(TranscriptItem::UserPrompt { body: "hello".into() });
        let inner = 40u16;
        let lines = build_transcript_lines(&app, inner);
        let body_line = lines
            .iter()
            .find(|l| line_text(l).trim() == "hello")
            .expect("hello body line should be present");
        let s = line_text(body_line);
        assert!(s.ends_with("hello"));
        // Body sits 2 cols left of where the header's `▐` ends.
        assert_eq!(s.chars().count(), (inner - 2) as usize);
    }

    #[test]
    fn user_prompt_wraps_long_text_into_right_aligned_chunks() {
        let mut app = empty_app();
        let body =
            "this is a sufficiently long user prompt that should wrap onto more than one chunk";
        app.transcript
            .push(TranscriptItem::UserPrompt { body: body.into() });
        let inner = 40u16;
        let lines = build_transcript_lines(&app, inner);
        // Body chunks: non-empty, no rule, no header glyph.
        let chunks: Vec<&Line> = lines
            .iter()
            .filter(|l| {
                let t: String = line_text(l);
                !t.trim().is_empty() && !t.contains('─') && !t.contains("▐")
            })
            .collect();
        assert!(chunks.len() >= 2, "expected wrapping, got {} chunk(s)", chunks.len());
        for chunk in chunks {
            let s = line_text(chunk);
            assert_eq!(
                s.chars().count(),
                (inner - 2) as usize,
                "chunk not padded to inner_width-2: {:?}",
                s
            );
        }
    }

    #[test]
    fn wrap_to_width_keeps_short_text_intact() {
        assert_eq!(wrap_to_width("hello", 40), vec!["hello".to_string()]);
    }

    #[test]
    fn wrap_to_width_breaks_on_whitespace_at_width() {
        let out = wrap_to_width("one two three four five", 9);
        // greedy: "one two", "three", "four five" — chunks ≤ 9 chars
        for chunk in &out {
            assert!(chunk.chars().count() <= 9, "{:?} too long", chunk);
        }
        assert_eq!(out.join(" "), "one two three four five");
    }

    #[test]
    fn bubble_width_caps_at_60() {
        assert_eq!(bubble_width_for(200), 60);
        assert_eq!(bubble_width_for(40), 36); // 40 - 4
        assert_eq!(bubble_width_for(10), 20); // floor
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
