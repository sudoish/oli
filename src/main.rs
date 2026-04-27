mod agent;
mod config;
mod error;
mod providers;
mod tools;

use clap::Parser;
use std::process;

use crate::agent::Agent;
use crate::config::Config;
use crate::error::{AgentError, Result};
use crate::providers::openai_compat::OpenAICompatProvider;
use crate::tools::{
    Registry, bash::Bash, edit::Edit, glob::Glob, grep::Grep, read::Read, write::Write,
};

#[derive(Parser)]
#[command(author, version, about)]
struct Args {
    #[arg(short = 'p', long)]
    prompt: String,
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
    let cfg = Config::load_or_default()?;
    let provider_name = cfg.default_provider.clone();
    let pcfg = cfg.provider(&provider_name)?;
    if pcfg.kind != "openai-compat" {
        return Err(AgentError::Config(format!(
            "unsupported provider kind '{}' for '{}' (Phase 0 supports 'openai-compat' only)",
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

    let agent = Agent::new(provider, tools, model);
    let output = agent.run(&args.prompt).await?;
    if !output.is_empty() {
        println!("{}", output);
    }
    Ok(())
}
