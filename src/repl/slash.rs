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

pub enum SlashOutcome {
    /// Continue the REPL. Optional message is printed to stdout.
    Continue(Option<String>),
    /// Tear down the REPL.
    Exit,
    /// Reload triggered. The slash command has already mutated the
    /// agent's tool/hook registries in place. The REPL drops every
    /// slash whose name appears in `removed_names` and registers
    /// each of `added_slashes` in their place.
    Rebuild {
        removed_names: Vec<String>,
        added_slashes: Vec<Box<dyn SlashCommand>>,
        message: String,
    },
}

// Manual PartialEq for SlashOutcome — Box<dyn SlashCommand> doesn't
// implement Eq, so we compare the variant + the comparable fields.
// Tests only care about the `Continue(_)` and `Exit` shapes anyway.
impl PartialEq for SlashOutcome {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Continue(a), Self::Continue(b)) => a == b,
            (Self::Exit, Self::Exit) => true,
            (
                Self::Rebuild {
                    removed_names: a,
                    message: ma,
                    ..
                },
                Self::Rebuild {
                    removed_names: b,
                    message: mb,
                    ..
                },
            ) => a == b && ma == mb,
            _ => false,
        }
    }
}

impl std::fmt::Debug for SlashOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Continue(s) => write!(f, "Continue({:?})", s),
            Self::Exit => write!(f, "Exit"),
            Self::Rebuild {
                removed_names,
                added_slashes,
                message,
            } => f
                .debug_struct("Rebuild")
                .field("removed_names", removed_names)
                .field("added_slashes_count", &added_slashes.len())
                .field("message", message)
                .finish(),
        }
    }
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

    /// Drop a registered command by name. Used by `/plugins reload`
    /// to clear stale plugin slashes before adding the fresh batch.
    /// Returns `true` if a command with that name was present.
    pub fn remove(&mut self, name: &str) -> bool {
        let had = self.commands.remove(name).is_some();
        if had {
            self.order.retain(|n| n != name);
        }
        had
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

    /// Default REPL command set without a `/plugins` reloader.
    /// Tests reach for this since they don't exercise plugin
    /// reloading; the binary calls `default_set_with_reloader`.
    /// Order is the order they show up in `/help`.
    #[cfg(test)]
    pub fn default_set() -> Self {
        Self::default_set_with_reloader(None)
    }

    /// Same as `default_set`, but `Plugins` carries the supplied
    /// reloader so `/plugins reload` re-scans the plugin dirs and
    /// swaps registrations in place.
    pub fn default_set_with_reloader(
        reloader: Option<std::sync::Arc<crate::plugins::PluginReloader>>,
    ) -> Self {
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
        r.register(Plugins::new(reloader));
        r.register(ConfigCmd);
        r.register(Diagnostics);
        r.register(Paths);
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
        "show last call + session-total token usage"
    }
    async fn run(&self, _args: &str, agent: &mut Agent) -> SlashOutcome {
        let last = match agent.last_usage {
            Some(u) => format!(
                "last call: {} prompt + {} completion = {} tokens",
                u.prompt_tokens, u.completion_tokens, u.total_tokens
            ),
            None => "last call: (no usage yet — provider may not report it)".into(),
        };
        let s = agent.session_usage;
        let session = if s.total_tokens == 0 && s.prompt_tokens == 0 && s.completion_tokens == 0 {
            "session: (no usage recorded yet)".to_string()
        } else {
            format!(
                "session: {} prompt + {} completion = {} tokens",
                s.prompt_tokens, s.completion_tokens, s.total_tokens
            )
        };
        SlashOutcome::Continue(Some(format!("{}\n{}", last, session)))
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

/// `/plugins` — list loaded Lua plugins. Subcommand:
///   - (no args) list manifest entries
///   - `reload`  re-scan plugin dirs, swap registered tools / hooks /
///               slashes atomically without restarting the session
pub struct Plugins {
    reloader: Option<std::sync::Arc<crate::plugins::PluginReloader>>,
}

impl Plugins {
    pub fn new(reloader: Option<std::sync::Arc<crate::plugins::PluginReloader>>) -> Self {
        Self { reloader }
    }
}

#[async_trait]
impl SlashCommand for Plugins {
    fn name(&self) -> &str {
        "plugins"
    }
    fn description(&self) -> &str {
        "list loaded Lua plugins; `reload` re-scans plugin dirs"
    }
    async fn run(&self, args: &str, agent: &mut Agent) -> SlashOutcome {
        match args.trim() {
            "" => render_listing(agent),
            "reload" => self.run_reload(agent).await,
            other => SlashOutcome::Continue(Some(format!(
                "unknown /plugins subcommand: {} (try `reload` or no args)",
                other
            ))),
        }
    }
}

fn render_listing(agent: &mut Agent) -> SlashOutcome {
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

impl Plugins {
    async fn run_reload(&self, agent: &mut Agent) -> SlashOutcome {
        let Some(reloader) = self.reloader.as_ref() else {
            return SlashOutcome::Continue(Some(
                "(plugin reload unavailable — no reloader bound at startup)".into(),
            ));
        };

        // Sweep the prior plugin contributions out of the agent's
        // tool / hook / slash registries. Hooks are removed by name
        // (plugin id); slashes go up the wire as `removed_names` so
        // the REPL can drop them atomically with the new ones.
        let prior = std::mem::take(&mut agent.plugin_manifest);
        let mut removed_slash_names = Vec::new();
        for m in &prior {
            for t in &m.tools {
                agent.tools.remove(t);
            }
            agent.hooks.remove_by_name(&m.name);
            for s in &m.slash_commands {
                removed_slash_names.push(s.clone());
            }
        }

        // Pull a fresh batch off disk and install the contributions
        // back into the agent. Plugins that fail to load surface as
        // eprintln lines from the loader; the rest still install.
        let fresh = reloader.reload().await;
        for t in fresh.tools {
            agent.tools.register_box(t);
        }
        for h in fresh.hooks {
            agent.hooks.register_box(h);
        }
        agent.plugin_manifest = fresh.manifest;

        let plugin_count = agent.plugin_manifest.len();
        let tool_count: usize = agent.plugin_manifest.iter().map(|m| m.tools.len()).sum();
        let hook_count: usize = agent
            .plugin_manifest
            .iter()
            .map(|m| m.hook_events.len())
            .sum();
        let added_slash_count = fresh.slash_commands.len();

        SlashOutcome::Rebuild {
            removed_names: removed_slash_names,
            added_slashes: fresh.slash_commands,
            message: format!(
                "(reloaded {} plugin{}, {} tool{}, {} hook{}, {} slash{})",
                plugin_count,
                if plugin_count == 1 { "" } else { "s" },
                tool_count,
                if tool_count == 1 { "" } else { "s" },
                hook_count,
                if hook_count == 1 { "" } else { "s" },
                added_slash_count,
                if added_slash_count == 1 { "" } else { "es" },
            ),
        }
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

/// `/config reload` — re-read the config file (global +
/// project-local) and apply changes to the running agent
/// without restarting. Memory, transcript, system prompt,
/// session token totals — all survive. The active provider gets
/// rebuilt only if its `default_provider` or its provider-block
/// config changed; the same is true for `[policy]` and the
/// model id.
pub struct ConfigCmd;

#[async_trait]
impl SlashCommand for ConfigCmd {
    fn name(&self) -> &str {
        "config"
    }
    fn description(&self) -> &str {
        "config tools (`/config reload` to pick up edits to config.toml)"
    }
    async fn run(&self, args: &str, agent: &mut Agent) -> SlashOutcome {
        let arg = args.trim();
        match arg {
            "reload" | "" => reload_config(agent).await,
            other => SlashOutcome::Continue(Some(format!(
                "unknown subcommand `{}`. try: /config reload",
                other
            ))),
        }
    }
}

async fn reload_config(agent: &mut Agent) -> SlashOutcome {
    let new_cfg = match crate::config::Config::load_or_default() {
        Ok(c) => std::sync::Arc::new(c),
        Err(e) => {
            return SlashOutcome::Continue(Some(format!("config reload failed: {}", e)));
        }
    };

    // Pick the new active provider: if `default_provider`
    // changed or the agent's current provider name vanished
    // from the new config, switch to the new default.
    let target_provider = if !agent.provider_name.is_empty()
        && new_cfg.providers.contains_key(&agent.provider_name)
    {
        agent.provider_name.clone()
    } else {
        new_cfg.default_provider.clone()
    };

    let new_model = match new_cfg.model_for(&target_provider) {
        Ok(m) => m,
        Err(e) => return SlashOutcome::Continue(Some(format!("config reload failed: {}", e))),
    };

    let new_provider = match crate::providers::build(new_cfg.as_ref(), &target_provider) {
        Ok(p) => p,
        Err(e) => return SlashOutcome::Continue(Some(format!("config reload failed: {}", e))),
    };

    let mut changes: Vec<String> = Vec::new();
    if agent.provider_name != target_provider {
        changes.push(format!(
            "provider: {} → {}",
            if agent.provider_name.is_empty() {
                "(none)"
            } else {
                &agent.provider_name
            },
            target_provider
        ));
    }
    if agent.model != new_model {
        changes.push(format!("model: {} → {}", agent.model, new_model));
    }

    agent.provider = new_provider;
    agent.provider_name = target_provider;
    agent.model = new_model.clone();
    agent.caps = agent.resolve_caps(&new_model);
    agent.policy = Box::new(crate::policy::ConfigPolicy::from_config(&new_cfg.policy));
    agent.cfg = Some(new_cfg);
    // last_usage doesn't survive a swap — the prior usage was
    // measured against a different model/provider.
    agent.last_usage = None;

    let summary = if changes.is_empty() {
        "config reloaded (no provider/model change)".to_string()
    } else {
        format!("config reloaded:\n  {}", changes.join("\n  "))
    };
    SlashOutcome::Continue(Some(summary))
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

/// `/paths` — print where every customization & state file lives on disk.
/// Each line is a real path resolved from the same code that loads the
/// file at startup, so it can't lie. Missing files get a `(not present)`
/// hint, which doubles as a how-to: touch the file to start customizing.
pub struct Paths;

#[async_trait]
impl SlashCommand for Paths {
    fn name(&self) -> &str {
        "paths"
    }
    fn description(&self) -> &str {
        "show on-disk locations for config, plugins, sessions, etc."
    }
    async fn run(&self, _args: &str, agent: &mut Agent) -> SlashOutcome {
        SlashOutcome::Continue(Some(render_paths(agent)))
    }
}

fn render_paths(agent: &Agent) -> String {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);

    let mut out = String::new();

    // Working — the runtime state that frames everything else.
    out.push_str("# Working\n");
    out.push_str(&format!("  cwd:               {}\n", cwd.display()));
    let provider = if agent.provider_name.is_empty() {
        "(unset)"
    } else {
        agent.provider_name.as_str()
    };
    out.push_str(&format!("  active provider:   {}\n", provider));
    out.push_str(&format!("  active model:      {}\n", agent.model));

    // Customization — files the user edits to change behavior.
    out.push_str("\n# Customization\n");
    push_path(
        &mut out,
        "Global config",
        crate::config::default_config_path().as_deref(),
    );
    let project_cfg = cwd.join(".oli").join("config.toml");
    push_path(&mut out, "Project config", Some(&project_cfg));
    let cwd_agents = cwd.join("AGENTS.md");
    let cwd_claude = cwd.join("CLAUDE.md");
    push_path(&mut out, "Project AGENTS.md", Some(&cwd_agents));
    push_path(&mut out, "Project CLAUDE.md", Some(&cwd_claude));
    if let Some(h) = home.as_ref() {
        push_path(
            &mut out,
            "Global CLAUDE.md",
            Some(&h.join(".claude").join("CLAUDE.md")),
        );
        push_path(
            &mut out,
            "Global AGENTS.md",
            Some(&h.join(".codex").join("AGENTS.md")),
        );
    }
    out.push_str("  (oli also walks parent dirs of cwd looking for AGENTS.md/CLAUDE.md)\n");

    // Extensions — Lua plugin discovery dirs.
    out.push_str("\n# Extensions  (drop .lua files into a plugin dir)\n");
    let plugin_dirs = crate::plugins::default_plugin_dirs();
    for (i, p) in plugin_dirs.iter().enumerate() {
        let label = if p.is_absolute() {
            "Global plugins"
        } else {
            "Project plugins"
        };
        // Some installs may resolve the same path twice; key by index so
        // both lines render even if labels collide.
        let _ = i;
        push_dir(&mut out, label, p);
    }

    // State — generated by oli, listed for awareness rather than editing.
    out.push_str("\n# State\n");
    push_dir_opt(
        &mut out,
        "Sessions",
        crate::agent::memory::persisted::sessions_dir().as_deref(),
    );
    push_dir_opt(
        &mut out,
        "Notes",
        crate::notes::filesystem::FilesystemNotesStore::default_dir().as_deref(),
    );
    #[cfg(feature = "tui")]
    push_path(
        &mut out,
        "TUI history",
        crate::tui::history::history_path().as_deref(),
    );
    push_path(
        &mut out,
        "Policy allow-list",
        crate::policy::persisted_allow::default_path().as_deref(),
    );

    out.trim_end().to_string()
}

fn push_path(out: &mut String, label: &str, path: Option<&std::path::Path>) {
    let Some(p) = path else {
        out.push_str(&format!(
            "  {:<18} (no $HOME / $XDG_CONFIG_HOME)\n",
            format!("{}:", label)
        ));
        return;
    };
    let mark = if p.exists() { "" } else { "  (not present)" };
    out.push_str(&format!(
        "  {:<18} {}{}\n",
        format!("{}:", label),
        p.display(),
        mark
    ));
}

fn push_dir(out: &mut String, label: &str, path: &std::path::Path) {
    let mark = if path.is_dir() { "" } else { "  (not present)" };
    out.push_str(&format!(
        "  {:<18} {}/{}\n",
        format!("{}:", label),
        path.display(),
        mark
    ));
}

fn push_dir_opt(out: &mut String, label: &str, path: Option<&std::path::Path>) {
    match path {
        Some(p) => push_dir(out, label, p),
        None => out.push_str(&format!(
            "  {:<18} (no $HOME / $XDG_CONFIG_HOME)\n",
            format!("{}:", label)
        )),
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
    async fn config_reload_picks_up_default_provider_change() {
        // The agent starts pointing at provider "ollama" (model "llama"),
        // then we write a config that switches the default to a fresh
        // openai-compat block with a different model. /config reload
        // should pick that up without losing memory.
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("config.toml");
        std::fs::write(
            &cfg_path,
            r#"
default_provider = "alt"

[providers.alt]
kind          = "openai-compat"
base_url      = "http://example.invalid/v1"
api_key       = "k"
default_model = "alt-model"
"#,
        )
        .unwrap();
        // Layered loader probes XDG_CONFIG_HOME first; point it at our temp.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", dir.path());
        }
        // load_or_default uses XDG_CONFIG_HOME/oli/config.toml — relocate
        // our seed file to that path.
        let real_path = dir.path().join("oli").join("config.toml");
        std::fs::create_dir_all(real_path.parent().unwrap()).unwrap();
        std::fs::rename(&cfg_path, &real_path).unwrap();

        let mut agent = fresh_agent();
        agent.provider_name = "ollama".into();
        agent.model = "llama".into();
        // Stash a memory entry so we can verify it survives.
        agent
            .memory
            .record(json!({"role":"user","content":"keep me"}))
            .await;
        let mem_len_before = agent.memory.len();

        let reg = SlashRegistry::default_set();
        let out = reg.dispatch("config reload", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(body)) => {
                assert!(body.contains("provider:"));
                assert!(body.contains("alt"));
                assert!(body.contains("alt-model"));
            }
            _ => panic!("expected reload summary, got {:?}", out),
        }
        assert_eq!(agent.provider_name, "alt");
        assert_eq!(agent.model, "alt-model");
        // Memory survived.
        assert_eq!(agent.memory.len(), mem_len_before);
    }

    #[tokio::test]
    async fn config_reload_unknown_subcommand_surfaces_help() {
        let mut agent = fresh_agent();
        let reg = SlashRegistry::default_set();
        let out = reg.dispatch("config nope", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(body)) => assert!(body.contains("/config reload")),
            _ => panic!("expected help text, got {:?}", out),
        }
    }

    #[tokio::test]
    async fn diagnostics_renders_recent_entries() {
        // Serialized with the diagnostics::tests cases that
        // share the process-wide ring buffer.
        let _g = crate::diagnostics::TEST_SERIAL.lock().unwrap();
        crate::diagnostics::clear();
        crate::diagnostics::push(
            crate::diagnostics::Level::Warn,
            "[plugins] foo failed to load: oops".into(),
        );
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent();
        let out = reg.dispatch("diagnostics", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(body)) => {
                assert!(body.contains("[warn]"));
                assert!(body.contains("foo failed to load"));
            }
            _ => panic!("expected Continue with body"),
        }
        crate::diagnostics::clear();
    }

    #[tokio::test]
    async fn diagnostics_clear_wipes_the_ring() {
        let _g = crate::diagnostics::TEST_SERIAL.lock().unwrap();
        crate::diagnostics::clear();
        crate::diagnostics::push(crate::diagnostics::Level::Info, "noise".into());
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent();
        let out = reg.dispatch("diagnostics clear", &mut agent).await.unwrap();
        assert!(matches!(out, SlashOutcome::Continue(Some(_))));
        assert!(crate::diagnostics::tail(usize::MAX).is_empty());
    }

    #[tokio::test]
    async fn diagnostics_empty_states_renders_a_note() {
        let _g = crate::diagnostics::TEST_SERIAL.lock().unwrap();
        crate::diagnostics::clear();
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent();
        let out = reg.dispatch("diagnostics", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(body)) => {
                assert!(body.contains("no diagnostics recorded"));
            }
            _ => panic!("expected note about empty buffer"),
        }
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
    async fn cost_reports_session_total_alongside_last_call() {
        // After multiple chat rounds the session total should equal
        // the sum, not just the last round.
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent();
        agent.last_usage = Some(crate::providers::Usage {
            prompt_tokens: 7,
            completion_tokens: 3,
            total_tokens: 10,
        });
        agent.session_usage = crate::providers::Usage {
            prompt_tokens: 22,
            completion_tokens: 9,
            total_tokens: 31,
        };
        let out = reg.dispatch("cost", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(msg)) => {
                // last call line
                assert!(msg.contains("last call:"));
                assert!(msg.contains("7 prompt"));
                assert!(msg.contains("10 tokens"));
                // session line
                assert!(msg.contains("session:"));
                assert!(msg.contains("22 prompt"));
                assert!(msg.contains("31 tokens"));
            }
            _ => panic!("expected Continue(Some(_)), got {:?}", out),
        }
    }

    #[tokio::test]
    async fn cost_session_line_says_no_usage_when_zero() {
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent();
        let out = reg.dispatch("cost", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(msg)) => {
                assert!(msg.contains("session:"));
                assert!(msg.contains("no usage recorded"));
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

    /// End-to-end `/plugins reload`: an empty agent, with a reloader
    /// pointed at a tempdir containing one plugin, picks up the
    /// plugin's tool, hook, and slash command after invoking
    /// `/plugins reload` — without any restart.
    #[tokio::test]
    async fn plugins_reload_picks_up_a_freshly_dropped_plugin() {
        use crate::plugins::PluginReloader;
        use std::sync::Arc;
        use tempfile::tempdir;

        let plugin_dir = tempdir().unwrap();
        std::fs::write(
            plugin_dir.path().join("hello.lua"),
            r#"
local p = { name = "hello", version = "0.1" }
p.tools = {
  { name = "Greet", description = "say hi",
    parameters = { type = "object", properties = {} },
    execute = function(args, ctx) return "hi" end },
}
p.slash_commands = {
  { name = "wave", description = "wave",
    execute = function(args, ctx) return "🌊" end },
}
p.hooks = {
  pre_tool_use = function(event, ctx) end,
}
return p
            "#,
        )
        .unwrap();

        let host_tools = Arc::new(tokio::sync::Mutex::new(Registry::new()));
        let reloader = Arc::new(PluginReloader::with_dirs(
            host_tools,
            None,
            vec![plugin_dir.path().to_path_buf()],
        ));

        let mut agent = fresh_agent();
        // Sanity: nothing plugin-y yet.
        assert!(agent.plugin_manifest.is_empty());
        assert_eq!(agent.tools.iter().count(), 0);
        assert_eq!(agent.hooks.len(), 0);

        let plugins_cmd = Plugins::new(Some(reloader.clone()));
        let outcome = plugins_cmd.run("reload", &mut agent).await;

        match outcome {
            SlashOutcome::Rebuild {
                removed_names,
                added_slashes,
                message,
            } => {
                // First reload: nothing to remove.
                assert!(removed_names.is_empty());
                // The plugin's slash command came back for the REPL
                // to install.
                assert_eq!(added_slashes.len(), 1);
                assert_eq!(added_slashes[0].name(), "wave");
                assert!(
                    message.contains("1 plugin"),
                    "expected reload summary, got: {}",
                    message
                );
            }
            other => panic!("expected Rebuild, got {:?}", other),
        }

        // Tool, hook, and manifest are all live without any restart.
        assert!(agent.tools.get("Greet").is_some());
        assert_eq!(agent.hooks.len(), 1);
        assert_eq!(agent.plugin_manifest.len(), 1);
        assert_eq!(agent.plugin_manifest[0].name, "hello");
        assert_eq!(agent.plugin_manifest[0].tools, vec!["Greet"]);
    }

    /// A second reload after a plugin file has been deleted on disk
    /// removes the prior plugin's contributions cleanly.
    #[tokio::test]
    async fn plugins_reload_removes_entries_when_file_disappears() {
        use crate::plugins::PluginReloader;
        use std::sync::Arc;
        use tempfile::tempdir;

        let plugin_dir = tempdir().unwrap();
        let plugin_path = plugin_dir.path().join("ephemeral.lua");
        std::fs::write(
            &plugin_path,
            r#"
local p = { name = "ephemeral" }
p.tools = {
  { name = "Boop", description = "",
    parameters = { type = "object", properties = {} },
    execute = function(args, ctx) return "boop" end },
}
p.slash_commands = {
  { name = "boop", description = "boop",
    execute = function(args, ctx) return "boop" end },
}
return p
            "#,
        )
        .unwrap();

        let host_tools = Arc::new(tokio::sync::Mutex::new(Registry::new()));
        let reloader = Arc::new(PluginReloader::with_dirs(
            host_tools,
            None,
            vec![plugin_dir.path().to_path_buf()],
        ));
        let plugins_cmd = Plugins::new(Some(reloader.clone()));
        let mut agent = fresh_agent();

        // First reload: plugin loads.
        let _ = plugins_cmd.run("reload", &mut agent).await;
        assert!(agent.tools.get("Boop").is_some());
        assert_eq!(agent.plugin_manifest.len(), 1);

        // Plugin file disappears between sessions.
        std::fs::remove_file(&plugin_path).unwrap();

        // Second reload: tool + slash names come back as `removed_names`,
        // nothing new to register.
        let outcome = plugins_cmd.run("reload", &mut agent).await;
        match outcome {
            SlashOutcome::Rebuild {
                removed_names,
                added_slashes,
                ..
            } => {
                assert!(
                    removed_names.contains(&"boop".to_string()),
                    "expected `boop` in removed_names, got {:?}",
                    removed_names
                );
                assert!(added_slashes.is_empty());
            }
            other => panic!("expected Rebuild, got {:?}", other),
        }
        assert!(agent.tools.get("Boop").is_none(), "Boop should be gone");
        assert!(agent.plugin_manifest.is_empty());
    }

    #[tokio::test]
    async fn plugins_reload_without_reloader_reports_unavailable() {
        let plugins_cmd = Plugins::new(None);
        let mut agent = fresh_agent();
        let outcome = plugins_cmd.run("reload", &mut agent).await;
        match outcome {
            SlashOutcome::Continue(Some(msg)) => {
                assert!(msg.contains("unavailable"));
            }
            other => panic!("expected Continue with unavailable msg, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn plugins_unknown_subcommand_reports_usage() {
        let plugins_cmd = Plugins::new(None);
        let mut agent = fresh_agent();
        let outcome = plugins_cmd.run("frobnicate", &mut agent).await;
        match outcome {
            SlashOutcome::Continue(Some(msg)) => {
                assert!(msg.contains("unknown /plugins subcommand"));
                assert!(msg.contains("reload"));
            }
            other => panic!("expected Continue, got {:?}", other),
        }
    }

    #[test]
    fn paths_is_registered_in_default_set() {
        let reg = SlashRegistry::default_set();
        assert!(
            reg.get("paths").is_some(),
            "default set should include /paths"
        );
    }

    #[tokio::test]
    async fn paths_renders_section_headers_and_active_model() {
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent();
        agent.provider_name = "ollama".into();

        let out = reg.dispatch("paths", &mut agent).await.unwrap();
        let body = match out {
            SlashOutcome::Continue(Some(msg)) => msg,
            other => panic!("expected Continue, got {:?}", other),
        };

        // Section headers anchor the layout — keep them stable so the
        // model can quote them when explaining customization to users.
        for header in &["# Working", "# Customization", "# Extensions", "# State"] {
            assert!(
                body.contains(header),
                "missing section header `{}` in:\n{}",
                header,
                body
            );
        }
        // Runtime fields surface so the user sees what the rest is
        // relative to.
        assert!(body.contains("active provider:"));
        assert!(body.contains("ollama"));
        assert!(body.contains("active model:"));
        // Customization filenames the docs reference.
        for filename in &["config.toml", "AGENTS.md", "CLAUDE.md"] {
            assert!(
                body.contains(filename),
                "missing customization filename `{}` in:\n{}",
                filename,
                body
            );
        }
    }

    #[tokio::test]
    async fn paths_marks_definitely_missing_files_as_not_present() {
        // The TUI history file is `~/.config/oli/tui-history.jsonl`. We
        // can't pre-create it without polluting the user's real config,
        // and we can't sandbox std::env::current_dir() either, but we
        // *can* assert that any line whose path doesn't exist on disk
        // gets the marker. Walk every line and check the invariant.
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent();
        let out = reg.dispatch("paths", &mut agent).await.unwrap();
        let body = match out {
            SlashOutcome::Continue(Some(msg)) => msg,
            other => panic!("expected Continue, got {:?}", other),
        };

        for line in body.lines() {
            // Section headers and the walk-up note don't carry paths.
            if !line.starts_with("  ") || line.trim_start().starts_with('(') {
                continue;
            }
            // Lines have shape `  Label:           /abs/path[/]  (not present)?`
            // — split on whitespace and grab the path token (last
            // non-marker word). If the path doesn't exist on disk,
            // the line MUST end with the marker.
            let trimmed = line.trim_end();
            let already_marked = trimmed.ends_with("(not present)");
            // Heuristic: extract the substring after the label colon.
            let Some(colon) = line.find(':') else {
                continue;
            };
            let rest = line[colon + 1..].trim();
            // Skip the env-missing fallback message.
            if rest.starts_with('(') {
                continue;
            }
            let path_str = rest
                .strip_suffix("(not present)")
                .unwrap_or(rest)
                .trim()
                .trim_end_matches('/');
            // Only filesystem paths get the marker. The Working
            // section's `active provider:` / `active model:` rows
            // hold short identifiers, not paths — skip them.
            if !path_str.starts_with('/') {
                continue;
            }
            let p = std::path::Path::new(path_str);
            if !p.exists() {
                assert!(
                    already_marked,
                    "non-existent path lacks `(not present)` marker:\n  line: {}",
                    line
                );
            }
        }
    }
}
