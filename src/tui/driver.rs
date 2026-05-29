//! Agent driver task. Owns the `Agent` + `SlashRegistry` so the
//! render loop can stay non-blocking while the model is talking.
//!
//! The TUI's render task and the driver task communicate over two
//! mpsc channels:
//!
//! - **commands** (UI → driver): `AgentCommand::Prompt` and
//!   `AgentCommand::Slash`. The driver processes them one at a
//!   time; the TUI gates submission on `Mode != Idle` so we never
//!   queue concurrent runs.
//! - **events** (driver → UI): the same `UiEvent` channel the
//!   input task uses. Driver pushes `TurnStarted`, `ContentChunk`,
//!   `TurnFinished`, `SystemNote`, `Quit`.
//!
//! Cancel: each command carries a `oneshot::Receiver<()>` that the
//! UI keeps the matching sender for. Pressing Ctrl+C during a
//! busy turn sends `()` on that oneshot, which races against the
//! agent's run. On cancel we truncate `Memory` to the pre-turn
//! `len()` so the next prompt doesn't carry a half-finished
//! assistant message back into context — same semantics as
//! `repl::run`.

use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::sync::oneshot;

use crate::agent::Agent;
use crate::error::Result;
use crate::plugins::PluginReloader;
use crate::repl::slash::{SlashCommand, SlashOutcome, SlashRegistry};
use crate::tui::event::UiEvent;

pub enum AgentCommand {
    /// Run a normal model turn. The body is what the user typed.
    Prompt {
        body: String,
        cancel: oneshot::Receiver<()>,
    },
    /// Dispatch a slash command. `line` is the post-`/` remainder
    /// (e.g. `cost`, `model claude-haiku-4.5`).
    Slash {
        line: String,
        cancel: oneshot::Receiver<()>,
    },
    /// Roll back the most recent user turn from the agent's
    /// memory. Driver replies via `UiEvent::UndoApplied` so the
    /// TUI can trim its transcript in lock-step. `load_into_input`
    /// is `true` when the trigger was Ctrl+E (edit-and-rerun) so
    /// the popped prompt is dropped back into the input box.
    Undo { load_into_input: bool },
    /// Tear-down signal so the driver task exits cleanly.
    Shutdown,
}

pub fn spawn(
    mut agent: Agent,
    plugin_slashes: Vec<Box<dyn SlashCommand>>,
    reloader: Option<Arc<PluginReloader>>,
    ui_tx: mpsc::UnboundedSender<UiEvent>,
) -> (mpsc::UnboundedSender<AgentCommand>, tokio::task::JoinHandle<()>) {
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<AgentCommand>();
    let mut registry = SlashRegistry::default_set_with_reloader(reloader);
    for s in plugin_slashes {
        registry.register_box(s);
    }

    let handle = tokio::spawn(async move {
        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                AgentCommand::Shutdown => break,
                AgentCommand::Prompt { body, cancel } => {
                    handle_prompt(&mut agent, body, cancel, &ui_tx).await;
                }
                AgentCommand::Slash { line, cancel } => {
                    handle_slash(&mut agent, &mut registry, line, cancel, &ui_tx).await;
                }
                AgentCommand::Undo { load_into_input } => {
                    let popped = agent.undo_last_user_turn().await;
                    let _ = ui_tx.send(UiEvent::UndoApplied {
                        prompt_body: popped,
                        load_into_input,
                    });
                }
            }
        }
    });

    (cmd_tx, handle)
}

async fn handle_prompt(
    agent: &mut Agent,
    body: String,
    mut cancel: oneshot::Receiver<()>,
    ui_tx: &mpsc::UnboundedSender<UiEvent>,
) {
    let saved_len = agent.memory.len();
    let _ = ui_tx.send(UiEvent::TurnStarted);

    // Streaming sink: each provider event is translated to a UiEvent
    // the render loop dispatches. Content deltas land as
    // `ContentChunk`; tool-args deltas land as `ToolArgsChunk` so the
    // streaming-diff peek can render before the call is dispatched.
    // Closure captures `ui_tx` by reference; sink lifetime is the
    // duration of the `run_streaming` call.
    let tx_for_sink = ui_tx.clone();
    let mut sink = move |ev: crate::providers::StreamEvent<'_>| match ev {
        crate::providers::StreamEvent::Content(s) => {
            let _ = tx_for_sink.send(UiEvent::ContentChunk(s.to_string()));
        }
        crate::providers::StreamEvent::ToolArgsChunk {
            provider_tool_id,
            name,
            accumulated_json,
            ..
        } => {
            let _ = tx_for_sink.send(UiEvent::ToolArgsChunk {
                provider_tool_id: provider_tool_id.to_string(),
                name: name.to_string(),
                accumulated_json: accumulated_json.to_string(),
            });
        }
    };

    let result: Result<String> = tokio::select! {
        biased;
        _ = &mut cancel => {
            // Cancel BEFORE checking the agent future would skip the
            // run; we race them. The biased select prefers cancel
            // when both are ready, so a cancel that landed before
            // run started fires immediately.
            agent.memory.truncate(saved_len).await;
            let _ = ui_tx.send(UiEvent::TurnCancelled);
            return;
        }
        res = agent.run_streaming(&body, &mut sink) => res,
    };

    // Surface the freshest usage to the status bar regardless of
    // whether the run succeeded — failed runs still consumed
    // tokens, and we want the gauge honest.
    let _ = ui_tx.send(UiEvent::UsageUpdate {
        last: agent.last_usage,
        session: agent.session_usage,
    });

    match result {
        Ok(content) => {
            let _ = ui_tx.send(UiEvent::TurnFinished {
                final_content: content,
            });
        }
        Err(e) => {
            let _ = ui_tx.send(UiEvent::TurnError(e.to_string()));
        }
    }
}

async fn handle_slash(
    agent: &mut Agent,
    registry: &mut SlashRegistry,
    line: String,
    _cancel: oneshot::Receiver<()>,
    ui_tx: &mpsc::UnboundedSender<UiEvent>,
) {
    // `/help` is rendered against the live registry — same special-
    // case the rustyline REPL uses. We surface the rendered text as
    // a SystemNote so the transcript shows it the same way.
    let trimmed = line.trim_start();
    if trimmed == "help" || trimmed.starts_with("help ") {
        let body = crate::repl::slash::render_help(registry);
        let _ = ui_tx.send(UiEvent::SystemNote(body));
        let _ = ui_tx.send(UiEvent::SlashFinished);
        return;
    }

    let outcome = registry.dispatch(trimmed, agent).await;
    match outcome {
        Some(SlashOutcome::Continue(Some(msg))) => {
            let _ = ui_tx.send(UiEvent::SystemNote(msg));
        }
        Some(SlashOutcome::Continue(None)) => {}
        Some(SlashOutcome::Exit) => {
            let _ = ui_tx.send(UiEvent::Quit);
            return;
        }
        Some(SlashOutcome::Rebuild {
            removed_names,
            added_slashes,
            message,
        }) => {
            for n in removed_names {
                registry.remove(&n);
            }
            for s in added_slashes {
                registry.register_box(s);
            }
            let _ = ui_tx.send(UiEvent::SystemNote(message));
        }
        None => {
            let head = trimmed.split_whitespace().next().unwrap_or("");
            let _ = ui_tx.send(UiEvent::SystemNote(format!(
                "unknown command: /{}",
                head
            )));
        }
    }
    let _ = ui_tx.send(UiEvent::SlashFinished);
}
