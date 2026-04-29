mod agent;
mod config;
mod error;
mod hooks;
mod notes;
mod plugins;
mod policy;
mod providers;
mod repl;
mod tools;

use async_trait::async_trait;
use clap::Parser;
use std::process;
use std::sync::Arc;

use crate::agent::Agent;
use crate::agent::context::SystemPromptBuilder;
use crate::agent::memory::{
    LinearWithCompact, Memory, PersistedMemory, list_sessions, new_session_id,
};
use crate::config::Config;
use crate::error::{AgentError, Result};
use crate::policy::{ConfigPolicy, ReadlineApprover};
use crate::providers::Provider as ProviderTrait;
use crate::providers::anthropic::AnthropicProvider;
use crate::providers::openai_compat::OpenAICompatProvider;
use crate::tools::task::{SubagentSpawner, Task};
use crate::tools::{
    Registry, bash::Bash, edit::Edit, glob::Glob, grep::Grep, read::Read, write::Write,
};

#[derive(Parser)]
#[command(author, version, about)]
struct Args {
    /// Single-shot prompt. If omitted, the binary enters an interactive REPL.
    #[arg(short = 'p', long)]
    prompt: Option<String>,

    /// Resume a specific session by id (file stem in
    /// `~/.config/agent/sessions/`). Conflicts with `--continue`.
    #[arg(long, conflicts_with = "continue_session")]
    resume: Option<String>,

    /// Resume the most recent session by mtime. Conflicts with `--resume`.
    #[arg(long = "continue", conflicts_with = "resume")]
    continue_session: bool,

    /// Override the per-run turn cap. Falls back to `[agent].max_turns`
    /// in config (default 40). Useful when one specific task genuinely
    /// needs to go further than the conservative default.
    #[arg(long)]
    max_turns: Option<usize>,
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
    let api_key = cfg.resolve_api_key(&provider_name)?;
    let model = cfg.model_for(&provider_name)?;

    let provider: Box<dyn ProviderTrait> = match pcfg.kind.as_str() {
        "openai-compat" => Box::new(OpenAICompatProvider::new(pcfg.base_url.clone(), api_key)),
        "anthropic" => Box::new(AnthropicProvider::new(pcfg.base_url.clone(), api_key)),
        other => {
            return Err(AgentError::Config(format!(
                "unsupported provider kind '{}' for '{}' (try 'openai-compat' or 'anthropic')",
                other, provider_name
            )));
        }
    };

    // Long-term notes store. Used by WriteNote/SearchNotes/ListNotes
    // tools. Single instance shared across parent + subagents so notes
    // written by one are visible to the others.
    let notes_store: Arc<dyn notes::NotesStore> = Arc::new(notes::FilesystemNotesStore::at(
        notes::FilesystemNotesStore::default_dir().unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."))
                .join(".agent")
                .join("notes")
        }),
    ));

    let mut tools = build_default_tools(&cfg, notes_store.clone());
    // The Task subagent tool spawns a fresh agent per call. We register
    // it only on the parent — a child built by the spawner gets the
    // baseline tool set without `Task`, preventing infinite recursion
    // through nested subagents.
    let spawner: Arc<dyn SubagentSpawner> = Arc::new(AgentSpawner {
        cfg: cfg.clone(),
        provider_name: provider_name.clone(),
        notes_store: notes_store.clone(),
    });
    tools.register(Task::new(spawner.clone()));

    // Load Lua plugins. Plugin tools see a snapshot of the built-in
    // tools (no `Task`, no other plugins) so plugins can call into
    // the harness via `ctx:tool` but can't recurse through each other.
    let plugin_host_tools = Arc::new(tokio::sync::Mutex::new(build_default_tools(
        &cfg,
        notes_store.clone(),
    )));
    let plugins = plugins::load_all(plugin_host_tools, Some(spawner)).await;
    let plugin_manifest = plugins.manifest;
    for t in plugins.tools {
        tools.register_box(t);
    }
    // Plugin-registered slash commands and hooks land in the agent
    // alongside the built-in ones. We thread them through builders
    // below.
    let plugin_slashes = plugins.slash_commands;
    let plugin_hooks = plugins.hooks;

    let system_prompt = SystemPromptBuilder::from_env().build().await;
    let policy = Box::new(ConfigPolicy::from_config(&cfg.policy));

    let interactive = args.prompt.is_none();
    let session_id = resolve_session_id(&args, interactive)?;
    let memory = build_memory(session_id.as_deref()).await?;

    let mut hooks = crate::hooks::HookRegistry::new();
    for h in plugin_hooks {
        hooks.register_box(h);
    }

    let max_turns = args.max_turns.unwrap_or(cfg.agent.max_turns);

    let agent_base = Agent::new(provider, tools, model)
        .with_policy(policy)
        .with_config(cfg.clone(), provider_name)
        .with_memory(memory)
        .with_hooks(hooks)
        .with_plugin_manifest(plugin_manifest)
        .with_max_turns(max_turns);

    match args.prompt {
        Some(p) => {
            // One-shot: scripted-friendly. Policy still gates which tools
            // run, but `Ask` decisions auto-approve (default `AlwaysApprove`)
            // so the model isn't blocked by an interactive prompt no one
            // can answer.
            let mut agent = agent_base.pin_system_prompt(system_prompt).await;
            let output = agent.run(&p).await?;
            if !output.is_empty() {
                println!("{}", output);
            }
            Ok(())
        }
        None => {
            // Interactive: prompt the user via stdin for any `Ask`.
            if let Some(id) = &session_id {
                println!("session: {}", id);
            }
            let agent = agent_base
                .with_approver(Box::new(ReadlineApprover))
                .pin_system_prompt(system_prompt)
                .await;
            repl::run(agent, plugin_slashes).await
        }
    }
}

/// Decide which session id to use for this run. Precedence: explicit
/// `--resume`, then `--continue` (latest by mtime), then a fresh id for
/// REPL mode, then `None` for ephemeral one-shot mode (`-p` without
/// either flag).
fn resolve_session_id(args: &Args, interactive: bool) -> Result<Option<String>> {
    if let Some(id) = &args.resume {
        return Ok(Some(id.clone()));
    }
    if args.continue_session {
        let entries = list_sessions();
        let latest = entries
            .into_iter()
            .next()
            .ok_or_else(|| AgentError::Config("no prior sessions to continue".into()))?;
        return Ok(Some(latest.id));
    }
    if interactive {
        return Ok(Some(new_session_id()));
    }
    Ok(None)
}

/// Build the agent's memory. With a session id we wrap the linear
/// default in `PersistedMemory`, replaying any existing transcript.
/// Without one (`-p` ephemeral mode), the linear memory stands alone.
async fn build_memory(session_id: Option<&str>) -> Result<Box<dyn Memory>> {
    let inner: Box<dyn Memory> = Box::new(LinearWithCompact::new());
    match session_id {
        Some(id) => {
            let persisted = PersistedMemory::open(id, inner).await?;
            Ok(Box::new(persisted))
        }
        None => Ok(inner),
    }
}

/// Built-in tool set shared between the parent agent and any subagent
/// spawned via `Task`. Excludes `Task` itself so subagents can't recurse
/// (the parent registers `Task` separately on top of this). Includes
/// the notes tools backed by the supplied `NotesStore`.
fn build_default_tools(cfg: &Config, notes_store: Arc<dyn notes::NotesStore>) -> Registry {
    let mut tools = Registry::new();
    tools.register(Read);
    tools.register(Write);
    tools.register(Edit);
    tools.register(Bash);
    tools.register(Grep);
    tools.register(Glob);
    tools.register(crate::tools::notes::WriteNote::new(notes_store.clone()));
    tools.register(crate::tools::notes::SearchNotes::new(notes_store.clone()));
    tools.register(crate::tools::notes::ListNotes::new(notes_store));
    for sub in &cfg.tools.subprocess {
        tools.register(crate::tools::subprocess::SubprocessTool::from_config(sub));
    }
    tools
}

/// `SubagentSpawner` impl that builds a fresh agent from config on each
/// call. Each subagent has its own LinearWithCompact memory (no
/// persistence — children are ephemeral by design) and inherits the
/// parent's policy + capability registry. The result is the child's
/// final assistant message; intermediate tool steps stay in the
/// child's memory and are discarded when it returns.
struct AgentSpawner {
    cfg: Arc<Config>,
    provider_name: String,
    notes_store: Arc<dyn notes::NotesStore>,
}

#[async_trait]
impl SubagentSpawner for AgentSpawner {
    async fn spawn(&self, prompt: &str, max_turns: usize) -> Result<String> {
        let pcfg = self.cfg.provider(&self.provider_name)?;
        let api_key = self.cfg.resolve_api_key(&self.provider_name)?;
        let model = self.cfg.model_for(&self.provider_name)?;
        let provider: Box<dyn ProviderTrait> = match pcfg.kind.as_str() {
            "openai-compat" => Box::new(OpenAICompatProvider::new(pcfg.base_url.clone(), api_key)),
            "anthropic" => Box::new(AnthropicProvider::new(pcfg.base_url.clone(), api_key)),
            other => {
                return Err(AgentError::Config(format!(
                    "subagent spawn: unsupported provider kind '{}'",
                    other
                )));
            }
        };
        let tools = build_default_tools(&self.cfg, self.notes_store.clone());
        let policy = Box::new(ConfigPolicy::from_config(&self.cfg.policy));

        let mut agent = Agent::new(provider, tools, model)
            .with_policy(policy)
            .with_config(self.cfg.clone(), &self.provider_name)
            .with_max_turns(max_turns);
        agent.run(prompt).await
    }
}
