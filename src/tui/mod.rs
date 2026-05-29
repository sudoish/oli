//! TUI front-end. Default for `oli` when stdin/stdout are TTYs and
//! `--plain` is not set. Falls back to `crate::repl::run` (the
//! rustyline REPL) otherwise. See `specs/tui.md` for the full
//! roadmap.
//!
//! Architecture (Phases F–G):
//!
//! - The **render task** is `tui::run`'s outer loop. It owns the
//!   `App` state and the `Terminal` and processes one `UiEvent`
//!   per iteration, then redraws.
//! - The **input task** wraps `crossterm::event::EventStream`
//!   into `UiEvent::Key` / `UiEvent::Resize` and pushes onto the
//!   shared mpsc channel.
//! - The **agent driver task** owns the `Agent` and the
//!   `SlashRegistry`. It receives `AgentCommand`s from the render
//!   task and pushes `TurnStarted` / `ContentChunk` / `TurnFinished`
//!   / `SystemNote` / `Quit` back through the same `UiEvent`
//!   channel.
//!
//! One mpsc channel funnels all events; the driver and input tasks
//! are independent producers. The render task is the only consumer.

mod app;
mod approver;
pub mod caps;
mod completion;
mod driver;
mod event;
pub mod fuzzy;
mod hints;
pub mod history;
mod hook;
mod markdown;
mod terminal;
pub mod theme;
mod ui;
mod wizard;

pub use app::App;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::{
    Event as CtEvent, EventStream, KeyCode, KeyEventKind, KeyModifiers, MouseEvent,
    MouseEventKind,
};
use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};

use crate::agent::Agent;
use crate::error::{AgentError, Result};
use crate::plugins::PluginReloader;
use crate::repl::slash::SlashCommand;
use crate::tui::app::{Mode, SubmitAction};
use crate::tui::approver::{PendingApproval, TuiApprover};
use crate::tui::driver::AgentCommand;
use crate::tui::event::{ApprovalResponse, UiEvent};
use crate::tui::terminal::{TerminalGuard, ViewportMode};

pub use crate::tui::terminal::{
    ViewportChoice, ViewportMode as Viewport, resolve_mode, resolve_mouse,
};

pub async fn run(
    mut agent: Agent,
    plugin_slashes: Vec<Box<dyn SlashCommand>>,
    reloader: Option<Arc<PluginReloader>>,
    session_id: Option<String>,
    viewport: ViewportMode,
    mouse_capture: bool,
    osc52_supported: bool,
    host_hint: String,
    theme: theme::Theme,
) -> Result<()> {
    // Snapshot identity fields for the status bar before the
    // agent moves into the driver task. Branch is queried once
    // (it doesn't change mid-session in any healthy workflow);
    // model + ctx_window come from the agent's configured caps.
    let initial_status = app::StatusModel {
        session_id,
        model: agent.model.clone(),
        ctx_window: agent.caps.ctx_window as u32,
        branch: detect_git_branch(),
        last_usage: None,
        session_usage: Default::default(),
    };

    let mut guard = TerminalGuard::enter(viewport, mouse_capture)
        .map_err(|e| AgentError::Provider(format!("tui init: {}", e)))?;

    let (tx, mut rx) = mpsc::unbounded_channel::<UiEvent>();

    // Register the tool-card hook BEFORE the agent moves into the
    // driver task. From inside the agent loop the hook fires on
    // each PreToolUse / PostToolUse and pushes UiEvents the
    // render task picks up. Registered first means it sits ahead
    // of any plugin-registered hooks in the dispatch order, so
    // plugin Replace/Skip outcomes don't suppress the card the
    // user wants to see.
    agent.hooks.register(hook::TuiHook::new(tx.clone()));

    // Swap in the TUI's approver: when policy returns Ask, this
    // pushes UiEvent::ApprovalRequested onto the same channel and
    // awaits the user's keystroke via the PendingApproval slot
    // shared with the render task. The persisted allow-list is
    // loaded from `~/.config/oli/policy-allow.json` so prior `[A]`
    // decisions short-circuit without prompting.
    let pending_approval: PendingApproval = Arc::new(Mutex::new(None));
    let persisted_allow = Arc::new(crate::policy::PersistedAllowList::open());
    agent = agent.with_approver(Box::new(TuiApprover::new(
        tx.clone(),
        pending_approval.clone(),
        persisted_allow,
    )));

    // Input task: lives until the channel is dropped (i.e. until
    // we drop the last `tx` after the loop exits).
    let input_tx = tx.clone();
    let input_handle = tokio::spawn(async move {
        let mut events = EventStream::new();
        while let Some(Ok(ev)) = events.next().await {
            match ev {
                CtEvent::Key(k) => {
                    if k.kind != KeyEventKind::Release
                        && input_tx.send(UiEvent::Key(k)).is_err()
                    {
                        break;
                    }
                }
                CtEvent::Resize(_, _) => {
                    if input_tx.send(UiEvent::Resize).is_err() {
                        break;
                    }
                }
                CtEvent::Mouse(m) => {
                    if input_tx.send(UiEvent::Mouse(m)).is_err() {
                        break;
                    }
                }
                _ => {}
            }
        }
    });

    // Snapshot slash (name, description) pairs BEFORE the
    // slashes move into the driver. Used by the completion
    // popup, the `/help` browser, and `/<cmd> ?` inline help.
    let initial_slash_meta = collect_slash_meta(&plugin_slashes);

    // Agent driver task: owns the agent + slash registry. Talks
    // back through the same UiEvent channel.
    let (cmd_tx, driver_handle) = driver::spawn(agent, plugin_slashes, reloader, tx.clone());

    let mut app = App::new();
    // Persistent history loaded once at startup; appended on each
    // submit. Failures are silently ignored — the user gets a
    // working session even if the history file is corrupt.
    app.set_history(history::load());
    app.set_slash_meta(initial_slash_meta);
    app.set_status(initial_status);
    app.set_shown_hints(hints::load());
    app.set_clipboard_caps(osc52_supported, host_hint);
    app.set_theme(theme);
    if !has_user_config() {
        app.open_wizard();
    }

    guard
            .terminal_mut()
            .draw(|f| ui::draw(f, &mut app))
            .map_err(io_err)?;

    let frame_budget = Duration::from_millis(16); // ~60fps ceiling
    loop {
        let first = match tokio::time::timeout(frame_budget, rx.recv()).await {
            Ok(Some(ev)) => Some(ev),
            Ok(None) => break, // all senders dropped (shouldn't happen here)
            Err(_) => None,
        };
        if let Some(ev) = first {
            handle_event(&mut app, ev, &cmd_tx, &tx, &pending_approval);
        }
        while let Ok(ev) = rx.try_recv() {
            handle_event(&mut app, ev, &cmd_tx, &tx, &pending_approval);
        }
        if app.should_quit {
            break;
        }
        guard
            .terminal_mut()
            .draw(|f| ui::draw(f, &mut app))
            .map_err(io_err)?;
    }

    input_handle.abort();
    let _ = cmd_tx.send(AgentCommand::Shutdown);
    let _ = driver_handle.await;
    Ok(())
}

fn handle_event(
    app: &mut App,
    ev: UiEvent,
    cmd_tx: &mpsc::UnboundedSender<AgentCommand>,
    ui_tx: &mpsc::UnboundedSender<UiEvent>,
    pending_approval: &PendingApproval,
) {
    match ev {
        UiEvent::Key(key) => on_key(app, key, cmd_tx, ui_tx, pending_approval),
        UiEvent::Resize => {}
        UiEvent::Mouse(m) => on_mouse(app, m),
        UiEvent::TurnStarted => app.on_turn_started(),
        UiEvent::ContentChunk(s) => app.on_content_chunk(&s),
        UiEvent::TurnFinished { final_content } => app.on_turn_finished(&final_content),
        UiEvent::TurnError(msg) => app.on_turn_error(&msg),
        UiEvent::TurnCancelled => app.on_turn_cancelled(),
        UiEvent::SystemNote(body) => app.on_system_note(body),
        UiEvent::SlashFinished => app.on_slash_finished(),
        UiEvent::Quit => app.request_quit(),
        UiEvent::ToolStart {
            id,
            tool,
            args_preview,
        } => app.on_tool_start(id, tool, args_preview),
        UiEvent::ToolDone {
            id,
            duration,
            summary,
            ok,
            full_output,
        } => app.on_tool_done(id, duration, summary, ok, full_output),
        UiEvent::ToolArgsChunk {
            provider_tool_id,
            name,
            accumulated_json,
        } => app.on_tool_args_chunk(provider_tool_id, name, accumulated_json),
        UiEvent::ApprovalRequested {
            tool,
            args,
            reason,
        } => app.on_approval_requested(tool, args, reason),
        UiEvent::UsageUpdate { last, session } => app.update_usage(last, session),
        UiEvent::UndoApplied {
            prompt_body,
            load_into_input,
        } => {
            // Trim the transcript in lock-step with the agent's
            // memory truncation.
            let trimmed = app.undo_last_user_turn();
            // Prefer the body the agent reports — it's the
            // authoritative source. Fall back to the
            // transcript-side trimmed body if the driver had
            // nothing (e.g. memory was already empty but we
            // still had a stray UserPrompt item — shouldn't
            // happen in practice).
            let body = prompt_body.or(trimmed);
            match body {
                Some(b) if load_into_input => {
                    app.set_input_text_pub(&b);
                    app.on_system_note(format!("(undid `{}` — re-edit and submit)", b));
                }
                Some(b) => {
                    app.on_system_note(format!("(undid `{}`)", b));
                }
                None => {
                    app.on_system_note("(nothing to undo)".into());
                }
            }
        }
        UiEvent::WizardOllamaProbed(probe) => {
            if let Some(w) = app.wizard_mut() {
                w.daemon = match probe {
                    crate::wizard_init::OllamaProbe::Down { reason } => {
                        crate::tui::wizard::DaemonStatus::Down(reason)
                    }
                    crate::wizard_init::OllamaProbe::Up { models } => {
                        crate::tui::wizard::DaemonStatus::Up { models }
                    }
                };
                w.reconcile_pull_status();
            }
        }
        UiEvent::WizardOllamaPullEvent(ev) => {
            if let Some(w) = app.wizard_mut() {
                w.pull = match ev {
                    crate::wizard_init::PullEvent::Phase(phase) => {
                        crate::tui::wizard::PullStatus::InProgress {
                            phase,
                            completed: 0,
                            total: 0,
                        }
                    }
                    crate::wizard_init::PullEvent::Progress {
                        phase,
                        completed,
                        total,
                    } => crate::tui::wizard::PullStatus::InProgress {
                        phase,
                        completed,
                        total,
                    },
                    crate::wizard_init::PullEvent::Done => {
                        crate::tui::wizard::PullStatus::Done
                    }
                    crate::wizard_init::PullEvent::Error(msg) => {
                        crate::tui::wizard::PullStatus::Failed(msg)
                    }
                };
            }
        }
    }
}

fn on_key(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    cmd_tx: &mpsc::UnboundedSender<AgentCommand>,
    ui_tx: &mpsc::UnboundedSender<UiEvent>,
    pending_approval: &PendingApproval,
) {
    // Overlay short-circuits. While any modal is up, the user's
    // keystrokes route to it, not to the input box. Order
    // matters for the safety-critical approval path: a stray
    // 'y' must not slip through to a freshly-typed prompt.
    use crate::tui::app::Overlay;
    match &app.overlay {
        Some(Overlay::Approval(_)) => {
            handle_approval_key(app, key, pending_approval);
            return;
        }
        Some(Overlay::SessionsPicker(_)) => {
            handle_sessions_picker_key(app, key);
            return;
        }
        Some(Overlay::HelpBrowser(_)) => {
            handle_help_browser_key(app, key);
            return;
        }
        Some(Overlay::InlineHelp(_)) => {
            // Any keypress dismisses the card. Modifier-only
            // events (KeyCode::Modifier) shouldn't, but
            // crossterm collapses those into nothing on most
            // terminals so we don't have to filter explicitly.
            app.close_inline_help();
            return;
        }
        Some(Overlay::HistorySearch(_)) => {
            handle_history_search_key(app, key);
            return;
        }
        Some(Overlay::CopyFallback(_)) => {
            handle_copy_fallback_key(app, key);
            return;
        }
        Some(Overlay::Wizard(_)) => {
            handle_wizard_key(app, key, ui_tx);
            return;
        }
        Some(Overlay::Search(_)) => {
            handle_search_key(app, key);
            return;
        }
        None => {}
    }

    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('c') if ctrl => {
            // Cancel-or-quit. While busy, fire the cancel signal
            // and stay in the loop. While idle, exit.
            if app.is_busy() {
                if let Some(tx) = app.take_cancel_sender() {
                    let _ = tx.send(());
                }
            } else {
                app.request_quit();
            }
            return;
        }
        KeyCode::Char('d') if ctrl => {
            // Ctrl+D always quits. The driver task gets a
            // Shutdown after the loop exits.
            if !app.is_busy() {
                app.request_quit();
            }
            return;
        }
        KeyCode::Char('r') if ctrl => {
            // Ctrl+R: history search. Open the overlay; while
            // it's up the dedicated handler eats every keystroke
            // (typed query / arrows / Enter / Esc) until the
            // user picks or cancels.
            if !app.is_busy() {
                app.open_history_search();
            }
            return;
        }
        KeyCode::Char('e') if ctrl => {
            // Ctrl+E: edit-and-rerun. Undo the last user turn
            // and pre-fill the input with its body. Reject
            // mid-stream — undoing while the agent is still
            // mid-flight would leave memory and transcript
            // out of sync.
            if !app.is_busy() {
                let _ = cmd_tx.send(AgentCommand::Undo {
                    load_into_input: true,
                });
            }
            return;
        }
        KeyCode::Char('f') if ctrl => {
            // Ctrl+F: in-transcript search. Opens a one-line
            // search bar above the input; typing filters,
            // Enter / n / N cycle matches, Esc closes. (Spec X2
            // suggested `/` but that's already the slash-command
            // sigil at the input prompt — Ctrl+F sidesteps the
            // conflict.)
            app.open_search();
            return;
        }
        _ => {}
    }
    // Completion menu interception: when a popup is open, send
    // navigation keys (Up/Down/Tab/Enter/Esc) to it first. If
    // it consumes the event, we don't touch the textarea this
    // tick.
    if app.completion.is_some() && app.on_completion_key(key) {
        return;
    }

    let action = app.on_key(key);
    match &action {
        SubmitAction::Prompt(body) | SubmitAction::Slash(body) => {
            // Persist to the history file alongside the in-memory
            // dedupe-aware push that `App::submit` already did. We
            // record both prompts and slashes so Ctrl+R can recall
            // any past invocation. Slashes go in with their `/`
            // prefix to match what the user typed.
            let entry = match &action {
                SubmitAction::Slash(_) => format!("/{}", body),
                _ => body.clone(),
            };
            history::append(&entry);
        }
        SubmitAction::None => {}
    }
    match action {
        SubmitAction::Prompt(body) => {
            let (cancel_tx, cancel_rx) = oneshot::channel();
            app.set_cancel_sender(cancel_tx);
            let _ = cmd_tx.send(AgentCommand::Prompt {
                body,
                cancel: cancel_rx,
            });
        }
        SubmitAction::Slash(line) => {
            // `/copy N` is TUI-local: the transcript lives in
            // App, not in the driver, so we handle it here
            // before forwarding to the slash registry.
            if let Some(rest) = line.strip_prefix("copy") {
                let arg = rest.trim();
                handle_copy_slash(app, arg);
                return;
            }
            // Trailing `?` opens the inline help overlay rather
            // than dispatching: `/cost ?` shows the description
            // for `/cost` without running it.
            let trimmed = line.trim();
            if let Some(name) = trimmed.strip_suffix(" ?") {
                app.open_inline_help(name.trim());
                return;
            }
            if let Some(name) = trimmed.strip_suffix("?") {
                let n = name.trim();
                if !n.is_empty() && !n.contains(char::is_whitespace) {
                    app.open_inline_help(n);
                    return;
                }
            }
            // `/help` and `/sessions` open interactive overlays
            // when called without args. With args they fall
            // through to the slash registry (e.g. `/help foo`
            // is rejected by the registry — same as today).
            if trimmed == "help" {
                app.open_help_browser();
                return;
            }
            if trimmed == "sessions" {
                let entries = collect_session_picker_rows();
                app.open_sessions_picker(entries);
                return;
            }
            if trimmed == "undo" {
                let _ = cmd_tx.send(AgentCommand::Undo {
                    load_into_input: false,
                });
                return;
            }
            let (cancel_tx, cancel_rx) = oneshot::channel();
            app.set_cancel_sender(cancel_tx);
            let _ = cmd_tx.send(AgentCommand::Slash {
                line,
                cancel: cancel_rx,
            });
        }
        SubmitAction::None => {}
    }
    // After a prompt submission we're effectively waiting for the
    // driver — flip Mode so the input box visibly disables. The
    // driver's TurnStarted will overwrite this state shortly.
    if app.cancel_tx.is_some() && matches!(app.mode, Mode::Idle) {
        app.mode = Mode::Thinking {
            since: std::time::Instant::now(),
        };
    }
}

/// (name, description) for every slash command the driver will
/// know about. Used by the completion popup (names) and the
/// `/help` / `/<cmd> ?` overlays (descriptions).
fn collect_slash_meta(plugin_slashes: &[Box<dyn SlashCommand>]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> =
        crate::repl::slash::SlashRegistry::default_set_with_reloader(None)
            .iter()
            .map(|c| (c.name().to_string(), c.description().to_string()))
            .collect();
    for s in plugin_slashes {
        out.push((s.name().to_string(), s.description().to_string()));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

/// Build the session-picker rows from disk. Newest-first by
/// mtime; the label includes the id and a coarse age. Bound to
/// the most recent 50 sessions so the picker doesn't paint
/// thousands of rows on a long-running config dir.
fn collect_session_picker_rows() -> Vec<app::SessionPickerRow> {
    use crate::agent::memory::list_sessions;
    let mut out: Vec<app::SessionPickerRow> = list_sessions()
        .into_iter()
        .map(|e| app::SessionPickerRow {
            label: format_session_label(&e),
            id: e.id,
        })
        .collect();
    out.truncate(50);
    out
}

fn format_session_label(e: &crate::agent::memory::SessionEntry) -> String {
    let age = e
        .mtime
        .and_then(|t| t.elapsed().ok())
        .map(|d| humanize_age(d.as_secs()))
        .unwrap_or_else(|| "?".into());
    format!("{}   ({} ago)", e.id, age)
}

fn humanize_age(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3_600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3_600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

/// True if the user has a config file at the conventional
/// location. Drives the first-run hint card.
fn has_user_config() -> bool {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")));
    let Some(base) = base else {
        return false;
    };
    base.join("oli").join("config.toml").exists()
}

/// `/copy [N]` — copy the N-th-most-recent assistant message to
/// the system clipboard. `N` defaults to 1 (the most recent).
///
/// In hosts that support OSC52 (iTerm2, kitty, WezTerm, ghostty,
/// tmux with `set-clipboard on`) we write the escape directly.
/// In hosts that don't (Neovim `:terminal`, VSCode integrated
/// terminal, generic xterm without an allowlist hit) we open the
/// `Overlay::CopyFallback` modal instead — the user reads the body
/// in a visible window and copies it via the host's own selection
/// affordances. The split is driven by `App::osc52_supported`,
/// which is resolved from `Capabilities::osc52` + `[ui].osc52`
/// at TUI startup.
fn handle_copy_slash(app: &mut App, arg: &str) {
    let n: usize = if arg.is_empty() {
        1
    } else {
        match arg.parse() {
            Ok(n) if n >= 1 => n,
            _ => {
                app.on_system_note(
                    "usage: /copy [N]   (copy N-th-most-recent assistant message; default N=1)"
                        .into(),
                );
                return;
            }
        }
    };

    let target_body = app
        .transcript
        .iter()
        .rev()
        .filter_map(|item| match item {
            crate::tui::app::TranscriptItem::Assistant { body, .. } if !body.is_empty() => {
                Some(body.clone())
            }
            _ => None,
        })
        .nth(n - 1);

    let Some(body) = target_body else {
        app.on_system_note(format!(
            "no assistant message at position {} (history has fewer entries)",
            n
        ));
        return;
    };

    if app.osc52_supported {
        write_osc52_clipboard(&body);
        app.on_system_note(format!(
            "copied {} bytes to clipboard via OSC52 (terminal must support it; tmux needs `set -g set-clipboard on`)",
            body.len()
        ));
    } else {
        let host = app.host_hint.clone();
        app.open_copy_fallback(body, n, host);
    }
}

/// Write `payload` to the user's clipboard via the OSC52 escape
/// sequence. Format: `ESC ] 5 2 ; c ; <base64-of-payload> BEL`.
/// We hand-roll base64 to avoid an extra dep — it's a few lines.
fn write_osc52_clipboard(payload: &str) {
    let encoded = base64_encode(payload.as_bytes());
    let escape = format!("\x1b]52;c;{}\x07", encoded);
    use std::io::Write;
    let mut out = std::io::stdout();
    let _ = out.write_all(escape.as_bytes());
    let _ = out.flush();
}

/// Standard base64 encoding (alphabet `A-Za-z0-9+/`, `=` padding)
/// of the input bytes. Hand-rolled to avoid pulling in `base64`
/// for a 30-line function.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    let mut chunks = bytes.chunks_exact(3);
    for ch in chunks.by_ref() {
        let n = ((ch[0] as u32) << 16) | ((ch[1] as u32) << 8) | ch[2] as u32;
        out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 6) & 0x3f) as usize] as char);
        out.push(ALPHABET[(n & 0x3f) as usize] as char);
    }
    let rem = chunks.remainder();
    match rem.len() {
        1 => {
            let n = (rem[0] as u32) << 16;
            out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
            out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
            out.push('=');
            out.push('=');
        }
        2 => {
            let n = ((rem[0] as u32) << 16) | ((rem[1] as u32) << 8);
            out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
            out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
            out.push(ALPHABET[((n >> 6) & 0x3f) as usize] as char);
            out.push('=');
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_encode_handles_full_blocks() {
        assert_eq!(base64_encode(b"Man"), "TWFu");
        assert_eq!(base64_encode(b"Many hands make light work."), "TWFueSBoYW5kcyBtYWtlIGxpZ2h0IHdvcmsu");
    }

    #[test]
    fn base64_encode_handles_padding() {
        // 1-byte tail.
        assert_eq!(base64_encode(b"M"), "TQ==");
        // 2-byte tail.
        assert_eq!(base64_encode(b"Ma"), "TWE=");
        // empty.
        assert_eq!(base64_encode(b""), "");
    }

    #[test]
    fn base64_encode_handles_unicode_bytes() {
        // Emoji is 4 UTF-8 bytes (one block + one byte tail).
        assert_eq!(base64_encode("✓".as_bytes()), "4pyT");
    }

    use crate::tui::app::TranscriptItem;

    fn app_with_assistants<I, S>(bodies: I) -> App
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut app = App::new();
        for body in bodies {
            app.transcript.push(TranscriptItem::Assistant {
                body: body.into(),
                done: true,
            });
        }
        app
    }

    #[test]
    fn copy_slash_opens_fallback_when_osc52_unsupported() {
        let mut app = app_with_assistants(["hello world"]);
        app.set_clipboard_caps(false, "vscode".into());
        handle_copy_slash(&mut app, "");
        let s = app.copy_fallback().expect("fallback should be open");
        assert_eq!(s.body, "hello world");
        assert_eq!(s.index, 1);
        assert_eq!(s.host_hint, "vscode");
    }

    #[test]
    fn copy_slash_does_not_open_fallback_when_osc52_supported() {
        // When the host honors OSC52, the slash handler writes the
        // escape and emits a system note — no modal is opened.
        let mut app = app_with_assistants(["body"]);
        app.set_clipboard_caps(true, "kitty".into());
        handle_copy_slash(&mut app, "");
        assert!(app.copy_fallback().is_none());
    }

    #[test]
    fn copy_slash_with_explicit_n_targets_nth_most_recent() {
        // `/copy 2` picks the second-most-recent assistant message,
        // not the first one in the transcript. Verifying the index
        // is carried into the modal title.
        let mut app = app_with_assistants(["oldest", "middle", "newest"]);
        app.set_clipboard_caps(false, "neovim:terminal".into());
        handle_copy_slash(&mut app, "2");
        let s = app.copy_fallback().expect("fallback should be open");
        assert_eq!(s.body, "middle");
        assert_eq!(s.index, 2);
    }

    #[test]
    fn copy_slash_without_assistant_messages_does_not_open_fallback() {
        // No transcript content → system note, no modal. Avoids an
        // empty modal that the user has to dismiss for no reason.
        let mut app = App::new();
        app.set_clipboard_caps(false, "vscode".into());
        handle_copy_slash(&mut app, "");
        assert!(app.copy_fallback().is_none());
    }
}

fn on_mouse(app: &mut App, m: MouseEvent) {
    // Wheel ticks scroll the transcript ~3 lines per notch.
    // Other mouse events (clicks, drag, motion) are intentionally
    // dropped — we don't have a reason to act on them yet, and
    // the alt-screen mode swallows native terminal selection
    // anyway.
    match m.kind {
        MouseEventKind::ScrollUp => app.scroll_wheel_up(3),
        MouseEventKind::ScrollDown => app.scroll_wheel_down(3),
        _ => {}
    }
}

fn handle_sessions_picker_key(app: &mut App, key: crossterm::event::KeyEvent) {
    match key.code {
        KeyCode::Up => app.sessions_picker_navigate(-1),
        KeyCode::Down => app.sessions_picker_navigate(1),
        KeyCode::Esc => app.close_sessions_picker(),
        KeyCode::Enter => {
            if let Some(id) = app.sessions_picker_pick() {
                let cmd = format!("oli --resume {}", id);
                write_osc52_clipboard(&cmd);
                app.close_sessions_picker();
                app.on_system_note(format!(
                    "copied `{}` to clipboard — paste in a new shell to resume that session",
                    cmd
                ));
            } else {
                app.close_sessions_picker();
            }
        }
        _ => {}
    }
}

fn handle_wizard_key(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    ui_tx: &mpsc::UnboundedSender<UiEvent>,
) {
    use crate::tui::wizard::{DaemonStatus, PullStatus, WizardStep};

    let step = app.wizard().map(|w| w.step.clone());
    let Some(step) = step else { return };

    match step {
        WizardStep::Welcome => match key.code {
            KeyCode::Esc => {
                app.close_wizard();
                app.on_system_note(
                    "(setup skipped — `oli` will fall back to env vars; \
                     create ~/.config/oli/config.toml when ready)"
                        .into(),
                );
            }
            KeyCode::Enter => {
                if let Some(w) = app.wizard_mut() {
                    w.advance();
                }
            }
            _ => {}
        },
        WizardStep::PickProvider => {
            let w = app.wizard_mut().unwrap();
            match key.code {
                KeyCode::Esc => app.close_wizard(),
                KeyCode::Up => w.navigate_provider(-1),
                KeyCode::Down => w.navigate_provider(1),
                KeyCode::Enter => {
                    w.advance();
                    // Entering CheckDaemon auto-fires the probe.
                    if matches!(w.step, WizardStep::CheckDaemon)
                        && matches!(w.daemon, DaemonStatus::Unchecked)
                    {
                        let base = w.current_provider().base_url().to_string();
                        spawn_ollama_probe(ui_tx.clone(), base);
                        if let Some(ww) = app.wizard_mut() {
                            ww.daemon = DaemonStatus::Probing;
                        }
                    }
                }
                _ => {}
            }
        }
        WizardStep::CheckDaemon => match key.code {
            KeyCode::Esc => app.close_wizard(),
            KeyCode::Backspace => {
                if let Some(w) = app.wizard_mut() {
                    w.step_back();
                }
            }
            KeyCode::Char('r') | KeyCode::Char('R') => {
                let w = app.wizard_mut().unwrap();
                let base = w.current_provider().base_url().to_string();
                w.daemon = DaemonStatus::Probing;
                spawn_ollama_probe(ui_tx.clone(), base);
            }
            KeyCode::Enter => {
                if let Some(w) = app.wizard_mut() {
                    w.advance();
                }
            }
            _ => {}
        },
        WizardStep::PullModel => match key.code {
            KeyCode::Esc => app.close_wizard(),
            KeyCode::Backspace => {
                if let Some(w) = app.wizard_mut() {
                    w.step_back();
                }
            }
            KeyCode::Char('p') | KeyCode::Char('P') => {
                let w = app.wizard_mut().unwrap();
                if matches!(
                    w.pull,
                    PullStatus::Idle | PullStatus::Failed(_)
                ) && matches!(w.daemon, DaemonStatus::Up { .. })
                {
                    let base = w.current_provider().base_url().to_string();
                    let model = w.current_provider().default_model().to_string();
                    w.pull = PullStatus::InProgress {
                        phase: "starting".into(),
                        completed: 0,
                        total: 0,
                    };
                    spawn_ollama_pull(ui_tx.clone(), base, model);
                }
            }
            KeyCode::Enter => {
                let w = app.wizard_mut().unwrap();
                // Block the user from advancing while a pull is in
                // flight — they'll lose progress visibility.
                if !matches!(w.pull, PullStatus::InProgress { .. }) {
                    w.advance();
                }
            }
            _ => {}
        },
        WizardStep::EnterApiKey => {
            let w = app.wizard_mut().unwrap();
            match key.code {
                KeyCode::Esc => app.close_wizard(),
                KeyCode::Backspace => {
                    w.api_key.pop();
                }
                KeyCode::Enter => {
                    if !w.api_key.is_empty() {
                        w.advance();
                    }
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    w.api_key.push(c);
                }
                _ => {}
            }
        }
        WizardStep::Confirm => match key.code {
            KeyCode::Esc => app.close_wizard(),
            KeyCode::Enter => save_wizard(app),
            KeyCode::Backspace => {
                if let Some(w) = app.wizard_mut() {
                    w.step_back();
                }
            }
            _ => {}
        },
        WizardStep::Saved { .. } => {
            app.close_wizard();
        }
    }
}

fn spawn_ollama_probe(
    ui_tx: mpsc::UnboundedSender<UiEvent>,
    base_url: String,
) {
    tokio::spawn(async move {
        let probe = crate::wizard_init::probe_ollama(
            &base_url,
            std::time::Duration::from_secs(2),
        )
        .await;
        let _ = ui_tx.send(UiEvent::WizardOllamaProbed(probe));
    });
}

fn spawn_ollama_pull(
    ui_tx: mpsc::UnboundedSender<UiEvent>,
    base_url: String,
    model: String,
) {
    tokio::spawn(async move {
        let tx = ui_tx.clone();
        let result = crate::wizard_init::pull_model(&base_url, &model, move |ev| {
            let _ = tx.send(UiEvent::WizardOllamaPullEvent(ev));
        })
        .await;
        if let Err(msg) = result {
            let _ = ui_tx.send(UiEvent::WizardOllamaPullEvent(
                crate::wizard_init::PullEvent::Error(msg),
            ));
        }
    });
}

fn save_wizard(app: &mut App) {
    let Some(w) = app.wizard_mut() else {
        return;
    };
    let body = w.render_toml();
    let path = match wizard::config_path() {
        Some(p) => p,
        None => {
            app.on_system_note("(can't resolve $HOME — config not saved)".into());
            app.close_wizard();
            return;
        }
    };
    match wizard::save(&path, &body) {
        Ok(()) => {
            w.step = wizard::WizardStep::Saved { path: path.clone() };
            app.on_system_note(format!(
                "✅ wrote {} — restart `oli` to use the new config",
                path.display()
            ));
        }
        Err(e) => {
            app.on_system_note(format!("(setup failed to save: {})", e));
        }
    }
}

fn handle_history_search_key(app: &mut App, key: crossterm::event::KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => app.close_history_search(),
        KeyCode::Up => app.history_search_navigate(-1),
        KeyCode::Down => app.history_search_navigate(1),
        KeyCode::Char('r') if ctrl => app.history_search_navigate(1),
        KeyCode::Enter => {
            if let Some(body) = app.history_search_pick() {
                app.set_input_text_pub(&body);
            }
            app.close_history_search();
        }
        KeyCode::Backspace => app.history_search_backspace(),
        KeyCode::Char(c) if !ctrl => app.history_search_push_char(c),
        _ => {}
    }
}

fn handle_help_browser_key(app: &mut App, key: crossterm::event::KeyEvent) {
    match key.code {
        KeyCode::Up => app.help_browser_navigate(-1),
        KeyCode::Down => app.help_browser_navigate(1),
        KeyCode::Esc | KeyCode::Enter => app.close_help_browser(),
        _ => {}
    }
}

fn handle_search_key(app: &mut App, key: crossterm::event::KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    // The renderer is the authority on the active match list
    // (it's computed against the laid-out transcript), so the
    // handler cycles via App's last cached count. For navigation
    // we use `i32::MAX` as a sentinel meaning "let the renderer
    // wrap it next paint" — App stores the modular index and the
    // next render clamps it. In practice the cached count from
    // `App.search_match_count` is sufficient.
    let count = app.search_match_count;
    match key.code {
        KeyCode::Esc => app.close_search(),
        KeyCode::Enter | KeyCode::Down => app.search_navigate(1, count),
        KeyCode::Up => app.search_navigate(-1, count),
        KeyCode::Char('n') if !ctrl => app.search_navigate(1, count),
        KeyCode::Char('N') if !ctrl => app.search_navigate(-1, count),
        KeyCode::Backspace => app.search_backspace(),
        KeyCode::Char(c) if !ctrl => app.search_push_char(c),
        _ => {}
    }
}

fn handle_copy_fallback_key(app: &mut App, key: crossterm::event::KeyEvent) {
    match key.code {
        KeyCode::PageUp => app.copy_fallback_scroll_up(),
        KeyCode::PageDown => app.copy_fallback_scroll_down(),
        // Modifier-only events shouldn't dismiss; everything else
        // (including Esc, Enter, single chars) closes the modal.
        KeyCode::Modifier(_) => {}
        _ => app.close_copy_fallback(),
    }
}

fn handle_approval_key(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    pending_approval: &PendingApproval,
) {
    let response = match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => Some(ApprovalResponse::Yes),
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Some(ApprovalResponse::No),
        // Lowercase `a` — allow for the rest of *this* session.
        // Uppercase `A` — also write the fingerprint to
        // `~/.config/oli/policy-allow.json` so future runs
        // skip the prompt.
        KeyCode::Char('a') => Some(ApprovalResponse::AlwaysAllow),
        KeyCode::Char('A') => Some(ApprovalResponse::PersistAllow),
        KeyCode::Char('d') | KeyCode::Char('D') => Some(ApprovalResponse::AlwaysDeny),
        // PgUp/PgDn scroll the diff body; let the user read a
        // long change before deciding.
        KeyCode::PageUp => {
            app.approval_scroll_up();
            None
        }
        KeyCode::PageDown => {
            app.approval_scroll_down();
            None
        }
        _ => None,
    };

    if let Some(resp) = response {
        if let Some(tx) = pending_approval.lock().unwrap().take() {
            let _ = tx.send(resp);
        }
        app.close_approval();
        // Once the user has used `a` (or `d`) at least once, fade
        // the "Press [a] to allow this session" hint from future
        // approval modals. Persist the change so the next session
        // doesn't keep nagging them.
        if matches!(
            resp,
            ApprovalResponse::AlwaysAllow
                | ApprovalResponse::AlwaysDeny
                | ApprovalResponse::PersistAllow
        ) && app.hint_is_unseen(hints::ids::APPROVAL_ALLOW)
        {
            app.mark_hint_shown(hints::ids::APPROVAL_ALLOW);
            hints::save(&app.shown_hints);
        }
    }
}

fn io_err(e: std::io::Error) -> AgentError {
    AgentError::Provider(format!("tui io: {}", e))
}

/// Best-effort current git branch + dirty marker, queried once
/// at TUI startup. Returns `None` if not inside a repo or git is
/// missing — the status bar drops the field in that case. Branch
/// changes mid-session are rare in agent workflows and a
/// stale-by-a-minute readout is fine.
fn detect_git_branch() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    // `git rev-parse --abbrev-ref HEAD` for the branch name.
    let branch = std::process::Command::new("git")
        .current_dir(&cwd)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !branch.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&branch.stdout).trim().to_string();
    if name.is_empty() {
        return None;
    }
    // Append `*` if the worktree is dirty.
    let dirty = std::process::Command::new("git")
        .current_dir(&cwd)
        .args(["status", "--porcelain=v1"])
        .output()
        .ok()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);
    Some(if dirty { format!("{} *", name) } else { name })
}
