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
use crate::tui::image::{ImageMarker, can_render_images, parse_image_marker};
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
/// rendered structure (bands, padding gutter, etc.) without a
/// terminal. `rule_width` is the inner width of the transcript
/// pane — user-prompt bands pad to it.
pub(super) fn build_transcript_lines(app: &App, rule_width: u16) -> Vec<Line<'static>> {
    // The viewport renders the live tail only. In fullscreen
    // `committed` is always 0, so this is the whole transcript —
    // identical to the pre-rework behavior. In inline mode items
    // `[0, committed)` already live in native scrollback.
    build_transcript_lines_range(app, rule_width, app.committed..app.transcript.len())
}

/// Render a contiguous range of transcript items to lines. Used by the
/// viewport builder (`committed..len`) and the inline-mode scrollback
/// flush (`old_committed..new_committed`). `range` selects which items
/// are *emitted*.
pub(super) fn build_transcript_lines_range(
    app: &App,
    rule_width: u16,
    range: std::ops::Range<usize>,
) -> Vec<Line<'static>> {
    let theme = &app.theme;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let items = &app.transcript;
    let end = range.end.min(items.len());
    for i in range.start.min(end)..end {
        let item = &items[i];
        let is_active = app.active_assistant == Some(i);
        match item {
            TranscriptItem::UserPrompt { body } => {
                let band = Style::default().bg(theme.user_band_bg);
                let width = rule_width.max(1) as usize;
                let pad_line = || Line::from(Span::styled(" ".repeat(width), band));
                lines.push(pad_line());
                let body_w = rule_width.saturating_sub(2).max(1) as usize;
                let mut visual: Vec<String> = Vec::new();
                for body_line in body.lines() {
                    visual.extend(wrap_to_width(body_line, body_w));
                }
                if visual.is_empty() {
                    visual.push(String::new());
                }
                for (idx, chunk) in visual.iter().enumerate() {
                    let (prefix, prefix_style) = if idx == 0 {
                        (
                            "› ",
                            band.add_modifier(Modifier::BOLD).add_modifier(Modifier::DIM),
                        )
                    } else {
                        ("  ", band)
                    };
                    let used = prefix.chars().count() + chunk.chars().count();
                    let pad = width.saturating_sub(used);
                    lines.push(Line::from(vec![
                        Span::styled(prefix.to_string(), prefix_style),
                        Span::styled(chunk.clone(), band),
                        Span::styled(" ".repeat(pad), band),
                    ]));
                }
                lines.push(pad_line());
            }
            TranscriptItem::Assistant { body, .. } => {
                if body.is_empty() && is_active {
                    lines.push(Line::from(vec![
                        Span::styled("• ".to_string(), Style::default().fg(theme.dim)),
                        Span::styled(
                            "waiting for first token".to_string(),
                            Style::default().fg(theme.dim).add_modifier(Modifier::ITALIC),
                        ),
                    ]));
                } else {
                    let mut md_lines = markdown::render(body, app.markdown_theme);
                    if md_lines.is_empty() {
                        md_lines.push(Line::raw(""));
                    }
                    for (idx, md_line) in md_lines.into_iter().enumerate() {
                        let mut spans: Vec<Span<'static>> =
                            Vec::with_capacity(md_line.spans.len() + 1);
                        if idx == 0 {
                            spans.push(Span::styled(
                                "• ".to_string(),
                                Style::default().fg(theme.dim),
                            ));
                        } else {
                            spans.push(Span::raw("  "));
                        }
                        spans.extend(md_line.spans.into_iter());
                        lines.push(Line::from(spans));
                    }
                }
            }
            TranscriptItem::System { body } => {
                if let Some(msg) = body.strip_prefix("error: ") {
                    lines.push(Line::from(Span::styled(
                        format!("■ {}", msg),
                        Style::default().fg(theme.tool_err),
                    )));
                } else if body.starts_with("✔ ") {
                    lines.push(Line::from(Span::styled(
                        body.clone(),
                        Style::default().fg(theme.tool_ok),
                    )));
                } else if body.starts_with("✗ ") {
                    lines.push(Line::from(Span::styled(
                        body.clone(),
                        Style::default().fg(theme.tool_err),
                    )));
                } else {
                    for body_line in body.lines() {
                        lines.push(Line::from(Span::styled(
                            format!("  {}", body_line),
                            Style::default()
                                .fg(theme.dim)
                                .add_modifier(Modifier::ITALIC),
                        )));
                    }
                }
            }
            TranscriptItem::ToolCard {
                tool,
                args_preview,
                state,
                ..
            } => {
                let focused = app.focused_card_idx == Some(i);
                lines.push(render_tool_card_line(
                    tool,
                    args_preview,
                    state,
                    theme,
                    focused,
                ));
                lines.extend(render_tool_card_detail(tool, state, theme));
            }
        }
        lines.push(Line::raw(""));
    }
    lines
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

/// Line indices (post-layout) of the start of each user turn.
/// Detects them by the `› ` prefix span emitted only by the
/// UserPrompt band in `build_transcript_lines`.
pub(super) fn user_turn_line_indices(lines: &[Line<'_>]) -> Vec<u16> {
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if line.spans.iter().any(|s| s.content.as_ref() == "› ") {
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

/// Header line of a tool card:
/// `→ Read   src/main.rs                            0.04s ✓`
fn render_tool_card_line(
    tool: &str,
    args_preview: &str,
    state: &ToolCardState,
    theme: &Theme,
    focused: bool,
) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let arrow_color = match state {
        ToolCardState::Streaming { .. } => theme.tool_running,
        ToolCardState::Running { .. } => theme.tool_running,
        ToolCardState::Done { ok: true, .. } => theme.tool_ok,
        ToolCardState::Done { ok: false, .. } => theme.tool_err,
    };
    // Y4: focused cards get a leading ▍ sidebar in the accent color
    // — the same focus glyph used elsewhere in the TUI — so the
    // user can see which card the {/} cursor is on and `Enter`
    // will expand/collapse.
    let leader = if focused { "▍ → " } else { "  → " };
    spans.push(Span::styled(
        leader.to_string(),
        Style::default()
            .fg(if focused { theme.accent } else { arrow_color })
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

/// Detail lines under a card.
///
/// - Streaming Edit/Write cards render a 6-line peek of the new
///   content the model is mid-emitting (lenient partial-JSON
///   extraction; partial last line OK).
/// - Running cards show no detail (the call is dispatched; we
///   wait for results).
/// - Done cards show their summary indented under the header.
fn render_tool_card_detail(
    tool: &str,
    state: &ToolCardState,
    theme: &Theme,
) -> Vec<Line<'static>> {
    match state {
        ToolCardState::Streaming { accumulated_json, .. } => {
            let peek = extract_streaming_peek(tool, accumulated_json);
            peek.into_iter()
                .map(|line| {
                    Line::from(vec![
                        Span::styled(
                            "    + ".to_string(),
                            Style::default().fg(theme.diff_added),
                        ),
                        Span::styled(
                            line,
                            Style::default().fg(theme.diff_added),
                        ),
                    ])
                })
                .collect()
        }
        ToolCardState::Running { .. } => Vec::new(),
        ToolCardState::Done {
            summary,
            ok,
            full_output,
            expanded,
            ..
        } => {
            if *expanded {
                expanded_output_lines(full_output, theme, *ok)
            } else if summary.is_empty() {
                Vec::new()
            } else {
                vec![Line::from(Span::styled(
                    format!("    {}", summary),
                    Style::default()
                        .fg(if *ok { theme.dim } else { theme.diff_removed })
                        .add_modifier(Modifier::ITALIC),
                ))]
            }
        }
    }
}

/// Y4: cap an expanded card's body at 40 lines and emit each line
/// dim-indented under the header, with a trailing "+N more" hint
/// when the cap was hit. Empty output renders a single italic
/// "(no output)" line so the expand gesture isn't silent.
const EXPANDED_LINE_CAP: usize = 40;

fn expanded_output_lines(full_output: &str, theme: &Theme, ok: bool) -> Vec<Line<'static>> {
    let trimmed = full_output.trim_end_matches('\n');
    if trimmed.is_empty() {
        return vec![Line::from(Span::styled(
            "    (no output)".to_string(),
            Style::default().fg(theme.dim).add_modifier(Modifier::ITALIC),
        ))];
    }
    // Phase Y3: an `[Image: ...]` marker gets its own polished render
    // — show the basename, dimensions, and format as a chip, plus a
    // hint about the rendering path. Without `--features images` the
    // hint says how to enable the protocol; with the feature on we
    // note the protocol we'd use. Actual inline image widget
    // rendering is deferred to a follow-up (requires splitting the
    // Paragraph + Image draw at the frame level).
    if let Some(marker) = parse_image_marker(trimmed) {
        return image_marker_lines(&marker, theme);
    }
    let all: Vec<&str> = trimmed.lines().collect();
    let total = all.len();
    let body_color = if ok { theme.fg } else { theme.diff_removed };
    let mut out: Vec<Line<'static>> = all
        .iter()
        .take(EXPANDED_LINE_CAP)
        .map(|s| {
            Line::from(vec![
                Span::raw("    ".to_string()),
                Span::styled(s.to_string(), Style::default().fg(body_color)),
            ])
        })
        .collect();
    if total > EXPANDED_LINE_CAP {
        out.push(Line::from(Span::styled(
            format!("    … +{} more lines", total - EXPANDED_LINE_CAP),
            Style::default().fg(theme.dim).add_modifier(Modifier::ITALIC),
        )));
    }
    out
}

/// Render an `[Image: ...]` marker as a multi-line chip inside an
/// expanded tool card. The shape:
///
/// ```text
///     🖼  cat.png
///        1024x768 PNG
///        /abs/path/to/cat.png
///        inline render off — build with `--features images` …
/// ```
///
/// (The emoji is a deliberate exception to the project's no-emoji
/// rule: the spec calls out the image chip specifically as a visual
/// affordance.) The fourth line is a `theme.dim` hint that varies
/// with `cfg!(feature = "images")`. Future work: at the frame level,
/// reserve `image_render_rect` and call into
/// `tui::image::render::protocol_for` to draw the actual pixels.
fn image_marker_lines(marker: &ImageMarker, theme: &Theme) -> Vec<Line<'static>> {
    let basename = std::path::Path::new(&marker.path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&marker.path)
        .to_string();
    let dims_line = match marker.dims {
        Some((w, h)) => format!("    {}x{} {}", w, h, marker.format),
        None => format!("    {} (dimensions unknown)", marker.format),
    };
    let hint = if cfg!(feature = "images") {
        // The feature is on at build time, but renderer wiring is the
        // remaining piece. Be honest in the hint.
        "    images feature enabled — inline render coming soon"
    } else {
        "    inline render off — build with `--features images` to enable"
    };
    vec![
        Line::from(Span::styled(
            format!("    🖼  {}", basename),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            dims_line,
            Style::default().fg(theme.fg),
        )),
        Line::from(Span::styled(
            format!("    {}", marker.path),
            Style::default().fg(theme.dim),
        )),
        Line::from(Span::styled(
            hint.to_string(),
            Style::default().fg(theme.dim).add_modifier(Modifier::ITALIC),
        )),
    ]
}

/// Whether the inline image rendering path is active for the given
/// `graphics` kind. Thin wrapper over `tui::image::can_render_images`
/// so the renderer can ask the question without importing the image
/// module everywhere. Wired up by the frame-level integration when it
/// lands.
#[allow(dead_code)]
pub(super) fn should_render_image_inline(
    graphics: crate::tui::caps::GraphicsKind,
    _marker: &ImageMarker,
) -> bool {
    can_render_images(graphics)
}

/// Extract up to 6 lines of streamed content from partial JSON for
/// Edit/Write tool calls.
///
/// For `Edit`, pulls the `new_string` field; for `Write`, pulls
/// `content`. Tries a strict parse first (complete JSON); falls
/// back to lenient field-scan for mid-stream JSON. Returns an
/// empty Vec for any other tool or when the field isn't reachable
/// yet.
fn extract_streaming_peek(tool: &str, accumulated_json: &str) -> Vec<String> {
    let field = match tool {
        "Edit" => "new_string",
        "Write" => "content",
        _ => return Vec::new(),
    };

    let raw = match serde_json::from_str::<serde_json::Value>(accumulated_json) {
        Ok(v) => v.get(field).and_then(|f| f.as_str()).map(String::from),
        Err(_) => scan_partial_string_field(accumulated_json, field),
    };

    let Some(raw) = raw else {
        return Vec::new();
    };

    raw.lines()
        .take(6)
        .map(|s| s.to_string())
        .collect()
}

/// Lenient scan for a partial `"<field>":"..."` value in JSON
/// that hasn't finished streaming yet. Honors `\\`, `\"`, `\n`,
/// `\t`, `\r`. Returns None if the field isn't present yet (or
/// the value hasn't started). Returns the partial value if the
/// closing quote hasn't arrived — including a trailing dangling
/// backslash, which is dropped.
fn scan_partial_string_field(json: &str, field: &str) -> Option<String> {
    let needle = format!("\"{}\"", field);
    let key_pos = json.find(&needle)?;
    let after_key = &json[key_pos + needle.len()..];

    // Skip whitespace + the colon.
    let mut bytes = after_key.bytes();
    let mut rest_offset = 0usize;
    let mut saw_colon = false;
    for b in bytes.by_ref() {
        rest_offset += 1;
        match b {
            b':' if !saw_colon => saw_colon = true,
            b' ' | b'\t' | b'\n' | b'\r' => continue,
            b'"' if saw_colon => break,
            _ => return None,
        }
    }
    if !saw_colon {
        return None;
    }
    // We've consumed up through the opening `"`.
    let value_start = key_pos + needle.len() + rest_offset;
    let value_str = &json[value_start..];

    let mut out = String::with_capacity(value_str.len());
    let mut chars = value_str.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('/') => out.push('/'),
                // Unknown / unfinished escape: drop it. The stream
                // may complete it on a later chunk; on the next
                // render we'll re-extract from the longer prefix.
                Some(other) => out.push(other),
                None => break,
            }
        } else if c == '"' {
            break;
        } else {
            out.push(c);
        }
    }
    Some(out)
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

    fn empty_app() -> App {
        let mut app = App::new();
        app.transcript.clear(); // drop the welcome-system banner
        app
    }

    #[test]
    fn user_prompt_renders_full_width_banded_block() {
        let mut app = empty_app();
        app.transcript
            .push(TranscriptItem::UserPrompt { body: "hi there".into() });
        let lines = build_transcript_lines(&app, 40);
        let band = Some(app.theme.user_band_bg);
        let body = lines
            .iter()
            .find(|l| line_text(l).contains("hi there"))
            .expect("body line");
        assert_eq!(line_text(body).chars().count(), 40);
        assert!(line_text(body).starts_with("› "));
        assert!(body.spans.iter().all(|s| s.style.bg == band));
        let first = &body.spans[0];
        assert_eq!(first.content, "› ");
        assert!(first.style.add_modifier.contains(Modifier::BOLD));
        assert!(first.style.add_modifier.contains(Modifier::DIM));
        // Tinted blank pad lines above and below the message.
        let idx = lines.iter().position(|l| std::ptr::eq(l, body)).unwrap();
        for neighbor in [&lines[idx - 1], &lines[idx + 1]] {
            assert!(line_text(neighbor).trim().is_empty());
            assert!(neighbor.spans.iter().all(|s| s.style.bg == band));
        }
    }

    #[test]
    fn user_prompt_wraps_with_indented_continuations() {
        let mut app = empty_app();
        app.transcript.push(TranscriptItem::UserPrompt {
            body: "one two three four five six seven eight nine ten".into(),
        });
        let lines = build_transcript_lines(&app, 20);
        let band = Some(app.theme.user_band_bg);
        let visual: Vec<&Line> = lines
            .iter()
            .filter(|l| !line_text(l).trim().is_empty())
            .collect();
        assert!(visual.len() >= 3, "expected wrap, got {}", visual.len());
        assert!(line_text(visual[0]).starts_with("› "));
        for cont in &visual[1..] {
            assert!(line_text(cont).starts_with("  "), "got: {:?}", line_text(cont));
        }
        for l in visual {
            assert_eq!(line_text(l).chars().count(), 20);
            assert!(l.spans.iter().all(|s| s.style.bg == band));
        }
    }

    #[test]
    fn assistant_renders_bullet_prefix_without_header_or_cursor() {
        let mut app = empty_app();
        app.transcript.push(TranscriptItem::Assistant {
            body: "hello world".into(),
            done: false,
        });
        app.active_assistant = Some(0);
        let lines = build_transcript_lines(&app, 40);
        let text = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(text.contains("• hello world"), "got: {text}");
        assert!(!text.contains('▌'), "old header glyph leaked: {text}");
        assert!(!text.contains('▍'), "streaming cursor leaked: {text}");
        let first = lines
            .iter()
            .find(|l| line_text(l).contains("hello world"))
            .expect("body line");
        assert_eq!(first.spans[0].content, "• ");
        assert_eq!(first.spans[0].style.fg, Some(app.theme.dim));
    }

    #[test]
    fn assistant_waiting_placeholder_while_empty_and_active() {
        let mut app = empty_app();
        app.on_turn_started();
        let lines = build_transcript_lines(&app, 40);
        let text = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(text.contains("waiting for first token"), "got: {text}");
    }

    #[test]
    fn system_note_stays_dim_italic() {
        let mut app = empty_app();
        app.on_system_note("just a note".into());
        let lines = build_transcript_lines(&app, 40);
        let note = lines
            .iter()
            .find(|l| line_text(l).contains("just a note"))
            .expect("note line");
        assert!(line_text(note).starts_with("  "));
        assert!(note.spans[0].style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn error_note_renders_block_glyph_in_error_color() {
        let mut app = empty_app();
        app.on_turn_error("boom");
        let lines = build_transcript_lines(&app, 40);
        let err = lines
            .iter()
            .find(|l| line_text(l).contains("boom"))
            .expect("error line");
        assert!(line_text(err).starts_with("■ "), "got: {:?}", line_text(err));
        assert_eq!(err.spans[0].style.fg, Some(app.theme.tool_err));
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
        // Each index points at a line containing the `› ` band
        // prefix followed by the prompt's first chunk, confirming
        // we landed on the band's first body row.
        let expected = ["› first", "› second"];
        for (i, want) in idxs.iter().zip(expected) {
            let row = &lines[*i as usize];
            let text: String = row.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(text.contains(want), "row text was: {:?}", text);
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

    #[test]
    fn committed_watermark_hides_scrollback_items_from_viewport() {
        // Inline mode: items below the `committed` watermark have been
        // flushed to native scrollback, so the viewport builder must
        // not re-emit them (re-emitting is what orphaned stale rows).
        let mut app = App::new();
        app.transcript = vec![
            TranscriptItem::UserPrompt { body: "alpha".into() },
            TranscriptItem::Assistant { body: "beta".into(), done: true },
            TranscriptItem::UserPrompt { body: "gamma".into() },
            TranscriptItem::Assistant { body: "delta".into(), done: true },
        ];
        app.committed = 2;
        let text = build_transcript_lines(&app, 40)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!text.contains("alpha"), "scrollback item leaked: {text:?}");
        assert!(!text.contains("beta"), "scrollback item leaked: {text:?}");
        assert!(text.contains("gamma"));
        assert!(text.contains("delta"));
    }

    #[test]
    fn build_range_emits_only_requested_items() {
        // The scrollback-flush path renders just `old..new`; confirm a
        // sub-range emits only those items, not the whole transcript.
        let mut app = App::new();
        app.transcript = vec![
            TranscriptItem::UserPrompt { body: "alpha".into() },
            TranscriptItem::Assistant { body: "beta".into(), done: true },
            TranscriptItem::UserPrompt { body: "gamma".into() },
        ];
        let text = build_transcript_lines_range(&app, 40, 0..1)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("alpha"));
        assert!(!text.contains("beta"), "out-of-range item emitted: {text:?}");
        assert!(!text.contains("gamma"), "out-of-range item emitted: {text:?}");
    }

    #[test]
    fn extract_peek_returns_empty_for_non_edit_write_tools() {
        assert!(extract_streaming_peek("Read", r#"{"file_path":"x"}"#).is_empty());
        assert!(extract_streaming_peek("Bash", r#"{"command":"ls"}"#).is_empty());
    }

    #[test]
    fn extract_peek_pulls_new_string_from_complete_edit_json() {
        let json = r#"{"file_path":"a.rs","old_string":"x","new_string":"hello\nworld"}"#;
        assert_eq!(
            extract_streaming_peek("Edit", json),
            vec!["hello".to_string(), "world".to_string()],
        );
    }

    #[test]
    fn extract_peek_pulls_content_from_complete_write_json() {
        let json = r#"{"file_path":"a.rs","content":"line1\nline2\nline3"}"#;
        assert_eq!(
            extract_streaming_peek("Write", json),
            vec!["line1".to_string(), "line2".to_string(), "line3".to_string()],
        );
    }

    #[test]
    fn extract_peek_caps_at_six_lines() {
        let body: String = (1..=10).map(|i| format!("L{}\\n", i)).collect();
        let json = format!(r#"{{"content":"{}"}}"#, body);
        let lines = extract_streaming_peek("Write", &json);
        assert_eq!(lines.len(), 6);
        assert_eq!(lines[0], "L1");
        assert_eq!(lines[5], "L6");
    }

    #[test]
    fn extract_peek_handles_partial_json_mid_string() {
        // Stream cut after the value started but before the closing
        // quote — typical for input_json_delta chunks.
        let json = r#"{"file_path":"a.rs","old_string":"abc","new_string":"def\nghi"#;
        assert_eq!(
            extract_streaming_peek("Edit", json),
            vec!["def".to_string(), "ghi".to_string()],
        );
    }

    #[test]
    fn extract_peek_returns_empty_when_field_not_yet_present() {
        // Only file_path has streamed so far; new_string is absent.
        let json = r#"{"file_path":"a.rs""#;
        assert!(extract_streaming_peek("Edit", json).is_empty());
    }

    #[test]
    fn extract_peek_handles_escape_sequences() {
        // \", \\, \t, \n all decoded.
        let json = r#"{"content":"a\\b\"c\td\nlast"}"#;
        let out = extract_streaming_peek("Write", json);
        assert_eq!(out, vec!["a\\b\"c\td".to_string(), "last".to_string()]);
    }

    #[test]
    fn extract_peek_dangling_backslash_drops_safely() {
        // Stream cut mid-escape: ends with a lone '\'. Falls back to
        // lenient scan (strict parse fails on the unfinished escape);
        // the dangling '\' is dropped — next chunk will resync.
        let json = r#"{"content":"abc\"#;
        let out = extract_streaming_peek("Write", json);
        assert_eq!(out, vec!["abc".to_string()]);
    }

    #[test]
    fn render_tool_card_detail_streaming_edit_emits_peek_lines() {
        let theme = Theme::dark();
        let state = ToolCardState::Streaming {
            provider_tool_id: "tu_1".into(),
            accumulated_json: r#"{"file_path":"a.rs","new_string":"hello\nworld"}"#.into(),
        };
        let detail = render_tool_card_detail("Edit", &state, &theme);
        assert_eq!(detail.len(), 2);
        assert_eq!(line_text(&detail[0]), "    + hello");
        assert_eq!(line_text(&detail[1]), "    + world");
        // Each line uses diff_added color.
        assert!(detail[0].spans.iter().all(|s| s.style.fg == Some(theme.diff_added)));
    }

    #[test]
    fn render_tool_card_detail_streaming_write_emits_peek_lines() {
        let theme = Theme::dark();
        let state = ToolCardState::Streaming {
            provider_tool_id: "tu_2".into(),
            accumulated_json: r#"{"content":"line1\nline2"}"#.into(),
        };
        let detail = render_tool_card_detail("Write", &state, &theme);
        assert_eq!(detail.len(), 2);
        assert_eq!(line_text(&detail[0]), "    + line1");
        assert_eq!(line_text(&detail[1]), "    + line2");
    }

    #[test]
    fn render_tool_card_detail_streaming_partial_renders_what_we_have() {
        let theme = Theme::dark();
        let state = ToolCardState::Streaming {
            provider_tool_id: "tu_3".into(),
            accumulated_json: r#"{"new_string":"partial"#.into(),
        };
        let detail = render_tool_card_detail("Edit", &state, &theme);
        assert_eq!(detail.len(), 1);
        assert_eq!(line_text(&detail[0]), "    + partial");
    }

    #[test]
    fn render_tool_card_detail_running_returns_no_lines() {
        let theme = Theme::dark();
        let state = ToolCardState::Running {
            started_at: std::time::Instant::now(),
        };
        assert!(render_tool_card_detail("Edit", &state, &theme).is_empty());
    }

    #[test]
    fn render_tool_card_detail_done_still_returns_summary_line() {
        let theme = Theme::dark();
        let state = ToolCardState::Done {
            duration: Duration::from_millis(120),
            summary: "wrote 3 lines".into(),
            ok: true,
            full_output: String::new(),
            expanded: false,
        };
        let detail = render_tool_card_detail("Write", &state, &theme);
        assert_eq!(detail.len(), 1);
        assert_eq!(line_text(&detail[0]), "    wrote 3 lines");
    }

    #[test]
    fn render_tool_card_line_focused_renders_sidebar_glyph() {
        let theme = Theme::dark();
        let state = ToolCardState::Done {
            duration: Duration::from_millis(1),
            summary: "ok".into(),
            ok: true,
            full_output: String::new(),
            expanded: false,
        };
        let unfocused = render_tool_card_line("Read", "x", &state, &theme, false);
        let focused = render_tool_card_line("Read", "x", &state, &theme, true);
        assert!(line_text(&unfocused).starts_with("  → "));
        assert!(line_text(&focused).starts_with("▍ → "));
    }

    #[test]
    fn render_tool_card_detail_expanded_done_shows_full_output_indented() {
        let theme = Theme::dark();
        let state = ToolCardState::Done {
            duration: Duration::from_millis(1),
            summary: "3 lines".into(),
            ok: true,
            full_output: "line1\nline2\nline3".into(),
            expanded: true,
        };
        let detail = render_tool_card_detail("Read", &state, &theme);
        assert_eq!(detail.len(), 3);
        assert_eq!(line_text(&detail[0]), "    line1");
        assert_eq!(line_text(&detail[1]), "    line2");
        assert_eq!(line_text(&detail[2]), "    line3");
    }

    #[test]
    fn render_tool_card_detail_expanded_caps_at_40_lines_with_hint() {
        let theme = Theme::dark();
        let body: String = (1..=50)
            .map(|i| format!("L{}\n", i))
            .collect();
        let state = ToolCardState::Done {
            duration: Duration::from_millis(1),
            summary: "50 lines".into(),
            ok: true,
            full_output: body,
            expanded: true,
        };
        let detail = render_tool_card_detail("Read", &state, &theme);
        // 40 content lines + 1 hint line.
        assert_eq!(detail.len(), 41);
        assert_eq!(line_text(&detail[0]), "    L1");
        assert_eq!(line_text(&detail[39]), "    L40");
        assert_eq!(line_text(&detail[40]), "    … +10 more lines");
    }

    #[test]
    fn render_tool_card_detail_expanded_empty_output_shows_placeholder() {
        let theme = Theme::dark();
        let state = ToolCardState::Done {
            duration: Duration::from_millis(1),
            summary: "ok".into(),
            ok: true,
            full_output: String::new(),
            expanded: true,
        };
        let detail = render_tool_card_detail("Bash", &state, &theme);
        assert_eq!(detail.len(), 1);
        assert_eq!(line_text(&detail[0]), "    (no output)");
    }

    #[test]
    fn render_tool_card_detail_expanded_image_marker_renders_chip() {
        // Phase Y3: when full_output is a `[Image: ...]` marker (as
        // emitted by `Read` for image files), the expanded body
        // produces a 4-line chip — basename, dims+format, abs path,
        // and a hint about the rendering path.
        let theme = Theme::dark();
        let state = ToolCardState::Done {
            duration: Duration::from_millis(1),
            summary: "image".into(),
            ok: true,
            full_output: "[Image: /tmp/photo.png 1024x768 PNG]".into(),
            expanded: true,
        };
        let detail = render_tool_card_detail("Read", &state, &theme);
        assert_eq!(detail.len(), 4, "expected 4-line chip, got {:?}",
            detail.iter().map(line_text).collect::<Vec<_>>());
        assert!(line_text(&detail[0]).contains("photo.png"));
        assert_eq!(line_text(&detail[1]), "    1024x768 PNG");
        assert_eq!(line_text(&detail[2]), "    /tmp/photo.png");
        // Last line is the hint; content varies with cfg!(feature = "images").
        let hint = line_text(&detail[3]);
        if cfg!(feature = "images") {
            assert!(hint.contains("images feature enabled"), "got {hint}");
        } else {
            assert!(hint.contains("--features images"), "got {hint}");
        }
    }

    #[test]
    fn render_tool_card_detail_expanded_image_marker_unknown_dims() {
        // `?x?` dim markers (e.g. WebP, or a broken header) still
        // render the chip; the dims line says "dimensions unknown"
        // rather than printing `?x?`.
        let theme = Theme::dark();
        let state = ToolCardState::Done {
            duration: Duration::from_millis(1),
            summary: "image".into(),
            ok: true,
            full_output: "[Image: /a/b.jpg ?x? JPEG]".into(),
            expanded: true,
        };
        let detail = render_tool_card_detail("Read", &state, &theme);
        assert!(line_text(&detail[0]).contains("b.jpg"));
        assert!(line_text(&detail[1]).contains("dimensions unknown"));
    }

    #[test]
    fn render_tool_card_detail_expanded_image_marker_embedded_falls_through() {
        // A `[Image: ...]` substring embedded inside other text is NOT
        // an image marker — fall through to the normal line-by-line
        // expansion.
        let theme = Theme::dark();
        let state = ToolCardState::Done {
            duration: Duration::from_millis(1),
            summary: "x".into(),
            ok: true,
            full_output: "some text\n[Image: /tmp/x.png 1x1 PNG]".into(),
            expanded: true,
        };
        let detail = render_tool_card_detail("Read", &state, &theme);
        // Two literal output lines, no chip.
        assert_eq!(detail.len(), 2);
        assert_eq!(line_text(&detail[0]), "    some text");
        assert!(line_text(&detail[1]).starts_with("    [Image: "));
    }

    #[test]
    fn render_tool_card_detail_collapsed_done_still_shows_summary_when_full_output_present() {
        // The summary line stays visible while the card is
        // collapsed — full_output is hidden until the user expands.
        let theme = Theme::dark();
        let state = ToolCardState::Done {
            duration: Duration::from_millis(1),
            summary: "12 lines".into(),
            ok: true,
            full_output: "a\nb\nc".into(),
            expanded: false,
        };
        let detail = render_tool_card_detail("Read", &state, &theme);
        assert_eq!(detail.len(), 1);
        assert_eq!(line_text(&detail[0]), "    12 lines");
    }

    #[test]
    fn build_transcript_lines_renders_focused_card_with_sidebar() {
        let mut app = App::new();
        app.transcript.clear();
        app.transcript.push(TranscriptItem::ToolCard {
            tool: "Read".into(),
            args_preview: "a.rs".into(),
            state: ToolCardState::Done {
                duration: Duration::from_millis(1),
                summary: "1 line".into(),
                ok: true,
                full_output: "hello".into(),
                expanded: false,
            },
        });
        // Sanity: unfocused renders the `  → ` leader.
        let lines = build_transcript_lines(&app, 40);
        assert!(lines.iter().any(|l| line_text(l).starts_with("  → ")));
        // Focus the card and re-render: leader flips to `▍ → `.
        app.focused_card_idx = Some(0);
        let lines = build_transcript_lines(&app, 40);
        assert!(lines.iter().any(|l| line_text(l).starts_with("▍ → ")));
    }

    #[test]
    fn build_transcript_lines_renders_expanded_full_output_under_focused_card() {
        let mut app = App::new();
        app.transcript.clear();
        app.transcript.push(TranscriptItem::ToolCard {
            tool: "Read".into(),
            args_preview: "a.rs".into(),
            state: ToolCardState::Done {
                duration: Duration::from_millis(1),
                summary: "3 lines".into(),
                ok: true,
                full_output: "alpha\nbeta\ngamma".into(),
                expanded: true,
            },
        });
        let lines = build_transcript_lines(&app, 40);
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert!(texts.iter().any(|t| t == "    alpha"));
        assert!(texts.iter().any(|t| t == "    beta"));
        assert!(texts.iter().any(|t| t == "    gamma"));
        // Summary is NOT rendered when expanded — full_output replaces it.
        assert!(!texts.iter().any(|t| t == "    3 lines"));
    }
}
