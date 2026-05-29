//! Transcript pane render — turns `App.transcript` into a
//! `Vec<Line<'static>>` and ships it through ratatui's
//! `Paragraph`. Owned strings everywhere so we can mutate scroll
//! state on `App` afterwards without fighting borrows.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Padding, Paragraph, Wrap};

use crate::tui::app::{App, ToolCardState, TranscriptItem};
use crate::tui::markdown;
use crate::tui::theme::Theme;

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

    // Cache user-turn line indices so the input handler can jump
    // between them with `[` / `]` (X3). The renderer is the only
    // place that knows the post-layout line numbers, so it owns
    // this cache.
    app.turn_line_indices = user_turn_line_indices(&lines);

    // Now that the borrow on `app.transcript` is dropped, settle
    // the scroll metrics from the actual rendered line count.
    let total = lines.len() as u16;
    let height = area.height.saturating_sub(TRANSCRIPT_BOTTOM_PAD);
    let max = total.saturating_sub(height);
    app.note_scroll_metrics(max, height);
    let detached = app.is_scroll_detached();
    let unread = app.unread_lines;

    // Search overlay (X2): when active, highlight every match in
    // the rendered lines, cache the match count on App so the
    // key handler can cycle, and scroll the focused match into
    // view (overrides `app.scroll_manual` while search is open).
    let search_query = app.search().map(|s| (s.query.clone(), s.current));
    let (lines, scroll_override) = if let Some((q, cur)) = search_query.as_ref() {
        let (highlighted, match_idxs) =
            apply_search_highlight(lines, q, app.theme.match_highlight, app.theme.selected_fg);
        app.search_match_count = match_idxs.len();
        let scroll = if match_idxs.is_empty() {
            None
        } else {
            let focus = match_idxs[(*cur).min(match_idxs.len() - 1)] as u16;
            // Place the matched line a few rows below the top of
            // the pane so the user can read context above it.
            Some(focus.saturating_sub(2).min(max))
        };
        (highlighted, scroll)
    } else {
        app.search_match_count = 0;
        (lines, None)
    };
    let offset = match (scroll_override, app.scroll_manual) {
        (Some(o), _) => o,
        (None, None) => max,
        (None, Some(o)) => o.min(max),
    };

    // Chat-app anchoring: when the transcript is shorter than the
    // pane, push messages to the bottom by prepending blank lines.
    // When content overflows the pane, the existing offset = max
    // logic already pins the latest line to the bottom.
    let lines = anchor_to_bottom(lines, inner_width, height);

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
                    .fg(app.theme.selected_fg)
                    .bg(app.theme.match_highlight)
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
    let theme = &app.theme;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let items = &app.transcript;
    for (i, item) in items.iter().enumerate() {
        let is_active = app.active_assistant == Some(i);
        match item {
            TranscriptItem::UserPrompt { body } => {
                let header_text = "you ";
                let header_chars = header_text.chars().count() as u16 + 1;
                let header_pad = rule_width.saturating_sub(header_chars) as usize;
                let mut header_spans: Vec<Span<'static>> = Vec::with_capacity(3);
                if header_pad > 0 {
                    header_spans.push(Span::raw(" ".repeat(header_pad)));
                }
                header_spans.push(Span::styled(
                    header_text.to_string(),
                    Style::default()
                        .fg(theme.user)
                        .add_modifier(Modifier::BOLD),
                ));
                header_spans.push(Span::styled(
                    "▐".to_string(),
                    Style::default()
                        .fg(theme.user)
                        .add_modifier(Modifier::BOLD),
                ));
                lines.push(Line::from(header_spans));

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
                        spans.push(Span::styled(chunk, Style::default().fg(theme.fg)));
                        lines.push(Line::from(spans));
                    }
                }
            }
            TranscriptItem::Assistant { body, done } => {
                lines.push(Line::from(Span::styled(
                    "▌ oli",
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                )));
                if body.is_empty() && is_active {
                    lines.push(Line::from(Span::styled(
                        "  · waiting for first token",
                        Style::default()
                            .fg(theme.dim)
                            .add_modifier(Modifier::ITALIC),
                    )));
                } else {
                    for md_line in markdown::render(body, app.markdown_theme) {
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
                            .fg(theme.accent)
                            .add_modifier(Modifier::SLOW_BLINK),
                    ));
                }
            }
            TranscriptItem::System { body } => {
                for body_line in body.lines() {
                    lines.push(Line::from(Span::styled(
                        format!("  {}", body_line),
                        Style::default()
                            .fg(theme.dim)
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
                lines.push(render_tool_card_line(tool, args_preview, state, theme));
                if let Some(detail) = render_tool_card_detail(state, theme) {
                    lines.push(detail);
                }
            }
        }
        let next = items.get(i + 1);
        let user_to_assistant = matches!(item, TranscriptItem::UserPrompt { .. })
            && matches!(next, Some(TranscriptItem::Assistant { .. }));
        if user_to_assistant {
            lines.push(separator_rule(rule_width, theme));
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

/// Conservative visual-row count estimate that accounts for
/// `Wrap { trim: false }`. Each line takes at least 1 row; longer
/// lines take ceil(chars/inner_width) rows. Word-wrap may break
/// earlier than this, so the estimate is an *upper bound*: safe
/// for "does content fit?" decisions.
fn visual_line_count(lines: &[Line<'_>], inner_width: u16) -> u16 {
    let w = inner_width.max(1);
    lines
        .iter()
        .map(|l| {
            let chars: u16 = l
                .spans
                .iter()
                .map(|s| s.content.chars().count() as u16)
                .sum();
            ((chars + w - 1) / w).max(1)
        })
        .sum()
}

/// Bottom-anchor the rendered lines: when the rendered content is
/// shorter than the visible inner height, prepend blank lines so
/// the latest message sits flush against the bottom (chat-app
/// convention). When content overflows, return as-is — the
/// existing `Paragraph::scroll((offset, 0))` math already pins the
/// last line to the bottom.
pub(super) fn anchor_to_bottom(
    mut lines: Vec<Line<'static>>,
    inner_width: u16,
    inner_height: u16,
) -> Vec<Line<'static>> {
    let visual = visual_line_count(&lines, inner_width);
    if visual < inner_height {
        let pad = (inner_height - visual) as usize;
        let mut padded = Vec::with_capacity(lines.len() + pad);
        for _ in 0..pad {
            padded.push(Line::raw(""));
        }
        padded.append(&mut lines);
        return padded;
    }
    lines
}

/// Re-paint every `Line` so case-insensitive substring matches of
/// Line indices (post-layout) of user-turn headers. Detects them
/// by the trailing `▐` glyph emitted only by the UserPrompt header
/// in `build_transcript_lines`. Returned indices match the row
/// offset in the laid-out transcript and are clamped to `u16` —
/// `scroll_manual` is `u16` too, so callers don't need to widen.
pub(super) fn user_turn_line_indices(lines: &[Line<'_>]) -> Vec<u16> {
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if line.spans.iter().any(|s| s.content.as_ref() == "▐") {
            out.push(i as u16);
        }
    }
    out
}

/// `needle` are styled with `bg = highlight` / `fg = highlight_fg`.
/// Returns the recolored lines and a vector of line indices that
/// contained at least one match. Empty `needle` short-circuits to
/// `(lines, vec![])` — search inactive, no recoloring.
pub(super) fn apply_search_highlight(
    lines: Vec<Line<'static>>,
    needle: &str,
    highlight: ratatui::style::Color,
    highlight_fg: ratatui::style::Color,
) -> (Vec<Line<'static>>, Vec<usize>) {
    if needle.is_empty() {
        return (lines, Vec::new());
    }
    let needle_lc = needle.to_lowercase();
    let mut match_idxs = Vec::new();
    let out: Vec<Line<'static>> = lines
        .into_iter()
        .enumerate()
        .map(|(line_idx, line)| {
            // Concatenate the line's text for a single
            // substring scan. If it doesn't match, return the
            // line untouched — preserves the original styling
            // and avoids redundant Span splits.
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            if !text.to_lowercase().contains(&needle_lc) {
                return line;
            }
            match_idxs.push(line_idx);
            highlight_line_spans(line, &needle_lc, highlight, highlight_fg)
        })
        .collect();
    (out, match_idxs)
}

/// Split a line's spans so every case-insensitive substring
/// occurrence of `needle_lc` gets the highlight style applied. The
/// non-match runs keep their original style.
fn highlight_line_spans(
    line: Line<'static>,
    needle_lc: &str,
    highlight: ratatui::style::Color,
    highlight_fg: ratatui::style::Color,
) -> Line<'static> {
    let hl_style = Style::default()
        .bg(highlight)
        .fg(highlight_fg)
        .add_modifier(Modifier::BOLD);

    let mut new_spans: Vec<Span<'static>> = Vec::new();
    for span in line.spans {
        let span_text: &str = span.content.as_ref();
        let span_lc = span_text.to_lowercase();
        // ASCII (the overwhelming common case) preserves byte
        // positions through `to_lowercase`. If the lowercased
        // span's byte length changed (a non-ASCII codepoint
        // folded to a different-width form), bail on the
        // highlight for this span — keep it as-is to avoid
        // slicing at a non-char-boundary.
        if span_lc.len() != span_text.len() {
            new_spans.push(span);
            continue;
        }
        let mut cursor = 0usize;
        while cursor < span_text.len() {
            match span_lc[cursor..].find(needle_lc) {
                None => {
                    new_spans.push(Span::styled(
                        span_text[cursor..].to_string(),
                        span.style,
                    ));
                    break;
                }
                Some(rel) => {
                    let start = cursor + rel;
                    let end = start + needle_lc.len();
                    if start > cursor {
                        new_spans.push(Span::styled(
                            span_text[cursor..start].to_string(),
                            span.style,
                        ));
                    }
                    new_spans.push(Span::styled(
                        span_text[start..end].to_string(),
                        hl_style,
                    ));
                    cursor = end;
                }
            }
        }
    }
    Line::from(new_spans)
}

/// Dim horizontal rule used between user→assistant turn
/// boundaries. Width is the inner width of the transcript pane so
/// the rule visually spans the full gutter.
fn separator_rule(width: u16, theme: &Theme) -> Line<'static> {
    let glyphs: String = std::iter::repeat('─').take(width.max(1) as usize).collect();
    Line::from(Span::styled(glyphs, Style::default().fg(theme.dim)))
}

/// Header line of a tool card:
/// `→ Read   src/main.rs                            0.04s ✓`
fn render_tool_card_line(
    tool: &str,
    args_preview: &str,
    state: &ToolCardState,
    theme: &Theme,
) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let arrow_color = match state {
        ToolCardState::Streaming { .. } => theme.tool_running,
        ToolCardState::Running { .. } => theme.tool_running,
        ToolCardState::Done { ok: true, .. } => theme.tool_ok,
        ToolCardState::Done { ok: false, .. } => theme.tool_err,
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
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        args_preview.to_string(),
        Style::default().fg(theme.fg),
    ));
    spans.push(Span::raw("  "));
    match state {
        ToolCardState::Streaming { .. } => {
            // Card is mid-stream — show a single static glyph + the
            // word "streaming". No timer because the call hasn't been
            // dispatched yet; PreToolUse hasn't fired.
            spans.push(Span::styled(
                "⠿ streaming…",
                Style::default().fg(theme.tool_running),
            ));
        }
        ToolCardState::Running { started_at } => {
            let elapsed = started_at.elapsed().as_secs_f32();
            spans.push(Span::styled(
                format!("{} {:.1}s", spinner_glyph(elapsed), elapsed),
                Style::default().fg(theme.tool_running),
            ));
        }
        ToolCardState::Done { duration, ok, .. } => {
            spans.push(Span::styled(
                format!("{:.2}s", duration.as_secs_f32()),
                Style::default().fg(theme.dim),
            ));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                if *ok { "✓" } else { "✗" },
                Style::default().fg(if *ok { theme.tool_ok } else { theme.tool_err }),
            ));
        }
    }
    Line::from(spans)
}

/// Optional detail line under a card. Running cards show no
/// detail; done cards show their summary indented under the
/// header.
fn render_tool_card_detail(state: &ToolCardState, theme: &Theme) -> Option<Line<'static>> {
    match state {
        // Slice 3 swaps this branch for a 6-line diff peek. For now,
        // streaming cards render with the header only.
        ToolCardState::Streaming { .. } => None,
        ToolCardState::Running { .. } => None,
        ToolCardState::Done { summary, ok, .. } => {
            if summary.is_empty() {
                return None;
            }
            Some(Line::from(Span::styled(
                format!("    {}", summary),
                Style::default()
                    .fg(if *ok { theme.dim } else { theme.diff_removed })
                    .add_modifier(Modifier::ITALIC),
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::App;
    use ratatui::style::Color;
    use std::time::Duration;

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn apply_search_highlight_empty_query_is_noop() {
        let lines = vec![Line::from(Span::raw("hello world".to_string()))];
        let (out, idxs) =
            apply_search_highlight(lines.clone(), "", Color::Yellow, Color::Black);
        assert!(idxs.is_empty());
        assert_eq!(line_text(&out[0]), "hello world");
    }

    #[test]
    fn apply_search_highlight_finds_substring_case_insensitive() {
        let lines = vec![
            Line::from(Span::raw("a panic happened".to_string())),
            Line::from(Span::raw("ok".to_string())),
            Line::from(Span::raw("PANIC again".to_string())),
        ];
        let (out, idxs) =
            apply_search_highlight(lines, "panic", Color::Yellow, Color::Black);
        assert_eq!(idxs, vec![0, 2]);
        // The needle text round-trips and shares a single span
        // with the highlight style.
        let hl_present = |line: &Line<'_>| {
            line.spans.iter().any(|s| {
                s.style.bg == Some(Color::Yellow)
                    && s.content.eq_ignore_ascii_case("panic")
            })
        };
        assert!(hl_present(&out[0]));
        assert!(hl_present(&out[2]));
        // Untouched line keeps its original (single) span.
        assert_eq!(out[1].spans.len(), 1);
    }

    #[test]
    fn apply_search_highlight_splits_spans_across_styled_runs() {
        // "hel" + "lo world" — a needle "ello" crosses both spans
        // *texts* but each span is matched independently. So
        // "hel" has no match; "lo world" has no "ello" either.
        // For a true cross-span match we'd need pre-concatenated
        // text; the current implementation explicitly scopes
        // matches inside a single span. Document that here.
        let line = Line::from(vec![
            Span::styled("hel".to_string(), Style::default().fg(Color::Red)),
            Span::styled("lo world".to_string(), Style::default().fg(Color::Blue)),
        ]);
        let (out, idxs) = apply_search_highlight(
            vec![line],
            "ello",
            Color::Yellow,
            Color::Black,
        );
        // The whole line *is* counted as a match (the
        // concatenated text contains 'ello'), but no span got
        // re-styled because no individual span contains it.
        assert_eq!(idxs, vec![0]);
        assert!(
            out[0]
                .spans
                .iter()
                .all(|s| s.style.bg != Some(Color::Yellow))
        );
    }

    #[test]
    fn apply_search_highlight_recolors_within_a_single_span() {
        let line = Line::from(Span::styled(
            "before MATCH after".to_string(),
            Style::default().fg(Color::Red),
        ));
        let (out, idxs) =
            apply_search_highlight(vec![line], "match", Color::Yellow, Color::Black);
        assert_eq!(idxs, vec![0]);
        // Three spans now: "before ", "MATCH", " after".
        let texts: Vec<&str> = out[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(texts, vec!["before ", "MATCH", " after"]);
        assert_eq!(out[0].spans[1].style.bg, Some(Color::Yellow));
        // Non-match spans keep their original fg.
        assert_eq!(out[0].spans[0].style.fg, Some(Color::Red));
        assert_eq!(out[0].spans[2].style.fg, Some(Color::Red));
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
    fn anchor_to_bottom_prepends_blanks_when_content_fits() {
        let content: Vec<Line<'static>> = vec![
            Line::raw("hello".to_string()),
            Line::raw("world".to_string()),
        ];
        let result = anchor_to_bottom(content, 40, 10);
        assert_eq!(result.len(), 10);
        // First 8 rows are blank padding…
        for i in 0..8 {
            assert_eq!(line_text(&result[i]), "", "row {} should be blank", i);
        }
        // …then real content sits at the bottom.
        assert_eq!(line_text(&result[8]), "hello");
        assert_eq!(line_text(&result[9]), "world");
    }

    #[test]
    fn anchor_to_bottom_passes_through_when_content_overflows() {
        let content: Vec<Line<'static>> = (0..20)
            .map(|i| Line::raw(format!("line-{}", i)))
            .collect();
        let result = anchor_to_bottom(content.clone(), 40, 10);
        assert_eq!(result.len(), content.len());
        assert_eq!(line_text(&result[0]), "line-0");
    }

    #[test]
    fn anchor_to_bottom_accounts_for_wrapping_in_long_lines() {
        // A 100-char line at width 40 wraps to 3 rows; combined
        // with one short line, visual height = 4. Pane height 10
        // should leave 6 blank rows at the top.
        let long: String = "x".repeat(100);
        let content: Vec<Line<'static>> = vec![
            Line::raw(long.clone()),
            Line::raw("short".to_string()),
        ];
        let result = anchor_to_bottom(content, 40, 10);
        // 6 blank rows + 1 long line + 1 short line = 8 entries
        // (the long line stays as one Line; wrap happens at render
        // time but our estimate accounts for the 3-row footprint).
        assert_eq!(result.len(), 8);
        for i in 0..6 {
            assert_eq!(line_text(&result[i]), "");
        }
        assert_eq!(line_text(&result[6]), long);
        assert_eq!(line_text(&result[7]), "short");
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

    #[test]
    fn user_turn_indices_match_each_user_prompt_header() {
        let mut app = App::new();
        app.transcript
            .push(TranscriptItem::UserPrompt { body: "first".into() });
        app.transcript.push(TranscriptItem::Assistant {
            body: "a".into(),
            done: true,
        });
        app.transcript.push(TranscriptItem::UserPrompt {
            body: "second".into(),
        });
        app.transcript.push(TranscriptItem::Assistant {
            body: "b".into(),
            done: true,
        });
        let lines = build_transcript_lines(&app, 40);
        let idxs = user_turn_line_indices(&lines);
        // Two user turns → two indices.
        assert_eq!(idxs.len(), 2);
        // Each index points at a line containing "you " (the user
        // header text), confirming we landed on the header row.
        for i in &idxs {
            let row = &lines[*i as usize];
            let text: String = row.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(text.contains("you "), "row text was: {:?}", text);
        }
        // Indices are strictly increasing.
        assert!(idxs[0] < idxs[1]);
    }

    #[test]
    fn user_turn_indices_returns_empty_when_no_user_prompts() {
        let mut app = App::new();
        app.transcript.push(TranscriptItem::Assistant {
            body: "a".into(),
            done: true,
        });
        let lines = build_transcript_lines(&app, 40);
        assert!(user_turn_line_indices(&lines).is_empty());
    }
}
