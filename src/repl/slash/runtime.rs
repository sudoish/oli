use super::{SlashCommand, SlashOutcome};
use crate::agent::Agent;
use async_trait::async_trait;

pub struct Tools;

#[async_trait]
impl SlashCommand for Tools {
    fn name(&self) -> &str {
        "tools"
    }
    fn description(&self) -> &str {
        "list registered tools"
    }
    async fn run(&self, _args: &str, agent: &mut Agent) -> SlashOutcome {
        let mut out = String::new();
        let tools: Vec<&dyn crate::tools::Tool> = agent.tools.iter().collect();
        out.push_str(&format!("Registered tools ({}):\n", tools.len()));
        let pad = tools.iter().map(|t| t.name().len()).max().unwrap_or(0);
        for t in tools {
            out.push_str(&format!(
                "  {:<width$}  {}\n",
                t.name(),
                t.description(),
                width = pad
            ));
        }
        SlashOutcome::Continue(Some(out.trim_end().to_string()))
    }
}

pub struct System;

#[async_trait]
impl SlashCommand for System {
    fn name(&self) -> &str {
        "system"
    }
    fn description(&self) -> &str {
        "show pinned system prompt(s)"
    }
    async fn run(&self, _args: &str, agent: &mut Agent) -> SlashOutcome {
        let pinned = agent.memory.pinned().await;
        if pinned.is_empty() {
            return SlashOutcome::Continue(Some("(no system prompt pinned)".into()));
        }
        let mut out = format!("System prompt ({} pinned):\n", pinned.len());
        for (i, m) in pinned.iter().enumerate() {
            let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("?");
            let content = m.get("content").and_then(|v| v.as_str()).unwrap_or("");
            out.push_str(&format!("--- [{}] {} ---\n{}\n", i, role, content));
        }
        SlashOutcome::Continue(Some(out.trim_end().to_string()))
    }
}

pub struct Sessions;

#[async_trait]
impl SlashCommand for Sessions {
    fn name(&self) -> &str {
        "sessions"
    }
    fn description(&self) -> &str {
        "list saved sessions, newest first"
    }
    async fn run(&self, _args: &str, _agent: &mut Agent) -> SlashOutcome {
        let entries = crate::agent::memory::list_sessions();
        if entries.is_empty() {
            return SlashOutcome::Continue(Some(
                "(no saved sessions found in ~/.config/oli/sessions/)".into(),
            ));
        }
        let mut out = format!("Sessions ({}):\n", entries.len());
        for (i, e) in entries.iter().take(20).enumerate() {
            let when = e
                .mtime
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| {
                    let secs = d.as_secs();
                    format!("epoch+{}s", secs)
                })
                .unwrap_or_else(|| "?".into());
            out.push_str(&format!("  {:>2}. {}  ({})\n", i + 1, e.id, when));
        }
        if entries.len() > 20 {
            out.push_str(&format!("  ... and {} more\n", entries.len() - 20));
        }
        out.push_str("Resume with: oli run --conversation <id> -p <prompt>");
        SlashOutcome::Continue(Some(out))
    }
}

/// `/diagnostics [clear]` — show the recent operational log
/// (plugin warnings, MCP failures, provider quirks). Without args
/// renders the tail; with `clear` empties the ring buffer.
pub struct Diagnostics;

#[async_trait]
impl SlashCommand for Diagnostics {
    fn name(&self) -> &str {
        "diagnostics"
    }
    fn description(&self) -> &str {
        "show operational log (plugin/MCP/provider warnings); pass `clear` to wipe"
    }
    async fn run(&self, args: &str, _agent: &mut Agent) -> SlashOutcome {
        let trimmed = args.trim();
        if trimmed == "clear" {
            crate::diagnostics::clear();
            return SlashOutcome::Continue(Some("(diagnostics cleared)".into()));
        }
        let entries = crate::diagnostics::tail(50);
        if entries.is_empty() {
            return SlashOutcome::Continue(Some("(no diagnostics recorded)".into()));
        }
        let mut out = String::new();
        out.push_str(&format!("Recent diagnostics ({}):\n", entries.len()));
        for e in entries {
            out.push_str(&format!("  [{}] {}\n", e.level.label(), e.body));
        }
        SlashOutcome::Continue(Some(out.trim_end().to_string()))
    }
}
