//! TUI front-end. Default for `oli` when stdin/stdout are TTYs and
//! `--plain` is not set. Falls back to `crate::repl::run` (the
//! rustyline REPL) otherwise. See `specs/tui.md` for the full
//! roadmap; Phase F is "skeleton + echo loop + plain fallback."
//!
//! ## What lives here (Phase F)
//!
//! - `App` — the in-memory state of one TUI session.
//! - `event::UiEvent` — the single channel feeding the render loop.
//! - `terminal::TerminalGuard` — alt-screen + raw mode lifecycle,
//!   restored on Drop so a panic doesn't leave the user's terminal
//!   in a broken state.
//! - `ui::draw` — the per-frame render fn.
//!
//! ## What's not here yet
//!
//! - Agent integration (Phase G): the input box accepts text and
//!   echoes it into the transcript; nothing reaches `Agent::run`
//!   yet.
//! - Tool cards, approval modal, markdown, completion (Phases H–N).

mod app;
mod event;
mod terminal;
mod ui;

pub use app::App;

use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{Event as CtEvent, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use tokio::sync::mpsc;

use crate::agent::Agent;
use crate::error::{AgentError, Result};
use crate::plugins::PluginReloader;
use crate::repl::slash::SlashCommand;
use crate::tui::event::UiEvent;
use crate::tui::terminal::TerminalGuard;

/// Drive a TUI session against `agent` until the user exits.
///
/// Phase F is echo-only: the agent argument is taken so the
/// signature matches `repl::run`, but no agent calls are made yet.
/// Phase G wires the agent into the event loop.
#[allow(unused_variables)] // Phase F: agent / slashes / reloader land in Phase G.
pub async fn run(
    agent: Agent,
    plugin_slashes: Vec<Box<dyn SlashCommand>>,
    reloader: Option<Arc<PluginReloader>>,
) -> Result<()> {
    let mut guard = TerminalGuard::enter()
        .map_err(|e| AgentError::Provider(format!("tui init: {}", e)))?;

    let (tx, mut rx) = mpsc::unbounded_channel::<UiEvent>();

    // Spawn an input task that translates crossterm events into our
    // `UiEvent`. Lives until the channel is dropped (i.e. until
    // `run` returns and `tx` goes out of scope after we drop the
    // last clone).
    let input_tx = tx.clone();
    let input_handle = tokio::spawn(async move {
        let mut events = EventStream::new();
        while let Some(Ok(ev)) = events.next().await {
            match ev {
                CtEvent::Key(k) => {
                    // crossterm fires Press AND Release on Windows;
                    // we only care about Press to avoid double-firing.
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

    let mut app = App::new();

    // Initial paint so the user sees the shell before the first
    // event arrives.
    guard.terminal_mut().draw(|f| ui::draw(f, &app)).map_err(io_err)?;

    // The render loop: drain events, mutate state, redraw. We cap
    // the redraw rate by coalescing — if multiple events arrive
    // between draws we drain them all before the next paint.
    let frame_budget = Duration::from_millis(16); // ~60fps ceiling
    loop {
        // Block for the next event, but with a short timeout so a
        // very chatty stream of resizes / keys doesn't block out
        // an explicit `should_quit` set elsewhere.
        let first = match tokio::time::timeout(frame_budget, rx.recv()).await {
            Ok(Some(ev)) => Some(ev),
            Ok(None) => break, // all senders dropped
            Err(_) => None,    // timeout — fall through to redraw if needed
        };

        if let Some(ev) = first {
            handle_event(&mut app, ev);
        }
        // Drain any further events that arrived while we were
        // handling the first — coalesces bursts so we redraw once.
        while let Ok(ev) = rx.try_recv() {
            handle_event(&mut app, ev);
        }

        if app.should_quit {
            break;
        }

        guard.terminal_mut().draw(|f| ui::draw(f, &app)).map_err(io_err)?;
    }

    input_handle.abort();
    Ok(())
}

fn handle_event(app: &mut App, ev: UiEvent) {
    match ev {
        UiEvent::Key(key) => {
            // Global shortcuts that work in any mode.
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            match key.code {
                KeyCode::Char('c') if ctrl => {
                    // Phase F: Ctrl+C exits when nothing else is
                    // happening. Phase G refines this to "cancel
                    // the in-flight turn first, exit only on a
                    // double-press."
                    app.request_quit();
                    return;
                }
                KeyCode::Char('d') if ctrl => {
                    app.request_quit();
                    return;
                }
                _ => {}
            }
            app.on_key(key);
        }
        UiEvent::Resize => {
            // App state is layout-agnostic; the next draw call
            // picks up the new size. Nothing to do.
        }
    }
}

fn io_err(e: std::io::Error) -> AgentError {
    AgentError::Provider(format!("tui io: {}", e))
}
