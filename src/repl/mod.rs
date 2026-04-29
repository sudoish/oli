//! Interactive REPL: rustyline for line editing, streaming output to stdout,
//! Ctrl-C cancels the in-flight turn, Ctrl-D exits.

use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;
use std::io::Write;

use crate::agent::Agent;
use crate::error::{AgentError, Result};

pub mod slash;

use slash::{SlashOutcome, SlashRegistry};

const PROMPT: &str = "> ";

/// Drive an interactive session against `agent` until the user exits.
pub async fn run(mut agent: Agent) -> Result<()> {
    let mut editor =
        DefaultEditor::new().map_err(|e| AgentError::Provider(format!("rustyline: {e}")))?;
    let registry = SlashRegistry::default_set();

    println!("agent ready. /help for commands, Ctrl-D to exit.");

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
