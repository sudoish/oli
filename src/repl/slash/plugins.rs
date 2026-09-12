use super::{SlashCommand, SlashOutcome};
use crate::agent::Agent;
use async_trait::async_trait;

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
