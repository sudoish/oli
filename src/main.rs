mod agent;
mod config;
mod error;
mod providers;
mod repl;
mod tools;

use clap::Parser;
use std::process;

use crate::agent::Agent;
use crate::agent::context::SystemPromptBuilder;
use crate::config::Config;
use crate::error::{AgentError, Result};
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
    let cfg = Config::load_or_default()?;
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

    let system_prompt = SystemPromptBuilder::from_env().build().await;
    let mut agent = Agent::new(provider, tools, model).with_system_prompt(system_prompt);

    match args.prompt {
        Some(p) => {
            // One-shot: keep the existing scripted-friendly behavior — print
            // the final assistant content once, no streaming.
            let output = agent.run(&p).await?;
            if !output.is_empty() {
                println!("{}", output);
            }
            Ok(())
        }
        None => repl::run(agent).await,
    }
}
