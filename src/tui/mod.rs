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

use crossterm::event::{Event as CtEvent, EventStream, KeyCode, KeyEventKind, KeyModifiers};
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

    guard.terminal_mut().draw(|f| ui::draw(f, &app)).map_err(io_err)?;

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
        guard.terminal_mut().draw(|f| ui::draw(f, &app)).map_err(io_err)?;
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
