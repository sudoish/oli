//! Interactive REPL: rustyline for line editing, streaming output to stdout,
//! Ctrl-C cancels the in-flight turn, Ctrl-D exits.

use async_trait::async_trait;
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;
use serde_json::Value;
use std::io::Write;

use crate::agent::Agent;
use crate::error::{AgentError, Result};
use crate::hooks::{Hook, HookPayload};

pub mod slash;

use slash::{SlashOutcome, SlashRegistry};

const PROMPT: &str = "> ";

/// Drive an interactive session against `agent` until the user exits.
/// `plugin_slashes` is the bag of Lua-backed slash commands discovered
/// at startup; the binary threads them in alongside the built-ins.
pub async fn run(
    mut agent: Agent,
    plugin_slashes: Vec<Box<dyn slash::SlashCommand>>,
) -> Result<()> {
    let mut editor =
        DefaultEditor::new().map_err(|e| AgentError::Provider(format!("rustyline: {e}")))?;
    let mut registry = SlashRegistry::default_set();
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
    let mut sink = |s: &str| {
        print!("{s}");
        let _ = std::io::stdout().flush();
    };

    let cancelled;
    {
        let fut = agent.run_streaming(prompt, &mut sink);
        tokio::pin!(fut);

        tokio::select! {
            biased;
            _ = tokio::signal::ctrl_c() => {
                cancelled = true;
            }
            r = &mut fut => {
                cancelled = false;
                match r {
                    Ok(_) => println!(),
                    Err(e) => eprintln!("\nerror: {e}"),
                }
            }
        }
    }

    if cancelled {
        agent.memory.truncate(saved_len).await;
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

    async fn handle(&self, payload: &HookPayload<'_>) {
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
