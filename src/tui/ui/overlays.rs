//! Per-overlay render code. One `pub(super) fn draw_*` per
//! variant of `tui::app::Overlay`, plus the wizard helpers and
//! the diff-line styling used by the approval modal.
//!
//! Layout is consistently a centered modal with a top-left
//! titled border, an inner body, and (where useful) a one-line
//! legend at the bottom.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Wrap};

use crate::tui::app::{
    APPROVAL_OPTIONS, App, ApprovalState, CopyFallbackState, HelpBrowserState, HistorySearchState,
    InlineHelpState, SessionsPickerState,
};
use crate::tui::theme::Theme;
use crate::tui::wizard::{DaemonStatus, PullStatus, WizardProvider, WizardState, WizardStep};

use super::{centered_rect, clip_to_width};

/// Height of the inline approval pane for the given preview.
/// Preview is capped at 10 rows; the pane never eats the whole
/// frame below 3 transcript rows, and never shrinks below 10.
pub(super) fn approval_pane_height(state: &ApprovalState, area: Rect) -> u16 {
    let preview_rows = state.preview.lines().count().clamp(3, 10) as u16;
    let reason_rows = u16::from(!state.reason.is_empty());
    // title + blank + reason? + preview + blank + options + blank + hint
    let want = 2 + reason_rows + preview_rows + 1 + APPROVAL_OPTIONS.len() as u16 + 2;
    want.min(area.height.saturating_sub(3)).max(10)
}

/// Pure line builder for the inline approval pane (Codex shape):
/// bold question, optional italic reason, tinted scrollable preview,
/// numbered options with a `›` selection accent, dim confirm hint.
pub(super) fn approval_pane_lines(
    state: &ApprovalState,
    preview_rows: usize,
    inner_w: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let title = match state.tool.to_ascii_lowercase().as_str() {
        "edit" | "multiedit" | "write" => "Would you like to make the following edits?",
        _ => "Would you like to run the following command?",
    };
    let mut lines: Vec<Line<'static>> = vec![
        Line::from(Span::styled(
            format!(" {}", title),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
    ];
    if !state.reason.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(" Reason: ".to_string(), Style::default().fg(theme.dim)),
            Span::styled(
                state.reason.clone(),
                Style::default().add_modifier(Modifier::ITALIC),
            ),
        ]));
    }
    let total = state.preview.lines().count();
    let scroll = (state.scroll as usize).min(total.saturating_sub(preview_rows));
    for l in state.preview.lines().skip(scroll).take(preview_rows) {
        lines.push(styled_diff_line(l, inner_w, theme));
    }
    lines.push(Line::raw(""));
    for (i, (label, key)) in APPROVAL_OPTIONS.iter().enumerate() {
        let selected = i == state.selected;
        let prefix = if selected { "› " } else { "  " };
        let style = if selected {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg)
        };
        lines.push(Line::from(Span::styled(
            format!("{}{}. {} ({})", prefix, i + 1, label, key),
            style,
        )));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "  Press enter to confirm or esc to cancel".to_string(),
        Style::default()
            .fg(theme.dim)
            .add_modifier(Modifier::ITALIC),
    )));
    lines
}

/// Inline bottom-pane approval (replaces the old centered modal).
/// Sizes the preview window to whatever rows are left after the
/// fixed chrome (title/reason/options/hint) so long diffs scroll.
pub(super) fn draw_approval_pane(f: &mut Frame, area: Rect, approval: &ApprovalState, app: &App) {
    let preview_rows = (area.height as usize)
        .saturating_sub(2 + usize::from(!approval.reason.is_empty()) + 1 + APPROVAL_OPTIONS.len() + 2)
        .max(1);
    let inner_w = area.width.saturating_sub(2);
    let lines = approval_pane_lines(approval, preview_rows, inner_w, &app.theme);
    let pane = Paragraph::new(lines).block(Block::default().padding(Padding::horizontal(1)));
    f.render_widget(pane, area);
}

/// Codex-style diff row: sign-colored fg over a full-width bg tint
/// for +/- lines; context lines stay dim and untinted. `width` pads
/// the tint to the pane's inner width. Input is
/// `policy::render_unified_diff`'s `    + body` / `    - body` /
/// `      body` format.
pub(super) fn styled_diff_line(line: &str, width: u16, theme: &Theme) -> Line<'static> {
    // The sign lives in a fixed column (`    + ` / `    - `, four
    // leading spaces); context rows carry six. Match the column, not
    // the first non-space char, so a context line whose body starts
    // with `+`/`-` doesn't get mis-tinted.
    let (body_style, bg) = if line.starts_with("    + ") {
        (Style::default().fg(theme.diff_added), Some(theme.diff_add_bg))
    } else if line.starts_with("    - ") {
        (Style::default().fg(theme.diff_removed), Some(theme.diff_del_bg))
    } else {
        (Style::default().fg(theme.dim), None)
    };
    let pad = (width as usize).saturating_sub(line.chars().count());
    match bg {
        Some(bg) => Line::from(vec![
            Span::styled(line.to_string(), body_style.bg(bg)),
            Span::styled(" ".repeat(pad), Style::default().bg(bg)),
        ]),
        None => Line::from(Span::styled(line.to_string(), body_style)),
    }
}

/// `/sessions` picker overlay. Centered modal with a single
/// list pane; arrow keys navigate, Enter copies the resume
/// command, Esc closes.
pub(super) fn draw_sessions_picker(
    f: &mut Frame,
    full_area: Rect,
    picker: &SessionsPickerState,
    theme: &Theme,
) {
    let modal = centered_rect(full_area, 70, 60).intersection(full_area);
    f.render_widget(Clear, modal);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.dim))
        .title(Line::from(Span::styled(
            " /sessions  (↑↓ select · Enter copy `--resume` cmd · Esc close) ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(modal);
    f.render_widget(block, modal);

    if picker.entries.is_empty() {
        let p = Paragraph::new(Line::from(Span::styled(
            "  (no prior sessions yet)",
            Style::default()
                .fg(theme.dim)
                .add_modifier(Modifier::ITALIC),
        )));
        f.render_widget(p, inner);
        return;
    }

    // Scroll the visible window so the selection stays in view.
    let visible = inner.height as usize;
    let start = picker
        .selected
        .saturating_sub(visible.saturating_sub(1))
        .min(picker.entries.len().saturating_sub(visible));
    let lines: Vec<Line<'static>> = picker
        .entries
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .map(|(i, row)| {
            let selected = i == picker.selected;
            let style = if selected {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.fg)
            };
            Line::from(Span::styled(
                if selected {
                    format!("› {}", row.label)
                } else {
                    format!("  {}", row.label)
                },
                style,
            ))
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

/// `/help` browser overlay. Two-pane: command list left, full
/// description right. Arrow keys cycle, Esc / Enter closes.
pub(super) fn draw_help_browser(
    f: &mut Frame,
    full_area: Rect,
    browser: &HelpBrowserState,
    theme: &Theme,
) {
    let modal = centered_rect(full_area, 80, 70).intersection(full_area);
    f.render_widget(Clear, modal);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.dim))
        .title(Line::from(Span::styled(
            " /help  (↑↓ select · Esc close) ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(modal);
    f.render_widget(block, modal);

    if browser.entries.is_empty() {
        let p = Paragraph::new(Line::from(Span::raw("  (no commands registered)")));
        f.render_widget(p, inner);
        return;
    }

    let split = Layout::horizontal([Constraint::Length(20), Constraint::Min(20)]).split(inner);
    let list_area = split[0];
    let detail_area = split[1];

    let visible = list_area.height as usize;
    let start = browser
        .selected
        .saturating_sub(visible.saturating_sub(1))
        .min(browser.entries.len().saturating_sub(visible));
    let list_lines: Vec<Line<'static>> = browser
        .entries
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .map(|(i, (name, _))| {
            let selected = i == browser.selected;
            let style = if selected {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.fg)
            };
            Line::from(Span::styled(
                if selected {
                    format!("› /{}", name)
                } else {
                    format!("  /{}", name)
                },
                style,
            ))
        })
        .collect();
    f.render_widget(Paragraph::new(list_lines), list_area);

    if let Some((name, desc)) = browser.entries.get(browser.selected) {
        let mut detail_lines: Vec<Line<'static>> = Vec::new();
        detail_lines.push(Line::from(Span::styled(
            format!("/{}", name),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )));
        detail_lines.push(Line::raw(""));
        for body_line in desc.lines() {
            detail_lines.push(Line::from(Span::styled(
                body_line.to_string(),
                Style::default().fg(theme.fg),
            )));
        }
        let p = Paragraph::new(detail_lines).wrap(Wrap { trim: false });
        f.render_widget(p, detail_area);
    }
}

/// `/<cmd> ?` one-shot help card. Smaller modal than the full
/// browser; fades on the next keystroke.
pub(super) fn draw_inline_help(
    f: &mut Frame,
    full_area: Rect,
    card: &InlineHelpState,
    theme: &Theme,
) {
    let modal = centered_rect(full_area, 60, 30).intersection(full_area);
    f.render_widget(Clear, modal);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.dim))
        .title(Line::from(Span::styled(
            format!(" /{} ", card.name),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(modal);
    f.render_widget(block, modal);

    let mut lines: Vec<Line<'static>> = Vec::new();
    for body_line in card.description.lines() {
        lines.push(Line::from(Span::styled(
            body_line.to_string(),
            Style::default().fg(theme.fg),
        )));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "(press any key to close)".to_string(),
        Style::default()
            .fg(theme.dim)
            .add_modifier(Modifier::ITALIC),
    )));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// Ctrl-R history search overlay. Top row shows the current
/// query (with a cursor block); the rest is a scrolling list of
/// matches, newest-first, highlighting the selected row.
pub(super) fn draw_history_search(
    f: &mut Frame,
    full_area: Rect,
    search: &HistorySearchState,
    app: &App,
) {
    let theme = &app.theme;
    let modal = centered_rect(full_area, 70, 60).intersection(full_area);
    f.render_widget(Clear, modal);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.dim))
        .title(Line::from(Span::styled(
            " (i-search) ↑↓ select · Enter load · Esc cancel ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(modal);
    f.render_widget(block, modal);

    let parts = Layout::vertical([Constraint::Length(2), Constraint::Min(3)]).split(inner);

    // Query row.
    let query_line = Line::from(vec![
        Span::styled("  search: ", Style::default().fg(theme.dim)),
        Span::styled(
            search.query.clone(),
            Style::default()
                .fg(theme.fg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "▍",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::SLOW_BLINK),
        ),
    ]);
    f.render_widget(Paragraph::new(query_line), parts[0]);

    if search.matches.is_empty() {
        let hint = if search.query.is_empty() {
            "  (history is empty)"
        } else {
            "  no matches"
        };
        let p = Paragraph::new(Line::from(Span::styled(
            hint,
            Style::default()
                .fg(theme.dim)
                .add_modifier(Modifier::ITALIC),
        )));
        f.render_widget(p, parts[1]);
        return;
    }

    let visible = parts[1].height as usize;
    let start = search
        .selected
        .saturating_sub(visible.saturating_sub(1))
        .min(search.matches.len().saturating_sub(visible));
    let lines: Vec<Line<'static>> = search
        .matches
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .map(|(i, &history_idx)| {
            let body = app
                .history
                .get(history_idx)
                .map(|s| s.replace('\n', " ⏎ "))
                .unwrap_or_default();
            let body = clip_to_width(&body, parts[1].width.saturating_sub(4) as usize);
            let selected = i == search.selected;
            let style = if selected {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.fg)
            };
            Line::from(Span::styled(
                if selected {
                    format!("› {}", body)
                } else {
                    format!("  {}", body)
                },
                style,
            ))
        })
        .collect();
    f.render_widget(Paragraph::new(lines), parts[1]);
}

/// `/copy N` fallback modal. Shown when the host doesn't support
/// OSC52 (Phase W4): we render the verbatim message body inside a
/// centered modal and instruct the user to select + copy via the
/// host's own selection affordances. PgUp/PgDn scroll the body
/// for long messages; any other key dismisses.
pub(super) fn draw_copy_fallback(
    f: &mut Frame,
    full_area: Rect,
    state: &CopyFallbackState,
    theme: &Theme,
) {
    let modal = centered_rect(full_area, 80, 70).intersection(full_area);
    f.render_widget(Clear, modal);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.dim))
        .title(Line::from(vec![
            Span::styled(
                " 📋 copy below ",
                Style::default()
                    .fg(theme.match_highlight)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" /copy {} ", state.index),
                Style::default()
                    .fg(theme.selected_fg)
                    .bg(theme.match_highlight)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
    let inner = block.inner(modal);
    f.render_widget(block, modal);

    // Layout: explanatory header (3 lines), body (flex), legend
    // (1 line).
    let parts = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(3),
        Constraint::Length(2),
    ])
    .split(inner);

    let header_lines = vec![
        Line::from(Span::styled(
            format!(
                "  Your terminal ({}) blocked OSC52 — select the text below and copy with your terminal's shortcut.",
                state.host_hint
            ),
            Style::default().fg(theme.fg),
        )),
        Line::from(Span::styled(
            "  ([ui].osc52 = \"on\" forces OSC52; \"off\" keeps this fallback.)".to_string(),
            Style::default()
                .fg(theme.dim)
                .add_modifier(Modifier::ITALIC),
        )),
    ];
    f.render_widget(Paragraph::new(header_lines), parts[0]);

    let body_lines: Vec<Line<'static>> = state
        .body
        .lines()
        .map(|l| Line::from(Span::styled(l.to_string(), Style::default().fg(theme.fg))))
        .collect();
    let body = Paragraph::new(body_lines)
        .wrap(Wrap { trim: false })
        .scroll((state.scroll, 0));
    f.render_widget(body, parts[1]);

    let legend = Paragraph::new(Line::from(Span::styled(
        "  [PgUp/Dn] scroll  [Esc / any other key] close".to_string(),
        Style::default()
            .fg(theme.dim)
            .add_modifier(Modifier::ITALIC),
    )));
    f.render_widget(legend, parts[2]);
}

/// First-run setup wizard overlay. Multi-step modal:
/// Welcome → PickProvider → (EnterApiKey if applicable) →
/// Confirm → Saved. Esc closes the wizard at any point.
pub(super) fn draw_wizard(f: &mut Frame, full_area: Rect, w: &WizardState, theme: &Theme) {
    let modal = centered_rect(full_area, 80, 70).intersection(full_area);
    f.render_widget(Clear, modal);

    let title = match &w.step {
        WizardStep::Welcome => " Setup  Welcome ",
        WizardStep::PickProvider => " Setup  Choose provider ",
        WizardStep::CheckDaemon => " Setup  Check Ollama daemon ",
        WizardStep::PullModel => " Setup  Pull model ",
        WizardStep::EnterApiKey => " Setup  API key ",
        WizardStep::Confirm => " Setup  Confirm ",
        WizardStep::Saved { .. } => " Setup  Saved ",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.dim))
        .title(Line::from(Span::styled(
            title,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(modal);
    f.render_widget(block, modal);

    let parts =
        Layout::vertical([Constraint::Min(3), Constraint::Length(2)]).split(inner);

    let (body_lines, legend) = match &w.step {
        WizardStep::Welcome => (welcome_lines(), "  [Enter] continue   [Esc] skip"),
        WizardStep::PickProvider => (
            provider_lines(w),
            "  [↑↓] choose   [Enter] continue   [Esc] cancel",
        ),
        WizardStep::CheckDaemon => (
            check_daemon_lines(w),
            "  [R] retry probe   [Enter] continue   [Backspace] back   [Esc] cancel",
        ),
        WizardStep::PullModel => (
            pull_model_lines(w),
            pull_model_legend(w),
        ),
        WizardStep::EnterApiKey => (
            api_key_lines(w),
            "  type your key   [Enter] continue   [Esc] cancel",
        ),
        WizardStep::Confirm => (
            confirm_lines(w),
            "  [Enter] save   [Backspace] back   [Esc] cancel",
        ),
        WizardStep::Saved { path } => (
            vec![
                Line::raw(""),
                Line::from(Span::styled(
                    format!("  ✅ wrote {}", path.display()),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::raw(""),
                Line::from(Span::styled(
                    "  Restart `oli` to use the new config.".to_string(),
                    Style::default().fg(Color::White),
                )),
            ],
            "  press any key to dismiss",
        ),
    };
    let body = Paragraph::new(body_lines).wrap(Wrap { trim: false });
    f.render_widget(body, parts[0]);

    let legend_para = Paragraph::new(Line::from(Span::styled(
        legend.to_string(),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    )));
    f.render_widget(legend_para, parts[1]);
}

fn welcome_lines() -> Vec<Line<'static>> {
    vec![
        Line::raw(""),
        Line::from(Span::styled(
            "  Welcome to oli!".to_string(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
        Line::from(Span::styled(
            "  No config file at ~/.config/oli/config.toml. Let's set one up.".to_string(),
            Style::default().fg(Color::White),
        )),
        Line::raw(""),
        Line::from(Span::styled(
            "  Three steps: pick a provider, paste an API key (skipped for Ollama),".to_string(),
            Style::default().fg(Color::White),
        )),
        Line::from(Span::styled(
            "  and confirm. Esc skips at any point — you can edit the file by hand later.".to_string(),
            Style::default().fg(Color::White),
        )),
    ]
}

fn provider_lines(w: &WizardState) -> Vec<Line<'static>> {
    let mut out = vec![
        Line::raw(""),
        Line::from(Span::styled(
            "  Pick a provider:".to_string(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
    ];
    let current = w.current_provider();
    for p in WizardProvider::all() {
        let selected = p == current;
        let style = if selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        out.push(Line::from(Span::styled(
            if selected {
                format!("  › {}", p.label())
            } else {
                format!("    {}", p.label())
            },
            style,
        )));
    }
    out
}

fn api_key_lines(w: &WizardState) -> Vec<Line<'static>> {
    let masked: String = w.api_key.chars().map(|_| '•').collect();
    let display = if masked.is_empty() {
        "(empty)".to_string()
    } else {
        masked
    };
    vec![
        Line::raw(""),
        Line::from(Span::styled(
            format!("  Provider: {}", w.current_provider().label()),
            Style::default().fg(Color::DarkGray),
        )),
        Line::raw(""),
        Line::from(vec![
            Span::styled(
                "  API key: ",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(display, Style::default().fg(Color::White)),
            Span::styled(
                "▍",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::SLOW_BLINK),
            ),
        ]),
        Line::raw(""),
        Line::from(Span::styled(
            "  (input is masked; characters are hidden behind • bullets)".to_string(),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )),
    ]
}

fn confirm_lines(w: &WizardState) -> Vec<Line<'static>> {
    let mut out = vec![
        Line::raw(""),
        Line::from(Span::styled(
            "  About to save the following config:".to_string(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
    ];
    // Render the rendered TOML body, masking the api_key line so
    // a screen reader / over-the-shoulder peeker doesn't see it.
    let body = w.render_toml();
    for line in body.lines() {
        let masked = if line.starts_with("api_key") && w.current_provider().needs_api_key() {
            "api_key       = \"••••••••••\"".to_string()
        } else {
            line.to_string()
        };
        out.push(Line::from(Span::styled(
            format!("  {}", masked),
            Style::default().fg(Color::White),
        )));
    }
    out
}

fn check_daemon_lines(w: &WizardState) -> Vec<Line<'static>> {
    let base = w.current_provider().base_url();
    let mut out = vec![
        Line::raw(""),
        Line::from(Span::styled(
            format!("  Probing Ollama daemon at {} ...", base),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
    ];
    match &w.daemon {
        DaemonStatus::Unchecked | DaemonStatus::Probing => {
            out.push(Line::from(Span::styled(
                "  · waiting for /api/tags response ...".to_string(),
                Style::default().fg(Color::DarkGray),
            )));
        }
        DaemonStatus::Up { models } => {
            out.push(Line::from(Span::styled(
                format!(
                    "  ✓ daemon reachable ({} model{} installed)",
                    models.len(),
                    if models.len() == 1 { "" } else { "s" }
                ),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )));
        }
        DaemonStatus::Down(reason) => {
            out.push(Line::from(Span::styled(
                format!("  ⚠ Ollama not reachable: {}", reason),
                Style::default()
                    .fg(Color::Red)
                    .add_modifier(Modifier::BOLD),
            )));
            out.push(Line::raw(""));
            out.push(Line::from(Span::styled(
                "  Install from https://ollama.com/download then run `ollama serve`.".to_string(),
                Style::default().fg(Color::White),
            )));
            out.push(Line::from(Span::styled(
                "  Press [R] to retry, or [Enter] to save the config anyway.".to_string(),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }
    out
}

fn pull_model_lines(w: &WizardState) -> Vec<Line<'static>> {
    let model = w.current_provider().default_model();
    let mut out = vec![
        Line::raw(""),
        Line::from(Span::styled(
            format!("  Default model: {}", model),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
    ];
    match &w.pull {
        PullStatus::AlreadyPresent => {
            out.push(Line::from(Span::styled(
                "  ✓ already pulled — ready to use.".to_string(),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )));
        }
        PullStatus::Idle => {
            if matches!(w.daemon, DaemonStatus::Up { .. }) {
                out.push(Line::from(Span::styled(
                    "  Not yet pulled. Press [P] to download (~4.5GB) or [Enter] to skip.".to_string(),
                    Style::default().fg(Color::White),
                )));
            } else {
                out.push(Line::from(Span::styled(
                    "  (skipped — daemon unreachable; pull later via `ollama pull`)"
                        .to_string(),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }
        PullStatus::InProgress {
            phase,
            completed,
            total,
        } => {
            out.push(Line::from(Span::styled(
                format!("  · {}", phase),
                Style::default().fg(Color::Yellow),
            )));
            if *total > 0 {
                let pct = ((*completed as f64 / *total as f64) * 100.0).clamp(0.0, 100.0);
                out.push(Line::from(Span::styled(
                    format!(
                        "    {:.1}%  ({} / {})",
                        pct,
                        human_bytes(*completed),
                        human_bytes(*total)
                    ),
                    Style::default().fg(Color::White),
                )));
                out.push(Line::from(Span::styled(
                    progress_bar(pct, 40),
                    Style::default().fg(Color::Green),
                )));
            }
        }
        PullStatus::Done => {
            out.push(Line::from(Span::styled(
                "  ✓ pull complete.".to_string(),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )));
        }
        PullStatus::Failed(msg) => {
            out.push(Line::from(Span::styled(
                format!("  ✗ pull failed: {}", msg),
                Style::default().fg(Color::Red),
            )));
            out.push(Line::from(Span::styled(
                "  Press [P] to retry, or [Enter] to continue without it.".to_string(),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }
    out
}

fn pull_model_legend(w: &WizardState) -> &'static str {
    match &w.pull {
        PullStatus::InProgress { .. } => {
            "  pulling — please wait   [Esc] cancel wizard"
        }
        PullStatus::AlreadyPresent | PullStatus::Done => {
            "  [Enter] continue   [Backspace] back   [Esc] cancel"
        }
        _ => "  [P] pull   [Enter] skip   [Backspace] back   [Esc] cancel",
    }
}

fn human_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = n as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    format!("{:.1}{}", size, UNITS[unit])
}

fn progress_bar(pct: f64, width: usize) -> String {
    let filled = ((pct / 100.0) * width as f64).round() as usize;
    let filled = filled.min(width);
    let mut s = String::with_capacity(width + 2);
    s.push_str("    [");
    for _ in 0..filled {
        s.push('█');
    }
    for _ in filled..width {
        s.push('░');
    }
    s.push(']');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn added_line_gets_tinted_full_width() {
        let theme = Theme::dark();
        let line = styled_diff_line("    + hello", 20, &theme);
        assert_eq!(line_text(&line).chars().count(), 20);
        assert!(line.spans.iter().all(|s| s.style.bg == Some(theme.diff_add_bg)));
        assert_eq!(line.spans[0].style.fg, Some(theme.diff_added));
    }

    #[test]
    fn removed_line_gets_del_tint() {
        let theme = Theme::dark();
        let line = styled_diff_line("    - gone", 20, &theme);
        assert!(line.spans.iter().all(|s| s.style.bg == Some(theme.diff_del_bg)));
        assert_eq!(line.spans[0].style.fg, Some(theme.diff_removed));
    }

    #[test]
    fn context_line_stays_dim_without_bg() {
        let theme = Theme::dark();
        let line = styled_diff_line("      same", 20, &theme);
        assert!(line.spans.iter().all(|s| s.style.bg.is_none()));
        assert_eq!(line.spans[0].style.fg, Some(theme.dim));
    }

    #[test]
    fn light_theme_uses_github_pastels() {
        assert_eq!(Theme::light().diff_add_bg, Color::Rgb(0xda, 0xfb, 0xe1));
        assert_eq!(Theme::light().diff_del_bg, Color::Rgb(0xff, 0xeb, 0xe9));
        assert_eq!(Theme::dark().diff_add_bg, Color::Rgb(0x21, 0x3a, 0x2b));
        assert_eq!(Theme::dimmed().diff_add_bg, Color::Rgb(0x1f, 0x2a, 0x1f));
    }

    #[test]
    fn removed_line_whose_body_starts_with_a_sign_still_tints() {
        let theme = Theme::dark();
        let line = styled_diff_line("    - - foo", 20, &theme);
        assert!(line.spans.iter().all(|s| s.style.bg == Some(theme.diff_del_bg)));
        assert_eq!(line.spans[0].style.fg, Some(theme.diff_removed));
    }

    #[test]
    fn context_line_whose_body_starts_with_plus_stays_dim() {
        let theme = Theme::dark();
        let line = styled_diff_line("      + foo", 20, &theme);
        assert!(line.spans.iter().all(|s| s.style.bg.is_none()));
        assert_eq!(line.spans[0].style.fg, Some(theme.dim));
    }

    #[test]
    fn short_line_without_sign_column_stays_dim() {
        let theme = Theme::dark();
        let line = styled_diff_line("abc", 20, &theme);
        assert!(line.spans.iter().all(|s| s.style.bg.is_none()));
    }

    #[test]
    fn approval_pane_lines_render_title_options_and_selection() {
        let theme = Theme::dark();
        let state = ApprovalState {
            tool: "Bash".into(),
            reason: "run it".into(),
            preview: "    ls -la".into(),
            scroll: 0,
            selected: 1,
        };
        let lines = approval_pane_lines(&state, 5, 60, &theme);
        let text: Vec<String> = lines.iter().map(line_text).collect();
        assert!(text[0].contains("Would you like to run the following command?"));
        assert!(text.iter().any(|l| l == "› 2. No (n)"));
        assert!(text.iter().any(|l| l == "  1. Yes (y)"));
        assert!(
            text.iter()
                .any(|l| l.contains("Press enter to confirm or esc to cancel"))
        );
        let selected = lines
            .iter()
            .find(|l| line_text(l).starts_with("› "))
            .unwrap();
        assert_eq!(selected.spans[0].style.fg, Some(theme.accent));
    }

    #[test]
    fn approval_pane_uses_edit_title_for_edit_tools() {
        let theme = Theme::dark();
        let state = ApprovalState {
            tool: "Edit".into(),
            reason: String::new(),
            preview: "    + x".into(),
            scroll: 0,
            selected: 0,
        };
        let lines = approval_pane_lines(&state, 5, 60, &theme);
        assert!(line_text(&lines[0]).contains("make the following edits"));
    }
}
