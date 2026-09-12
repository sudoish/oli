//! Line-mode interactive REPL — Oli's optional interactive client.
//! Rustyline drives line editing + history; tool output streams
//! to stdout as it arrives; Ctrl-C cancels the current turn,
//! Ctrl-D exits.
//!
//! Slash commands live in [`slash`], so `/help`, `/cost`, `/clear`,
//! `/sessions` and plugin- or MCP-registered slashes stay available.
//!
//! Runtime tool events are rendered inline (`→ Read(file=…)`) so the
//! user sees progress while scripted runs remain quiet.

use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;
use serde_json::Value;
use std::io::Write;

use crate::error::{AgentError, Result};
use crate::runtime::{CancellationToken, RuntimeCommand, RuntimeEvent, SessionRuntime};

pub mod slash;

use slash::{SlashOutcome, SlashRegistry};

const PROMPT: &str = "> ";

/// Drive an interactive session against `agent` until the user exits.
/// `plugin_slashes` is the bag of Lua-backed slash commands discovered
/// at startup; the binary threads them in alongside the built-ins.
/// `reloader`, when supplied, wires `/plugins reload` to a re-scan of
/// the plugin directories.
pub async fn run(
    mut runtime: SessionRuntime,
    plugin_slashes: Vec<Box<dyn slash::SlashCommand>>,
    reloader: Option<std::sync::Arc<crate::plugins::PluginReloader>>,
) -> Result<()> {
    let mut editor =
        DefaultEditor::new().map_err(|e| AgentError::Provider(format!("rustyline: {e}")))?;
    let mut registry = SlashRegistry::default_set_with_reloader(reloader);
    for s in plugin_slashes {
        registry.register_box(s);
    }

    println!("oli ready. /help for commands, Ctrl-D to exit.");

    let result = loop {
        let line = match read_line(editor).await {
            (Ok(l), ed) => {
                editor = ed;
                l
            }
            (Err(ReadlineError::Interrupted), ed) => {
                // Ctrl-C at the prompt — discard the line, prompt again.
                editor = ed;
                continue;
            }
            (Err(ReadlineError::Eof), _) => {
                // Ctrl-D — clean exit.
                println!();
                break Ok(());
            }
            (Err(e), _) => {
                break Err(AgentError::Provider(format!("rustyline: {e}")));
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let _ = editor.add_history_entry(trimmed);

        // Slash commands are dispatched without going through the model.
        if let Some(rest) = trimmed.strip_prefix('/') {
            // `/help` is rendered against the live registry rather than the
            // command's own `run`, since the command can't introspect peers.
            if rest == "help" || rest.starts_with("help ") {
                println!("{}", slash::render_help(&registry));
                continue;
            }
            match registry.dispatch(rest, runtime.agent_mut()).await {
                Some(SlashOutcome::Continue(Some(msg))) => println!("{msg}"),
                Some(SlashOutcome::Continue(None)) => {}
                Some(SlashOutcome::Exit) => break Ok(()),
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
                    println!("{message}");
                }
                None => println!(
                    "unknown command: /{}",
                    rest.split_whitespace().next().unwrap_or("")
                ),
            }
            continue;
        }

        run_turn(&mut runtime, trimmed).await;
    };

    runtime.finish().await;
    result
}

/// Run one model turn with streaming output and Ctrl-C cancellation.
/// On cancellation, conversation history is truncated back to its
/// pre-turn length so the next turn doesn't carry a half-completed
/// state into the prompt.
async fn run_turn(runtime: &mut SessionRuntime, prompt: &str) {
    let cancellation = CancellationToken::new();
    let signal = cancellation.clone();
    let signal_task = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal.cancel();
        }
    });
    let result = runtime
        .execute(
            RuntimeCommand::Prompt(prompt.to_string()),
            &cancellation,
            |event| match event {
                RuntimeEvent::Content { text } => {
                    print!("{text}");
                    let _ = std::io::stdout().flush();
                }
                RuntimeEvent::ToolStarted { name, args } => {
                    let preview = preview_args(&args, 60);
                    let line = if preview.is_empty() {
                        format!("→ {name}\n")
                    } else {
                        format!("→ {name}({preview})\n")
                    };
                    let mut err = std::io::stderr();
                    let _ = err.write_all(line.as_bytes());
                    let _ = err.flush();
                }
                RuntimeEvent::Completed { .. } => println!(),
                RuntimeEvent::MaxTurnsExhausted { message, .. } => {
                    println!("\n{message}")
                }
                RuntimeEvent::Cancelled => println!("\n(cancelled)"),
                RuntimeEvent::Error { message } => crate::log_error!("\nerror: {message}"),
                RuntimeEvent::ToolCallDelta { .. } => {}
            },
        )
        .await;
    signal_task.abort();
    let _ = result;
}

/// Single-line, char-bounded preview of tool args. Picks a couple of
/// common scalar fields (`file_path`, `command`, `pattern`, `path`,
/// `prompt`) when present and falls through to a JSON dump otherwise.
fn preview_args(args: &Value, max_len: usize) -> String {
    let priority = ["file_path", "command", "pattern", "path", "prompt"];
    for k in priority.iter() {
        if let Some(v) = args.get(*k).and_then(|v| v.as_str()) {
            return clip_one_line(&format!("{}={}", k, v), max_len);
        }
    }
    let raw = args.to_string();
    if raw == "{}" {
        return String::new();
    }
    clip_one_line(&raw, max_len)
}

fn clip_one_line(s: &str, max_len: usize) -> String {
    let one = s.replace('\n', " ");
    if one.chars().count() <= max_len {
        return one;
    }
    let mut out: String = one.chars().take(max_len.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// rustyline's `readline` is blocking, so it has to live on a blocking
/// thread to keep the tokio runtime free for everything else (including
/// the in-flight signal handler used by `run_turn`). The editor is moved
/// in and back out on each iteration.
async fn read_line(
    mut editor: DefaultEditor,
) -> (std::result::Result<String, ReadlineError>, DefaultEditor) {
    tokio::task::spawn_blocking(move || {
        let res = editor.readline(PROMPT);
        (res, editor)
    })
    .await
    .expect("readline task panicked")
}
