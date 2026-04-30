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
mod completion;
mod driver;
mod event;
mod history;
mod hook;
mod terminal;
mod ui;

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
use crate::tui::terminal::TerminalGuard;

pub async fn run(
    mut agent: Agent,
    plugin_slashes: Vec<Box<dyn SlashCommand>>,
    reloader: Option<Arc<PluginReloader>>,
) -> Result<()> {
    let mut guard = TerminalGuard::enter()
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
    // shared with the render task.
    let pending_approval: PendingApproval = Arc::new(Mutex::new(None));
    agent = agent.with_approver(Box::new(TuiApprover::new(
        tx.clone(),
        pending_approval.clone(),
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

    // Snapshot slash names BEFORE the slashes move into the
    // driver. The default registry's set is fixed at compile
    // time; plugin slashes contribute their names dynamically.
    // The TUI's completion popup uses this list. `/plugins
    // reload` updates it via the driver-emitted SlashNamesChanged
    // event.
    let initial_slash_names = collect_slash_names(&plugin_slashes);

    // Agent driver task: owns the agent + slash registry. Talks
    // back through the same UiEvent channel.
    let (cmd_tx, driver_handle) = driver::spawn(agent, plugin_slashes, reloader, tx.clone());

    let mut app = App::new();
    // Persistent history loaded once at startup; appended on each
    // submit. Failures are silently ignored — the user gets a
    // working session even if the history file is corrupt.
    app.set_history(history::load());
    app.set_slash_names(initial_slash_names);

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
            handle_event(&mut app, ev, &cmd_tx, &pending_approval);
        }
        while let Ok(ev) = rx.try_recv() {
            handle_event(&mut app, ev, &cmd_tx, &pending_approval);
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
    pending_approval: &PendingApproval,
) {
    match ev {
        UiEvent::Key(key) => on_key(app, key, cmd_tx, pending_approval),
        UiEvent::Resize => {}
        UiEvent::Mouse(m) => on_mouse(app, m),
        UiEvent::TurnStarted => app.on_turn_started(),
        UiEvent::ContentChunk(s) => app.on_content_chunk(&s),
        UiEvent::TurnFinished { final_content } => app.on_turn_finished(&final_content),
        UiEvent::TurnError(msg) => app.on_turn_error(&msg),
        UiEvent::TurnCancelled => app.on_turn_cancelled(),
        UiEvent::SystemNote(body) => app.on_system_note(body),
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
        } => app.on_tool_done(id, duration, summary, ok),
        UiEvent::ApprovalRequested {
            id,
            tool,
            args,
            reason,
        } => app.on_approval_requested(id, tool, args, reason),
    }
}

fn on_key(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    cmd_tx: &mpsc::UnboundedSender<AgentCommand>,
    pending_approval: &PendingApproval,
) {
    // Approval modal short-circuits everything else: while it's
    // up, the user's keystrokes go to y/n/a/d/Esc and modal
    // scroll, not to the input box.
    if app.approval.is_some() {
        handle_approval_key(app, key, pending_approval);
        return;
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

/// Names of all slash commands the driver will know about: the
/// default set + any plugin-registered ones the binary passed in.
/// Used to seed the completion popup's slash list.
fn collect_slash_names(plugin_slashes: &[Box<dyn SlashCommand>]) -> Vec<String> {
    let mut names: Vec<String> =
        crate::repl::slash::SlashRegistry::default_set_with_reloader(None)
            .iter()
            .map(|c| c.name().to_string())
            .collect();
    for s in plugin_slashes {
        names.push(s.name().to_string());
    }
    names.sort();
    names.dedup();
    names
}

/// `/copy [N]` — copy the N-th-most-recent assistant message to
/// the system clipboard via OSC52. `N` defaults to 1 (the most
/// recent). OSC52 lands in iTerm2, kitty, WezTerm, Alacritty
/// (with `clipboard.osc52: true`), and tmux (with
/// `set-clipboard on`). Terminals that don't support it silently
/// drop the escape; we surface a hint either way.
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

    write_osc52_clipboard(&body);
    app.on_system_note(format!(
        "copied {} bytes to clipboard via OSC52 (terminal must support it; tmux needs `set -g set-clipboard on`)",
        body.len()
    ));
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

fn handle_approval_key(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    pending_approval: &PendingApproval,
) {
    let response = match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => Some(ApprovalResponse::Yes),
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Some(ApprovalResponse::No),
        KeyCode::Char('a') | KeyCode::Char('A') => Some(ApprovalResponse::AlwaysAllow),
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
    }
}

fn io_err(e: std::io::Error) -> AgentError {
    AgentError::Provider(format!("tui io: {}", e))
}
