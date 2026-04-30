//! Binary entry point for `oli`. Parses CLI args, builds the
//! agent + tool registry from config, and dispatches to the TUI,
//! the line-mode REPL, or one-shot prompt mode. Reaches into the
//! library at `oli::*` for everything substantive — this file
//! is intentionally a wiring shim, not where logic lives.

use clap::Parser;
use std::process;
use std::sync::Arc;

use oli::agent::Agent;
use oli::agent::context::SystemPromptBuilder;
use oli::bootstrap::{
    DefaultAgentSpawner, build_default_tools, build_memory, resolve_session_id,
};
use oli::config::Config;
use oli::error::Result;
use oli::policy::{AlwaysDeny, ConfigPolicy, ReadlineApprover};
use oli::providers::Provider as ProviderTrait;
use oli::tools::task::{SubagentSpawner, Task};
use oli::{hooks, mcp, notes, plugins, providers, repl, tui};

#[derive(Parser)]
#[command(author, version, about)]
struct Args {
    /// Single-shot prompt. If omitted, the binary enters an interactive REPL.
    #[arg(short = 'p', long)]
    prompt: Option<String>,

    /// Resume a specific session by id (file stem in
    /// `~/.config/oli/sessions/`). Conflicts with `--continue`.
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

    /// Strict mode: in `-p` runs, deny every `Ask` policy decision
    /// instead of auto-approving. Use for unattended scripted tasks
    /// that should not silently rubber-stamp Edit/Write/unknown-Bash
    /// calls. Has no effect on the interactive REPL (which already
    /// prompts).
    #[arg(long)]
    strict: bool,

    /// Disable the TUI and fall back to the line-mode rustyline REPL.
    /// Auto-enabled when stdin or stdout isn't a terminal (so
    /// piped usage still works without setting the flag). Useful
    /// inside SSH sessions on minimal terminals or when
    /// terminal-native scrollback / mouse selection matter.
    #[arg(long)]
    plain: bool,
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
    let model = cfg.model_for(&provider_name)?;

    let provider: Box<dyn ProviderTrait> = providers::build(&cfg, &provider_name)?;

    // Long-term notes store. Used by WriteNote/SearchNotes/ListNotes
    // tools. Single instance shared across parent + subagents so notes
    // written by one are visible to the others.
    let notes_store: Arc<dyn notes::NotesStore> = Arc::new(notes::FilesystemNotesStore::at(
        notes::FilesystemNotesStore::default_dir().unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."))
                .join(".oli")
                .join("notes")
        }),
    ));

    let mut tools = build_default_tools(&cfg, notes_store.clone());

    // MCP servers: dial up everything in `[mcp.servers]` in parallel,
    // register healthy servers' tools alongside the built-ins. Failed
    // servers are kept in `mcp_handles` (Down) so `/mcp` can show why
    // they're missing.
    let mcp_handles = Arc::new(mcp::connect_all(&cfg.mcp).await);
    for tool in mcp::build_tools(&mcp_handles).await {
        tools.register_box(tool);
    }

    // The Task subagent tool spawns a fresh agent per call. We register
    // it only on the parent — a child built by the spawner gets the
    // baseline tool set without `Task`, preventing infinite recursion
    // through nested subagents.
    let spawner: Arc<dyn SubagentSpawner> = Arc::new(DefaultAgentSpawner {
        cfg: cfg.clone(),
        provider_name: provider_name.clone(),
        notes_store: notes_store.clone(),
        mcp_handles: mcp_handles.clone(),
    });
    tools.register(Task::new(spawner.clone()));

    // Load Lua plugins. Plugin tools see a snapshot of the built-in
    // tools (no `Task`, no other plugins) so plugins can call into
    // the harness via `ctx:tool` but can't recurse through each other.
    let plugin_host_tools = Arc::new(tokio::sync::Mutex::new(build_default_tools(
        &cfg,
        notes_store.clone(),
    )));
    // Reloader keeps a handle on the same host args so `/plugins reload`
    // can re-scan the plugin dirs at any time without re-plumbing
    // startup wiring. Only useful in interactive mode.
    let plugin_reloader = Arc::new(plugins::PluginReloader::new(
        plugin_host_tools.clone(),
        Some(spawner.clone()),
    ));
    let plugins = plugins::load_all(plugin_host_tools, Some(spawner)).await;
    let plugin_manifest = plugins.manifest;
    for t in plugins.tools {
        tools.register_box(t);
    }
    // Plugin-registered slash commands and hooks land in the agent
    // alongside the built-in ones. We thread them through builders
    // below. The /mcp command rides the same channel — it's not a
    // plugin, but the REPL's `extra slashes` bag is the right shape
    // for "extra commands constructed at startup with their own state."
    let mut plugin_slashes = plugins.slash_commands;
    plugin_slashes.push(Box::new(repl::slash::Mcp::new(mcp_handles.clone())));
    let plugin_hooks = plugins.hooks;

    let system_prompt = SystemPromptBuilder::from_env().build().await;
    let policy = Box::new(ConfigPolicy::from_config(&cfg.policy));

    let interactive = args.prompt.is_none();
    let session_id =
        resolve_session_id(args.resume.as_deref(), args.continue_session, interactive)?;
    let (memory, replayed_reads, read_logger) = build_memory(session_id.as_deref()).await?;

    let mut hooks = hooks::HookRegistry::new();
    for h in plugin_hooks {
        hooks.register_box(h);
    }
    if interactive {
        // Surface tool calls live so the user sees `→ Read(file=…)`
        // before each call. Stays out of the scripted `-p` path so
        // automation log scrapers don't have to filter it.
        hooks.register(repl::ProgressHook);
    }

    let max_turns = args.max_turns.unwrap_or(cfg.agent.max_turns);

    let agent_base = Agent::new(provider, tools, model)
        .with_policy(policy)
        .with_config(cfg.clone(), provider_name)
        .with_memory(memory)
        .with_hooks(hooks)
        .with_plugin_manifest(plugin_manifest)
        .with_mcp_handles(mcp_handles.clone())
        .with_max_turns(max_turns);

    // Wire up read-set persistence: drained replay paths repopulate the
    // `Edit`-required read-set from prior sessions; the logger forwards
    // future `mark_read` calls into the JSONL transcript.
    {
        let ctx = agent_base.tool_context();
        for p in replayed_reads {
            ctx.insert_canonical_read(p).await;
        }
        if let Some(logger) = read_logger {
            ctx.set_read_logger(logger).await;
        }
    }

    match args.prompt {
        Some(p) => {
            // One-shot: scripted-friendly. Policy still gates which tools
            // run; `--strict` flips `Ask` decisions from auto-approve
            // to deny, suitable for unattended runs that must not
            // rubber-stamp Edit/Write/unknown-Bash without supervision.
            let mut agent = if args.strict {
                agent_base.with_approver(Box::new(AlwaysDeny))
            } else {
                agent_base
            }
            .pin_system_prompt(system_prompt)
            .await;
            let output = agent.run(&p).await?;
            if !output.is_empty() {
                println!("{}", output);
            }
            Ok(())
        }
        None => {
            // Interactive: pick TUI by default, fall back to the
            // line-mode REPL on `--plain` or when stdin/stdout
            // isn't a TTY (piped invocations).
            let tty = std::io::IsTerminal::is_terminal(&std::io::stdin())
                && std::io::IsTerminal::is_terminal(&std::io::stdout());
            let use_tui = !args.plain && tty;
            if let Some(id) = &session_id {
                if !use_tui {
                    println!("session: {}", id);
                }
            }
            let agent = agent_base
                .with_approver(Box::new(ReadlineApprover))
                .pin_system_prompt(system_prompt)
                .await;
            if use_tui {
                tui::run(agent, plugin_slashes, Some(plugin_reloader), session_id).await
            } else {
                repl::run(agent, plugin_slashes, Some(plugin_reloader)).await
            }
        }
    }
}
