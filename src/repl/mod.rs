//! Line-mode interactive REPL — Oli's optional interactive client.
//! Rustyline drives line editing + history; tool output streams
//! to stdout as it arrives; Ctrl-C cancels the current turn,
//! Ctrl-D exits.
//!
//! Slash commands live in [`slash`], so `/help`, `/cost`, `/clear`,
//! `/sessions` and plugin- or MCP-registered slashes stay available.
//!
//! [`ProgressHook`] surfaces tool calls inline (`→ Read(file=…)`)
//! so the user sees what's happening; the binary registers it
//! only in interactive mode so scripted `-p` runs stay quiet.

use async_trait::async_trait;
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;
use serde_json::Value;
use std::io::Write;

use crate::agent::{Agent, RunOutcome};
use crate::error::{AgentError, Result};
use crate::hooks::{Hook, HookOutcome, HookPayload};

pub mod slash;

use slash::{SlashOutcome, SlashRegistry};

const PROMPT: &str = "> ";

/// Drive an interactive session against `agent` until the user exits.
/// `plugin_slashes` is the bag of Lua-backed slash commands discovered
/// at startup; the binary threads them in alongside the built-ins.
/// `reloader`, when supplied, wires `/plugins reload` to a re-scan of
/// the plugin directories.
pub async fn run(
    mut agent: Agent,
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

    loop {
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
                return Ok(());
            }
            (Err(e), _) => {
                return Err(AgentError::Provider(format!("rustyline: {e}")));
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
            match registry.dispatch(rest, &mut agent).await {
                Some(SlashOutcome::Continue(Some(msg))) => println!("{msg}"),
                Some(SlashOutcome::Continue(None)) => {}
                Some(SlashOutcome::Exit) => return Ok(()),
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

        run_turn(&mut agent, trimmed).await;
    }
}

/// Run one model turn with streaming output and Ctrl-C cancellation.
/// On cancellation, conversation history is truncated back to its
/// pre-turn length so the next turn doesn't carry a half-completed
/// state into the prompt.
async fn run_turn(agent: &mut Agent, prompt: &str) {
    let saved_len = agent.memory.len();
    let mut sink = |ev: crate::providers::StreamEvent<'_>| {
        if let crate::providers::StreamEvent::Content(s) = ev {
            print!("{s}");
            let _ = std::io::stdout().flush();
        }
        // ToolArgsChunk events are ignored in line-mode REPL — the
        // line REPL deliberately omits streaming-diff previews.
    };

    let cancelled;
    {
        let fut = agent.run_streaming_outcome(prompt, &mut sink);
        tokio::pin!(fut);

        tokio::select! {
            biased;
            _ = tokio::signal::ctrl_c() => {
                cancelled = true;
            }
            r = &mut fut => {
                cancelled = false;
                match r {
                    Ok(RunOutcome::Completed(_)) => println!(),
                    Ok(RunOutcome::MaxTurnsExhausted { message, .. }) => {
                        println!("\n{message}")
                    }
                    Err(e) => crate::log_error!("\nerror: {e}"),
                }
            }
        }
    }

    if cancelled {
        if let Err(e) = agent.memory.truncate(saved_len).await {
            crate::log_error!("failed to truncate memory on cancel: {e}");
        }
        println!("\n(cancelled)");
    }
}

/// Live progress indicator for tool rounds. Prints a one-line
/// `→ Tool(args)` to stderr on `PreToolUse` so the user sees what
/// the model is reaching for *before* it runs. Stays on stderr so
/// it doesn't interleave with the streamed assistant content on
/// stdout. Args are clipped to fit a single line.
pub struct ProgressHook;

#[async_trait]
impl Hook for ProgressHook {
    fn name(&self) -> &str {
        "progress"
    }

    async fn handle(&self, payload: &HookPayload<'_>) -> HookOutcome {
        if let HookPayload::PreToolUse { tool, args } = payload {
            let preview = preview_args(args, 60);
            let line = if preview.is_empty() {
                format!("→ {}\n", tool)
            } else {
                format!("→ {}({})\n", tool, preview)
            };
            let mut err = std::io::stderr();
            let _ = err.write_all(line.as_bytes());
            let _ = err.flush();
        }
        HookOutcome::Continue
    }
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
