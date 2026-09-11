use super::{SlashCommand, SlashOutcome};
use crate::agent::Agent;
use async_trait::async_trait;

pub struct Clear;

#[async_trait]
impl SlashCommand for Clear {
    fn name(&self) -> &str {
        "clear"
    }
    fn description(&self) -> &str {
        "drop conversation history (system prompt is preserved)"
    }
    async fn run(&self, _args: &str, agent: &mut Agent) -> SlashOutcome {
        match agent.clear().await {
            Ok(()) => SlashOutcome::Continue(Some("(history cleared)".into())),
            Err(e) => SlashOutcome::Continue(Some(format!("clear failed: {e}"))),
        }
    }
}

pub struct Help;

#[async_trait]
impl SlashCommand for Help {
    fn name(&self) -> &str {
        "help"
    }
    fn description(&self) -> &str {
        "list available slash commands"
    }
    async fn run(&self, _args: &str, _agent: &mut Agent) -> SlashOutcome {
        // Help renders by introspecting the registry, but the trait's `run`
        // doesn't have a registry handle. The REPL formats `/help` directly
        // before falling through to dispatch; this body is a fallback so a
        // standalone Help still yields something useful.
        SlashOutcome::Continue(Some("/clear, /help, /exit (Ctrl-D also exits)".into()))
    }
}

pub struct Exit;

#[async_trait]
impl SlashCommand for Exit {
    fn name(&self) -> &str {
        "exit"
    }
    fn description(&self) -> &str {
        "leave the REPL"
    }
    async fn run(&self, _args: &str, _agent: &mut Agent) -> SlashOutcome {
        SlashOutcome::Exit
    }
}
