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
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::tui::app::{
    App, ApprovalState, CopyFallbackState, HelpBrowserState, HistorySearchState, InlineHelpState,
    SessionsPickerState,
};
use crate::tui::hints;
use crate::tui::wizard::{DaemonStatus, PullStatus, WizardProvider, WizardState, WizardStep};

use super::{centered_rect, clip_to_width};

/// Centered overlay above the transcript. Width 80% of the
/// terminal (clamped to [60, 120] cols), height 60% of the
/// terminal (clamped to [10, 40] rows). `Clear` blanks the area
/// underneath so the transcript bleed-through doesn't make the
/// modal hard to read.
pub(super) fn draw_approval_modal(
    f: &mut Frame,
    full_area: Rect,
    approval: &ApprovalState,
    app: &App,
) {
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
        Span::styled("  reason: ", Style::default().fg(Color::DarkGray)),
        Span::styled(approval.reason.as_str(), Style::default().fg(Color::White)),
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
    let mut legend_spans: Vec<Span<'static>> = vec![Span::styled(
        "  [y]es  [n]o  [a]llow this session  [A]llow always (persisted)  [d]eny session  [PgUp/Dn] scroll  [Esc] cancel",
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    )];
    // Fading hint: highlight `a` and `d` for first-time users.
    if app.hint_is_unseen(hints::ids::APPROVAL_ALLOW) {
        legend_spans.insert(
            0,
            Span::styled("  💡 ".to_string(), Style::default().fg(Color::Yellow)),
        );
    }
    let legend = Paragraph::new(Line::from(legend_spans));
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
            Span::styled(format!("+ {}", rest), Style::default().fg(Color::Green)),
        ])
    } else if let Some(rest) = trimmed.strip_prefix("- ") {
        Line::from(vec![
            Span::raw("    "),
            Span::styled(format!("- {}", rest), Style::default().fg(Color::Red)),
        ])
    } else {
        Line::from(Span::styled(
            line.to_string(),
            Style::default().fg(Color::White),
        ))
    }
}

/// `/sessions` picker overlay. Centered modal with a single
/// list pane; arrow keys navigate, Enter copies the resume
/// command, Esc closes.
pub(super) fn draw_sessions_picker(
    f: &mut Frame,
    full_area: Rect,
    picker: &SessionsPickerState,
) {
    let modal = centered_rect(full_area, 70, 60).intersection(full_area);
    f.render_widget(Clear, modal);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .title(Line::from(Span::styled(
            " /sessions  (↑↓ select · Enter copy `--resume` cmd · Esc close) ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(modal);
    f.render_widget(block, modal);

    if picker.entries.is_empty() {
        let p = Paragraph::new(Line::from(Span::styled(
            "  (no prior sessions yet)",
            Style::default()
                .fg(Color::DarkGray)
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
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            Line::from(Span::styled(
                if selected {
                    format!("▌ {}", row.label)
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
pub(super) fn draw_help_browser(f: &mut Frame, full_area: Rect, browser: &HelpBrowserState) {
    let modal = centered_rect(full_area, 80, 70).intersection(full_area);
    f.render_widget(Clear, modal);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .title(Line::from(Span::styled(
            " /help  (↑↓ select · Esc close) ",
            Style::default()
                .fg(Color::Cyan)
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
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            Line::from(Span::styled(
                if selected {
                    format!("▌ /{}", name)
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
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        detail_lines.push(Line::raw(""));
        for body_line in desc.lines() {
            detail_lines.push(Line::from(Span::styled(
                body_line.to_string(),
                Style::default().fg(Color::White),
            )));
        }
        let p = Paragraph::new(detail_lines).wrap(Wrap { trim: false });
        f.render_widget(p, detail_area);
    }
}

/// `/<cmd> ?` one-shot help card. Smaller modal than the full
/// browser; fades on the next keystroke.
pub(super) fn draw_inline_help(f: &mut Frame, full_area: Rect, card: &InlineHelpState) {
    let modal = centered_rect(full_area, 60, 30).intersection(full_area);
    f.render_widget(Clear, modal);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Line::from(Span::styled(
            format!(" /{} ", card.name),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(modal);
    f.render_widget(block, modal);

    let mut lines: Vec<Line<'static>> = Vec::new();
    for body_line in card.description.lines() {
        lines.push(Line::from(Span::styled(
            body_line.to_string(),
            Style::default().fg(Color::White),
        )));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "(press any key to close)".to_string(),
        Style::default()
            .fg(Color::DarkGray)
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
    let modal = centered_rect(full_area, 70, 60).intersection(full_area);
    f.render_widget(Clear, modal);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .title(Line::from(Span::styled(
            " (i-search) ↑↓ select · Enter load · Esc cancel ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(modal);
    f.render_widget(block, modal);

    let parts = Layout::vertical([Constraint::Length(2), Constraint::Min(3)]).split(inner);

    // Query row.
    let query_line = Line::from(vec![
        Span::styled("  search: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            search.query.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "▍",
            Style::default()
                .fg(Color::Cyan)
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
                .fg(Color::DarkGray)
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
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            Line::from(Span::styled(
                if selected {
                    format!("▌ {}", body)
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
pub(super) fn draw_copy_fallback(f: &mut Frame, full_area: Rect, state: &CopyFallbackState) {
    let modal = centered_rect(full_area, 80, 70).intersection(full_area);
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
                " 📋 copy below ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" /copy {} ", state.index),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
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
            Style::default().fg(Color::White),
        )),
        Line::from(Span::styled(
            "  ([ui].osc52 = \"on\" forces OSC52; \"off\" keeps this fallback.)".to_string(),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )),
    ];
    f.render_widget(Paragraph::new(header_lines), parts[0]);

    let body_lines: Vec<Line<'static>> = state
        .body
        .lines()
        .map(|l| Line::from(Span::styled(l.to_string(), Style::default().fg(Color::White))))
        .collect();
    let body = Paragraph::new(body_lines)
        .wrap(Wrap { trim: false })
        .scroll((state.scroll, 0));
    f.render_widget(body, parts[1]);

    let legend = Paragraph::new(Line::from(Span::styled(
        "  [PgUp/Dn] scroll  [Esc / any other key] close".to_string(),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    )));
    f.render_widget(legend, parts[2]);
}

/// First-run setup wizard overlay. Multi-step modal:
/// Welcome → PickProvider → (EnterApiKey if applicable) →
/// Confirm → Saved. Esc closes the wizard at any point.
pub(super) fn draw_wizard(f: &mut Frame, full_area: Rect, w: &WizardState) {
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
        .border_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
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
                format!("  ▌ {}", p.label())
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
