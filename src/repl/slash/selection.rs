use super::{SlashCommand, SlashOutcome};
use crate::agent::Agent;
use async_trait::async_trait;

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
                    name,
                    pcfg.kind,
                    pcfg.resolved_base_url(name)
                        .unwrap_or_else(|_| "<unset>".to_string()),
                    active
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
        agent.refresh_ledger_accounting();

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
        agent.caps = agent.resolve_caps(arg);
        agent.last_usage = None;
        agent.refresh_ledger_accounting();
        SlashOutcome::Continue(Some(format!("model switched to {}", arg)))
    }
}
