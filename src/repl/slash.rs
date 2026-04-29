//! Slash-command surface for the REPL. Mirrors `tools::Registry`: a small
//! trait, a registry that preserves registration order, and a couple of
//! built-in commands to exercise the seam.

use async_trait::async_trait;
use std::collections::HashMap;

use crate::agent::Agent;

#[async_trait]
pub trait SlashCommand: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    async fn run(&self, args: &str, agent: &mut Agent) -> SlashOutcome;
}

#[derive(Debug, PartialEq, Eq)]
pub enum SlashOutcome {
    /// Continue the REPL. Optional message is printed to stdout.
    Continue(Option<String>),
    /// Tear down the REPL.
    Exit,
}

#[derive(Default)]
pub struct SlashRegistry {
    commands: HashMap<String, Box<dyn SlashCommand>>,
    order: Vec<String>,
}

impl SlashRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<C: SlashCommand + 'static>(&mut self, cmd: C) {
        let name = cmd.name().to_string();
        if !self.commands.contains_key(&name) {
            self.order.push(name.clone());
        }
        self.commands.insert(name, Box::new(cmd));
    }

    pub fn get(&self, name: &str) -> Option<&dyn SlashCommand> {
        self.commands.get(name).map(|b| b.as_ref())
    }

    pub fn iter(&self) -> impl Iterator<Item = &dyn SlashCommand> {
        self.order
            .iter()
            .filter_map(|n| self.commands.get(n).map(|b| b.as_ref()))
    }

    /// Parse a line that begins with `/` (caller strips the slash) into a
    /// `(command, rest)` pair and dispatch. Returns `None` if the command is
    /// unknown — caller decides how to surface that.
    pub async fn dispatch(&self, line: &str, agent: &mut Agent) -> Option<SlashOutcome> {
        let line = line.trim_start();
        let (name, rest) = match line.find(char::is_whitespace) {
            Some(i) => (&line[..i], line[i..].trim_start()),
            None => (line, ""),
        };
        let cmd = self.get(name)?;
        Some(cmd.run(rest, agent).await)
    }

    /// Default REPL command set: `/clear`, `/help`, `/exit`.
    pub fn default_set() -> Self {
        let mut r = Self::new();
        r.register(Clear);
        r.register(Help);
        r.register(Exit);
        r
    }
}

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
        agent.clear().await;
        SlashOutcome::Continue(Some("(history cleared)".into()))
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

/// Render a `/help` listing for a registry. Kept here so the REPL can call it
/// directly without re-implementing trait introspection.
pub fn render_help(reg: &SlashRegistry) -> String {
    let mut out = String::from("Available commands:\n");
    for cmd in reg.iter() {
        out.push_str(&format!("  /{:<10} {}\n", cmd.name(), cmd.description()));
    }
    out.push_str("Ctrl-C cancels the current turn; Ctrl-D exits.");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::fake::FakeProvider;
    use crate::tools::Registry;
    use serde_json::json;

    fn fresh_agent() -> Agent {
        let provider = FakeProvider::new(vec![json!({"role":"assistant","content":"x"})]);
        Agent::new(Box::new(provider), Registry::new(), "m".into())
    }

    #[tokio::test]
    async fn dispatch_unknown_command_returns_none() {
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent();
        let out = reg.dispatch("nope", &mut agent).await;
        assert!(out.is_none());
    }

    #[tokio::test]
    async fn clear_resets_agent_history_but_keeps_pinned_system_prompt() {
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent().pin_system_prompt("sys").await;
        agent
            .memory
            .record(json!({"role":"user","content":"prior"}))
            .await;

        let out = reg.dispatch("clear", &mut agent).await.unwrap();
        assert!(matches!(out, SlashOutcome::Continue(Some(_))));
        assert_eq!(agent.memory.len(), 0);

        // System prompt is pinned, so it survives `clear()` and reappears
        // at the head of the next snapshot.
        let snap = agent.memory.snapshot().await;
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0]["role"], "system");
        assert_eq!(snap[0]["content"], "sys");
    }

    #[tokio::test]
    async fn exit_returns_exit_outcome() {
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent();
        let out = reg.dispatch("exit", &mut agent).await.unwrap();
        assert_eq!(out, SlashOutcome::Exit);
    }

    #[tokio::test]
    async fn dispatch_strips_command_name_from_args() {
        struct CaptureArgs(std::sync::Arc<std::sync::Mutex<String>>);
        #[async_trait]
        impl SlashCommand for CaptureArgs {
            fn name(&self) -> &str {
                "capture"
            }
            fn description(&self) -> &str {
                "test"
            }
            async fn run(&self, args: &str, _agent: &mut Agent) -> SlashOutcome {
                *self.0.lock().unwrap() = args.to_string();
                SlashOutcome::Continue(None)
            }
        }
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let mut reg = SlashRegistry::new();
        reg.register(CaptureArgs(captured.clone()));

        let mut agent = fresh_agent();
        reg.dispatch("capture  with args", &mut agent)
            .await
            .unwrap();
        assert_eq!(*captured.lock().unwrap(), "with args");
    }

    #[test]
    fn render_help_lists_all_registered_commands_in_order() {
        let reg = SlashRegistry::default_set();
        let s = render_help(&reg);
        let clear_pos = s.find("/clear").unwrap();
        let help_pos = s.find("/help").unwrap();
        let exit_pos = s.find("/exit").unwrap();
        assert!(clear_pos < help_pos);
        assert!(help_pos < exit_pos);
    }

    #[test]
    fn re_registering_replaces_but_keeps_position() {
        let mut reg = SlashRegistry::new();
        reg.register(Clear);
        reg.register(Clear);
        assert_eq!(reg.iter().count(), 1);
    }
}
