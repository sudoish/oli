use super::{SlashCommand, SlashOutcome};
use crate::agent::Agent;
use async_trait::async_trait;

/// `/config reload` — re-read the config file (global +
/// project-local) and apply changes to the running agent
/// without restarting. Memory, transcript, system prompt,
/// session token totals — all survive. The active provider gets
/// rebuilt only if its `default_provider` or its provider-block
/// config changed; the same is true for the model id.
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
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    reload_config_at(agent, &cwd).await
}

pub(super) async fn reload_config_at(agent: &mut Agent, cwd: &std::path::Path) -> SlashOutcome {
    let new_cfg = match crate::config::Config::load_layered(cwd) {
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
    agent.cfg = Some(new_cfg);
    // last_usage doesn't survive a swap — the prior usage was
    // measured against a different model/provider.
    agent.last_usage = None;
    agent.refresh_ledger_accounting();

    let summary = if changes.is_empty() {
        "config reloaded (no provider/model change)".to_string()
    } else {
        format!("config reloaded:\n  {}", changes.join("\n  "))
    };
    SlashOutcome::Continue(Some(summary))
}
