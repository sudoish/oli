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
        self.register_box(Box::new(cmd));
    }

    /// Insert an already-boxed slash command. Plugin loader uses this.
    pub fn register_box(&mut self, cmd: Box<dyn SlashCommand>) {
        let name = cmd.name().to_string();
        if !self.commands.contains_key(&name) {
            self.order.push(name.clone());
        }
        self.commands.insert(name, cmd);
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

    /// Default REPL command set. Order is the order they show up in `/help`.
    pub fn default_set() -> Self {
        let mut r = Self::new();
        r.register(Clear);
        r.register(Help);
        r.register(Cost);
        r.register(Tools);
        r.register(System);
        r.register(MemoryCmd);
        r.register(Compact);
        r.register(Provider);
        r.register(Model);
        r.register(Sessions);
        r.register(Plugins);
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

pub struct Cost;

#[async_trait]
impl SlashCommand for Cost {
    fn name(&self) -> &str {
        "cost"
    }
    fn description(&self) -> &str {
        "show last call's token usage"
    }
    async fn run(&self, _args: &str, agent: &mut Agent) -> SlashOutcome {
        let msg = match agent.last_usage {
            Some(u) => format!(
                "last call: {} prompt + {} completion = {} tokens",
                u.prompt_tokens, u.completion_tokens, u.total_tokens
            ),
            None => "last call: (no usage yet — provider may not report it)".into(),
        };
        SlashOutcome::Continue(Some(msg))
    }
}

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

pub struct MemoryCmd;

#[async_trait]
impl SlashCommand for MemoryCmd {
    fn name(&self) -> &str {
        "memory"
    }
    fn description(&self) -> &str {
        "memory state — `stats` (default) or `dump`"
    }
    async fn run(&self, args: &str, agent: &mut Agent) -> SlashOutcome {
        let sub = args.trim();
        match sub {
            "" | "stats" => {
                let pinned = agent.memory.pinned().await.len();
                let recorded = agent.memory.len();
                let snap_len = agent.memory.snapshot().await.len();
                let synthesized = snap_len.saturating_sub(pinned + recorded);
                let mut out = format!(
                    "records (logical): {}\npinned messages:   {}\nsynthesized:       {} (compaction summary)",
                    recorded, pinned, synthesized
                );
                if let Some(u) = agent.last_usage {
                    out.push_str(&format!("\nlast prompt tokens: {}", u.prompt_tokens));
                }
                SlashOutcome::Continue(Some(out))
            }
            "dump" => {
                let snap = agent.memory.snapshot().await;
                let mut out = format!("snapshot ({} messages):\n", snap.len());
                for (i, m) in snap.iter().enumerate() {
                    let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("?");
                    let preview = render_message_preview(m);
                    out.push_str(&format!("  [{}] {}: {}\n", i, role, preview));
                }
                SlashOutcome::Continue(Some(out.trim_end().to_string()))
            }
            other => SlashOutcome::Continue(Some(format!(
                "unknown subcommand: {} (try `stats` or `dump`)",
                other
            ))),
        }
    }
}

pub struct Compact;

#[async_trait]
impl SlashCommand for Compact {
    fn name(&self) -> &str {
        "compact"
    }
    fn description(&self) -> &str {
        "force a compaction pass on memory now"
    }
    async fn run(&self, _args: &str, agent: &mut Agent) -> SlashOutcome {
        let before = agent.memory.snapshot().await.len();
        match agent.force_compact().await {
            Ok(()) => {
                let after = agent.memory.snapshot().await.len();
                let msg = if after < before {
                    format!("compacted: {} → {} messages in snapshot", before, after)
                } else {
                    "compaction declined (not enough live messages, or model returned empty summary)"
                        .into()
                };
                SlashOutcome::Continue(Some(msg))
            }
            Err(e) => SlashOutcome::Continue(Some(format!("compact failed: {}", e))),
        }
    }
}

/// One-line preview of a snapshot message for `/memory dump`.
fn render_message_preview(m: &serde_json::Value) -> String {
    if let Some(content) = m.get("content").and_then(|v| v.as_str()) {
        if !content.is_empty() {
            return truncate_for_preview(content);
        }
    }
    if let Some(tcs) = m.get("tool_calls").and_then(|v| v.as_array()) {
        let names: Vec<&str> = tcs
            .iter()
            .filter_map(|t| {
                t.get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
            })
            .collect();
        return format!("(tool_calls: {})", names.join(", "));
    }
    "(empty)".into()
}

fn truncate_for_preview(s: &str) -> String {
    const LIMIT: usize = 100;
    let one_line = s.replace('\n', " ");
    if one_line.len() <= LIMIT {
        return one_line;
    }
    let mut cut = LIMIT;
    while cut > 0 && !one_line.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &one_line[..cut])
}

pub struct Provider;

#[async_trait]
impl SlashCommand for Provider {
    fn name(&self) -> &str {
        "provider"
    }
    fn description(&self) -> &str {
        "list providers, or `<name>` to swap"
    }
    async fn run(&self, args: &str, agent: &mut Agent) -> SlashOutcome {
        let cfg = match agent.cfg.clone() {
            Some(c) => c,
            None => {
                return SlashOutcome::Continue(Some(
                    "no config available — agent constructed without `with_config`".into(),
                ));
            }
        };
        let arg = args.trim();
        if arg.is_empty() {
            let mut names: Vec<&String> = cfg.providers.keys().collect();
            names.sort();
            let mut out = format!("Configured providers ({}):\n", names.len());
            for name in names {
                let pcfg = &cfg.providers[name];
                let active = if name == &agent.provider_name {
                    " [active]"
                } else if name == &cfg.default_provider {
                    " (default)"
                } else {
                    ""
                };
                out.push_str(&format!(
                    "  {:<14}  kind={} url={}{}\n",
                    name, pcfg.kind, pcfg.base_url, active
                ));
            }
            return SlashOutcome::Continue(Some(out.trim_end().to_string()));
        }

        // Swap path. Delegate to the central provider factory so we
        // pick up any new kinds without duplicating the dispatch.
        let pcfg = match cfg.provider(arg) {
            Ok(p) => p,
            Err(e) => return SlashOutcome::Continue(Some(format!("error: {}", e))),
        };
        let new_model = pcfg
            .default_model
            .clone()
            .or_else(|| cfg.default_model.clone())
            .unwrap_or_else(|| agent.model.clone());
        let new_provider = match crate::providers::build(cfg.as_ref(), arg) {
            Ok(p) => p,
            Err(e) => return SlashOutcome::Continue(Some(format!("error: {}", e))),
        };

        agent.provider = new_provider;
        agent.provider_name = arg.to_string();
        agent.model = new_model.clone();
        agent.caps = agent.resolve_caps(&new_model);
        agent.last_usage = None;

        SlashOutcome::Continue(Some(format!(
            "switched to provider {} (model {})",
            arg, new_model
        )))
    }
}

pub struct Model;

#[async_trait]
impl SlashCommand for Model {
    fn name(&self) -> &str {
        "model"
    }
    fn description(&self) -> &str {
        "show current model, list available, or `<id>` to swap"
    }
    async fn run(&self, args: &str, agent: &mut Agent) -> SlashOutcome {
        let arg = args.trim();
        if arg.is_empty() {
            // Try to enumerate via the provider's `list_models` method.
            let mut out = format!("current model: {}\n", agent.model);
            match agent.provider.list_models().await {
                Ok(ids) if !ids.is_empty() => {
                    out.push_str(&format!("\navailable ({}):", ids.len()));
                    for id in ids {
                        let active = if id == agent.model { " ←" } else { "" };
                        out.push_str(&format!("\n  {}{}", id, active));
                    }
                }
                Ok(_) => out.push_str(
                    "\n(provider doesn't expose a model list — pass `<id>` to switch anyway)",
                ),
                Err(e) => out.push_str(&format!("\n(could not list models: {})", e)),
            }
            return SlashOutcome::Continue(Some(out));
        }

        // Swap path. Caps are recomputed from the new model id; the same
        // provider client serves it.
        agent.model = arg.to_string();
        agent.caps = crate::agent::caps_for(arg);
        agent.last_usage = None;
        SlashOutcome::Continue(Some(format!("model switched to {}", arg)))
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
        out.push_str("Resume with: oli --resume <id>");
        SlashOutcome::Continue(Some(out))
    }
}

pub struct Plugins;

#[async_trait]
impl SlashCommand for Plugins {
    fn name(&self) -> &str {
        "plugins"
    }
    fn description(&self) -> &str {
        "list loaded Lua plugins"
    }
    async fn run(&self, _args: &str, agent: &mut Agent) -> SlashOutcome {
        if agent.plugin_manifest.is_empty() {
            return SlashOutcome::Continue(Some(
                "(no plugins loaded — drop .lua files into ~/.config/oli/plugins/ \
                 or .oli/plugins/)"
                    .into(),
            ));
        }
        let mut out = format!("Loaded plugins ({}):\n", agent.plugin_manifest.len());
        for m in &agent.plugin_manifest {
            let v = m.version.as_deref().unwrap_or("?");
            out.push_str(&format!(
                "  {} (v{})  source={}\n",
                m.name,
                v,
                m.source.display()
            ));
            if !m.tools.is_empty() {
                out.push_str(&format!("    tools: {}\n", m.tools.join(", ")));
            }
            if !m.slash_commands.is_empty() {
                out.push_str(&format!("    slash: /{}\n", m.slash_commands.join(", /")));
            }
            if !m.hook_events.is_empty() {
                out.push_str(&format!("    hooks: {}\n", m.hook_events.join(", ")));
            }
        }
        SlashOutcome::Continue(Some(out.trim_end().to_string()))
    }
}

/// `/mcp` — introspection over the Model Context Protocol clients
/// connected at startup. Subcommands:
///   - (no args)         list servers, health, tool count
///   - `tools <server>`  enumerate the tools that server exposes
///   - `logs <server>`   show captured stderr (stdio servers only)
///   - `restart <server>` re-run connect+initialize for a server
pub struct Mcp {
    handles: std::sync::Arc<Vec<crate::mcp::McpHandle>>,
}

impl Mcp {
    pub fn new(handles: std::sync::Arc<Vec<crate::mcp::McpHandle>>) -> Self {
        Self { handles }
    }
}

#[async_trait]
impl SlashCommand for Mcp {
    fn name(&self) -> &str {
        "mcp"
    }
    fn description(&self) -> &str {
        "list MCP servers; `tools <server>` or `logs <server>` for detail"
    }
    async fn run(&self, args: &str, _agent: &mut Agent) -> SlashOutcome {
        let args = args.trim();
        if self.handles.is_empty() {
            return SlashOutcome::Continue(Some(
                "(no MCP servers configured — add a [mcp.servers.*] table to your config)".into(),
            ));
        }

        // Subcommand split: first token is the verb, rest is the
        // server name. `<empty>` falls through to the listing path.
        let (sub, rest) = match args.find(char::is_whitespace) {
            Some(i) => (&args[..i], args[i..].trim()),
            None => (args, ""),
        };

        match sub {
            "" => {
                let mut out = format!("MCP servers ({}):\n", self.handles.len());
                for h in self.handles.iter() {
                    let s = h.server.lock().await;
                    let (status, detail) = match &s.health {
                        crate::mcp::HealthState::Healthy => ("healthy", String::new()),
                        crate::mcp::HealthState::Down(r) => ("down", format!(" ({})", r)),
                    };
                    out.push_str(&format!(
                        "  {:<14}  {:<8}  {} tool(s){}\n",
                        h.name,
                        status,
                        s.tools.len(),
                        detail
                    ));
                }
                SlashOutcome::Continue(Some(out.trim_end().to_string()))
            }
            "tools" => {
                let target = rest;
                if target.is_empty() {
                    return SlashOutcome::Continue(Some("usage: /mcp tools <server>".into()));
                }
                let h = match self.handles.iter().find(|h| h.name == target) {
                    Some(h) => h,
                    None => {
                        return SlashOutcome::Continue(Some(format!(
                            "unknown MCP server: {}",
                            target
                        )));
                    }
                };
                let s = h.server.lock().await;
                if s.tools.is_empty() {
                    return SlashOutcome::Continue(Some(format!(
                        "(server `{}` exposes no tools)",
                        target
                    )));
                }
                let mut out = format!("Tools from `{}` ({}):\n", target, s.tools.len());
                let pad = s.tools.iter().map(|t| t.name.len()).max().unwrap_or(0);
                for t in &s.tools {
                    out.push_str(&format!(
                        "  {:<width$}  {}\n",
                        t.name,
                        t.description,
                        width = pad
                    ));
                }
                SlashOutcome::Continue(Some(out.trim_end().to_string()))
            }
            "logs" => {
                let target = rest;
                if target.is_empty() {
                    return SlashOutcome::Continue(Some("usage: /mcp logs <server>".into()));
                }
                let h = match self.handles.iter().find(|h| h.name == target) {
                    Some(h) => h,
                    None => {
                        return SlashOutcome::Continue(Some(format!(
                            "unknown MCP server: {}",
                            target
                        )));
                    }
                };
                let s = h.server.lock().await;
                let logs = s.stderr_snapshot().await;
                let body = if logs.is_empty() {
                    "(no stderr captured)".to_string()
                } else {
                    logs
                };
                SlashOutcome::Continue(Some(format!("--- stderr from `{}` ---\n{}", target, body)))
            }
            "restart" => {
                let target = rest;
                if target.is_empty() {
                    return SlashOutcome::Continue(Some("usage: /mcp restart <server>".into()));
                }
                let h = match self.handles.iter().find(|h| h.name == target) {
                    Some(h) => h,
                    None => {
                        return SlashOutcome::Continue(Some(format!(
                            "unknown MCP server: {}",
                            target
                        )));
                    }
                };
                let mut s = h.server.lock().await;
                match s.restart().await {
                    Ok(()) => SlashOutcome::Continue(Some(format!(
                        "restarted `{}` — {} tool(s) available",
                        target,
                        s.tools.len()
                    ))),
                    Err(e) => SlashOutcome::Continue(Some(format!(
                        "restart of `{}` failed: {}",
                        target, e
                    ))),
                }
            }
            other => SlashOutcome::Continue(Some(format!(
                "unknown subcommand: {} (try `tools <server>`, `logs <server>`, \
                 or `restart <server>`)",
                other
            ))),
        }
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
        // Phase-2 batch is registered between help and exit.
        for name in &["/cost", "/tools", "/system", "/memory", "/compact"] {
            let pos = s.find(name).unwrap_or_else(|| panic!("missing {}", name));
            assert!(pos > help_pos, "{} should come after /help", name);
            assert!(pos < exit_pos, "{} should come before /exit", name);
        }
    }

    #[tokio::test]
    async fn cost_reports_no_usage_yet_when_agent_has_not_run() {
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent();
        let out = reg.dispatch("cost", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(msg)) => {
                assert!(msg.contains("no usage"));
            }
            _ => panic!("expected Continue(Some(_)), got {:?}", out),
        }
    }

    #[tokio::test]
    async fn cost_reports_token_breakdown_when_usage_present() {
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent();
        agent.last_usage = Some(crate::providers::Usage {
            prompt_tokens: 10,
            completion_tokens: 4,
            total_tokens: 14,
        });
        let out = reg.dispatch("cost", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(msg)) => {
                assert!(msg.contains("10 prompt"));
                assert!(msg.contains("4 completion"));
                assert!(msg.contains("14"));
            }
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn tools_lists_registered_tool_names() {
        let reg = SlashRegistry::default_set();
        let mut tools = crate::tools::Registry::new();
        tools.register(crate::tools::read::Read);
        tools.register(crate::tools::glob::Glob);
        let provider = FakeProvider::new(vec![json!({"role":"assistant","content":"x"})]);
        let mut agent = Agent::new(Box::new(provider), tools, "m".into());

        let out = reg.dispatch("tools", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(msg)) => {
                assert!(msg.contains("Read"));
                assert!(msg.contains("Glob"));
                assert!(msg.contains("Registered tools (2)"));
            }
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn system_shows_pinned_content() {
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent().pin_system_prompt("you are helpful").await;
        let out = reg.dispatch("system", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(msg)) => {
                assert!(msg.contains("you are helpful"));
                assert!(msg.contains("system"));
            }
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn system_reports_when_nothing_is_pinned() {
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent();
        let out = reg.dispatch("system", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(msg)) => {
                assert!(msg.to_lowercase().contains("no system prompt"));
            }
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn memory_stats_reports_record_and_pinned_counts() {
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent().pin_system_prompt("sys").await;
        agent
            .memory
            .record(json!({"role":"user","content":"hi"}))
            .await;
        let out = reg.dispatch("memory", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(msg)) => {
                assert!(msg.contains("records (logical): 1"));
                assert!(msg.contains("pinned messages:   1"));
            }
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn memory_dump_renders_each_message() {
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent();
        agent
            .memory
            .record(json!({"role":"user","content":"first"}))
            .await;
        agent
            .memory
            .record(json!({"role":"assistant","content":"second"}))
            .await;
        let out = reg.dispatch("memory dump", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(msg)) => {
                assert!(msg.contains("[0] user: first"));
                assert!(msg.contains("[1] assistant: second"));
            }
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn provider_lists_configured_entries_when_called_without_args() {
        let reg = SlashRegistry::default_set();
        let cfg = std::sync::Arc::new(crate::config::Config::env_default());
        let provider = FakeProvider::new(vec![json!({"role":"assistant","content":"x"})]);
        let mut agent = Agent::new(Box::new(provider), Registry::new(), "m".into())
            .with_config(cfg, "openrouter");

        let out = reg.dispatch("provider", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(msg)) => {
                assert!(msg.contains("Configured providers"));
                assert!(msg.contains("openrouter"));
                assert!(msg.contains("[active]"));
            }
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn provider_swap_updates_model_and_caps() {
        // Build a config with two providers; swap to the non-default one
        // and verify the agent's model + caps change accordingly.
        let toml = r#"
default_provider = "ollama"
[providers.ollama]
kind          = "openai-compat"
base_url      = "http://localhost:11434/v1"
api_key       = "ollama"
default_model = "qwen2.5-coder:7b"

[providers.cloud]
kind          = "openai-compat"
base_url      = "https://api.example.com/v1"
api_key       = "x"
default_model = "anthropic/claude-haiku-4.5"
"#;
        let cfg = std::sync::Arc::new(crate::config::Config::from_str(toml).unwrap());
        let provider = FakeProvider::new(vec![]);
        let mut agent = Agent::new(
            Box::new(provider),
            Registry::new(),
            "qwen2.5-coder:7b".into(),
        )
        .with_config(cfg, "ollama");

        assert_eq!(agent.model, "qwen2.5-coder:7b");
        assert!(!agent.caps.supports_native_tool_calls);

        let out = reg_dispatch_provider("provider cloud", &mut agent).await;
        assert!(out.contains("switched to provider cloud"));
        assert_eq!(agent.provider_name, "cloud");
        assert_eq!(agent.model, "anthropic/claude-haiku-4.5");
        assert!(agent.caps.supports_native_tool_calls);
    }

    #[tokio::test]
    async fn provider_swap_to_unknown_name_is_error_message_not_panic() {
        let cfg = std::sync::Arc::new(crate::config::Config::env_default());
        let provider = FakeProvider::new(vec![]);
        let mut agent = Agent::new(Box::new(provider), Registry::new(), "m".into())
            .with_config(cfg, "openrouter");
        let out = reg_dispatch_provider("provider missing", &mut agent).await;
        assert!(out.contains("unknown provider"));
    }

    #[tokio::test]
    async fn model_without_args_reports_current_and_lists_when_provider_supports_it() {
        let reg = SlashRegistry::default_set();
        let provider = FakeProvider::new(vec![]);
        let mut agent = Agent::new(Box::new(provider), Registry::new(), "m".into());
        let out = reg.dispatch("model", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(msg)) => {
                assert!(msg.contains("current model: m"));
                // FakeProvider uses the trait default, returns empty —
                // command should mention that gracefully.
                assert!(msg.contains("doesn't expose a model list"));
            }
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn model_swap_recomputes_caps() {
        let reg = SlashRegistry::default_set();
        let provider = FakeProvider::new(vec![]);
        let mut agent = Agent::new(
            Box::new(provider),
            Registry::new(),
            "qwen2.5-coder:7b".into(),
        );
        assert!(!agent.caps.supports_native_tool_calls);

        let out = reg
            .dispatch("model anthropic/claude-haiku-4.5", &mut agent)
            .await
            .unwrap();
        match out {
            SlashOutcome::Continue(Some(msg)) => assert!(msg.contains("model switched")),
            _ => panic!(),
        }
        assert_eq!(agent.model, "anthropic/claude-haiku-4.5");
        assert!(agent.caps.supports_native_tool_calls);
    }

    /// Helper: runs the `/provider` command and pulls the message out so
    /// callers don't need to repeat the SlashOutcome match.
    async fn reg_dispatch_provider(line: &str, agent: &mut Agent) -> String {
        let reg = SlashRegistry::default_set();
        match reg.dispatch(line, agent).await.unwrap() {
            SlashOutcome::Continue(Some(s)) => s,
            other => panic!("unexpected outcome: {:?}", other),
        }
    }

    #[tokio::test]
    async fn memory_unknown_subcommand_returns_helpful_message() {
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent();
        let out = reg.dispatch("memory wat", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(msg)) => {
                assert!(msg.contains("unknown subcommand"));
                assert!(msg.contains("stats"));
                assert!(msg.contains("dump"));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn re_registering_replaces_but_keeps_position() {
        let mut reg = SlashRegistry::new();
        reg.register(Clear);
        reg.register(Clear);
        assert_eq!(reg.iter().count(), 1);
    }
}
