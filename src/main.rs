mod agent;
mod config;
mod error;
mod policy;
mod providers;
mod repl;
mod tools;

use clap::Parser;
use std::process;

use crate::agent::Agent;
use crate::agent::context::SystemPromptBuilder;
use crate::config::Config;
use crate::error::{AgentError, Result};
use crate::policy::{ConfigPolicy, ReadlineApprover};
use crate::providers::openai_compat::OpenAICompatProvider;
use crate::tools::{
    Registry, bash::Bash, edit::Edit, glob::Glob, grep::Grep, read::Read, write::Write,
};

#[derive(Parser)]
#[command(author, version, about)]
struct Args {
    /// Single-shot prompt. If omitted, the binary enters an interactive REPL.
    #[arg(short = 'p', long)]
    prompt: Option<String>,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    if let Err(e) = run(args).await {
        eprintln!("{}", e);
        process::exit(1);
    }
}

async fn run(args: Args) -> Result<()> {
    let cfg = std::sync::Arc::new(Config::load_or_default()?);
    let provider_name = cfg.default_provider.clone();
    let pcfg = cfg.provider(&provider_name)?;
    if pcfg.kind != "openai-compat" {
        return Err(AgentError::Config(format!(
            "unsupported provider kind '{}' for '{}' (Phase 1 supports 'openai-compat' only)",
            pcfg.kind, provider_name
        )));
    }
    let api_key = cfg.resolve_api_key(&provider_name)?;
    let model = cfg.model_for(&provider_name)?;

    let provider = Box::new(OpenAICompatProvider::new(pcfg.base_url.clone(), api_key));

    let mut tools = Registry::new();
    tools.register(Read);
    tools.register(Write);
    tools.register(Edit);
    tools.register(Bash);
    tools.register(Grep);
    tools.register(Glob);
    for sub in &cfg.tools.subprocess {
        tools.register(crate::tools::subprocess::SubprocessTool::from_config(sub));
    }

    let system_prompt = SystemPromptBuilder::from_env().build().await;
    let policy = Box::new(ConfigPolicy::from_config(&cfg.policy));

    let agent_base = Agent::new(provider, tools, model)
        .with_policy(policy)
        .with_config(cfg.clone(), provider_name);

    match args.prompt {
        Some(p) => {
            // One-shot: scripted-friendly. Policy still gates which tools
            // run, but `Ask` decisions auto-approve (default `AlwaysApprove`)
            // so the model isn't blocked by an interactive prompt no one
            // can answer. Users who want a stricter scripted run can swap
            // an `AlwaysDeny` approver in once the config flag for it
            // exists (Phase 2 follow-up).
            let mut agent = agent_base.pin_system_prompt(system_prompt).await;
            let output = agent.run(&p).await?;
            if !output.is_empty() {
                println!("{}", output);
            }
            Ok(())
        }
        None => {
            // Interactive: prompt the user via stdin for any `Ask`.
            let agent = agent_base
                .with_approver(Box::new(ReadlineApprover))
                .pin_system_prompt(system_prompt)
                .await;
            repl::run(agent).await
        }
    }
}
