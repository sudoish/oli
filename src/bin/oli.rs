//! Binary entry point for `oli`. Parses CLI args, builds the
//! agent + tool registry from config, and dispatches to the TUI,
//! the line-mode REPL, or one-shot prompt mode. Reaches into the
//! library at `oli::*` for everything substantive — this file
//! is intentionally a wiring shim, not where logic lives.

use clap::{Parser, Subcommand};
use std::process;
use std::sync::Arc;

use oli::agent::Agent;
use oli::agent::context::SystemPromptBuilder;
use oli::bootstrap::{DefaultAgentSpawner, build_default_tools, build_memory, resolve_session_id};
use oli::config::Config;
use oli::error::Result;
use oli::policy::{AlwaysDeny, ConfigPolicy, ReadlineApprover};
use oli::providers::Provider as ProviderTrait;
use oli::tools::task::{SubagentSpawner, Task};
#[cfg(feature = "tui")]
use oli::tui;
use oli::{hooks, mcp, notes, plugins, providers, repl};

#[derive(Parser)]
#[command(
    author,
    version,
    about,
    long_about = "A minimal, hackable, single-binary terminal coding agent.\n\n\
                  Keyboard cheatsheet: docs/cheatsheet.md (in the repo) — \
                  every shortcut, slash command, file path, and feature flag in one place."
)]
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

    /// Render the TUI inline in the host buffer (no alt-screen,
    /// no mouse capture by default). Recommended inside Neovim
    /// `:terminal`, VSCode's integrated terminal, and similar
    /// buffer-terminals. Overrides `[ui].viewport` from config.
    /// Conflicts with `--fullscreen`.
    #[arg(long, conflicts_with = "fullscreen")]
    inline: bool,

    /// Force the TUI into alternate-screen / fullscreen mode even
    /// when capability detection or `[ui].viewport` would have
    /// picked inline. Conflicts with `--inline`.
    #[arg(long, conflicts_with = "inline")]
    fullscreen: bool,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Write a starter `~/.config/oli/config.toml`. Mirrors the
    /// TUI's first-run wizard but works headlessly — useful in
    /// Dockerfiles, CI, or any setup where you can't pop a TUI.
    Init {
        /// Provider template: ollama (local), openrouter, or
        /// anthropic. Without this flag, prompts on stdin.
        #[arg(long)]
        provider: Option<String>,

        /// API key for paid providers. Without this flag and on
        /// a paid provider, prompts on stdin (echoing input —
        /// pipe in or use --provider ollama if that matters).
        #[arg(long)]
        api_key: Option<String>,

        /// Overwrite an existing config file instead of refusing.
        #[arg(long)]
        force: bool,

        /// Skip the Ollama daemon probe + model-pull offer (only
        /// meaningful with `--provider ollama`). Useful in CI /
        /// container builds where the daemon isn't reachable
        /// from the build step but will be at runtime.
        #[arg(long)]
        skip_ollama_check: bool,

        /// Auto-pull the chosen Ollama model if it isn't already
        /// present. Without this flag, the command prints a
        /// suggested `ollama pull` and exits without downloading.
        #[arg(long)]
        pull: bool,
    },

    /// Sign in with a ChatGPT Plus/Pro subscription instead of an
    /// OpenAI API key. Stores tokens in `~/.config/oli/auth.json`
    /// (mode 0600) for use by a `kind = "openai-chatgpt"` provider.
    ///
    /// API-key auth is unaffected and remains the default — this is
    /// an addition, not a replacement.
    Login {
        /// Print the sign-in URL instead of launching a browser.
        /// Implied on a Linux session with no display.
        #[arg(long)]
        no_browser: bool,
    },

    /// Discard stored ChatGPT subscription credentials.
    Logout,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let result = match args.cmd {
        Some(Cmd::Init {
            provider,
            api_key,
            force,
            skip_ollama_check,
            pull,
        }) => init_command(provider, api_key, force, skip_ollama_check, pull).await,
        Some(Cmd::Login { no_browser }) => login_command(no_browser).await,
        Some(Cmd::Logout) => logout_command(),
        None => run(args).await,
    };
    if let Err(e) = result {
        eprintln!("{}", e);
        process::exit(1);
    }
}

/// `oli login`. Runs the ChatGPT subscription OAuth flow and stores
/// the resulting tokens. Does not touch `config.toml` — pointing a
/// provider at these credentials is a separate, explicit step, so
/// logging in can never silently change how an existing config
/// authenticates.
async fn login_command(no_browser: bool) -> Result<()> {
    use oli::auth::login::{LoginOptions, run as run_login};
    use oli::auth::store::AuthStore;

    let store = AuthStore::default_location()?;
    let opts = LoginOptions {
        open_browser: !no_browser,
        ..LoginOptions::default()
    };
    run_login(&opts, &store).await?;

    println!(
        "\nTo use it, add a provider to your config:\n\n\
         \x20 [providers.chatgpt]\n\
         \x20 kind = \"openai-chatgpt\"\n\
         \x20 base_url = \"https://chatgpt.com/backend-api/codex\"\n"
    );
    Ok(())
}

/// `oli logout`. Removes stored subscription credentials. Leaves
/// config alone; a provider still pointed at `openai-chatgpt` will
/// fail loudly and tell the user to log in again.
fn logout_command() -> Result<()> {
    use oli::auth::store::AuthStore;

    let store = AuthStore::default_location()?;
    let existed = store.exists();
    store.clear()?;
    if existed {
        println!("Removed {}.", store.path().display());
    } else {
        println!(
            "No stored ChatGPT credentials at {}.",
            store.path().display()
        );
    }
    Ok(())
}

/// Headless `oli init`. Writes the same config the TUI wizard
/// produces, falling back to stdin prompts for fields not given
/// on the CLI. Refuses to clobber an existing file unless
/// `--force` is set. For Ollama, also probes the local daemon
/// and (with `--pull`) downloads the default model.
async fn init_command(
    provider: Option<String>,
    api_key: Option<String>,
    force: bool,
    skip_ollama_check: bool,
    pull: bool,
) -> Result<()> {
    use oli::wizard_init::{WizardProvider, config_path, render_toml, save};
    use std::io::Write;

    let path = config_path().ok_or_else(|| {
        oli::error::AgentError::Config(
            "could not resolve config path (no $HOME or $XDG_CONFIG_HOME)".into(),
        )
    })?;

    let provider = match provider.as_deref() {
        Some(name) => WizardProvider::from_name(name).ok_or_else(|| {
            oli::error::AgentError::Config(format!(
                "unknown --provider `{}` (try ollama, openrouter, anthropic)",
                name
            ))
        })?,
        None => prompt_provider()?,
    };

    let api_key = if provider.needs_api_key() {
        match api_key {
            Some(k) if !k.trim().is_empty() => k,
            _ => prompt_api_key(provider)?,
        }
    } else {
        String::new()
    };

    let body = render_toml(provider, &api_key);
    save(&path, &body, force).map_err(oli::error::AgentError::Config)?;

    let mut stdout = std::io::stdout();
    let _ = writeln!(stdout, "wrote {}", path.display());
    let _ = writeln!(stdout, "  provider:      {}", provider.label());
    let _ = writeln!(stdout, "  default_model: {}", provider.default_model());
    if !provider.needs_api_key() {
        let _ = writeln!(stdout, "  (Ollama: api_key field is a placeholder)");
    }

    if matches!(provider, WizardProvider::Ollama) && !skip_ollama_check {
        ollama_post_init(provider, pull).await?;
    }

    let _ = writeln!(stdout, "Run `oli` to start.");
    Ok(())
}

async fn ollama_post_init(
    provider: oli::wizard_init::WizardProvider,
    auto_pull: bool,
) -> Result<()> {
    use oli::wizard_init::{OllamaProbe, PullEvent, has_pulled_model, probe_ollama, pull_model};
    use std::io::Write;
    use std::time::Duration;

    let mut stdout = std::io::stdout();
    let model = provider.default_model();
    let base_url = provider.base_url();

    let _ = writeln!(stdout, "Checking Ollama at {} ...", base_url);
    let _ = stdout.flush();
    let probe = probe_ollama(base_url, Duration::from_secs(2)).await;
    match &probe {
        OllamaProbe::Down { reason } => {
            let _ = writeln!(stdout, "  ⚠ Ollama not reachable: {}", reason);
            let _ = writeln!(
                stdout,
                "    install:  https://ollama.com/download   (then `ollama serve`)"
            );
            let _ = writeln!(stdout, "    once running:  ollama pull {}", model);
            return Ok(());
        }
        OllamaProbe::Up { models } => {
            let _ = writeln!(
                stdout,
                "  ✓ daemon reachable ({} model{} installed)",
                models.len(),
                if models.len() == 1 { "" } else { "s" }
            );
        }
    }

    if has_pulled_model(&probe, model) {
        let _ = writeln!(stdout, "  ✓ {} already pulled", model);
        return Ok(());
    }

    if !auto_pull {
        let _ = writeln!(
            stdout,
            "  ⚠ {} is not pulled. Run `oli init --provider ollama --pull --force`",
            model
        );
        let _ = writeln!(stdout, "    or  `ollama pull {}` to download it.", model);
        return Ok(());
    }

    let _ = writeln!(
        stdout,
        "Pulling {} (this can take a few minutes) ...",
        model
    );
    let _ = stdout.flush();
    let mut last_pct: i32 = -1;
    pull_model(base_url, model, |ev| match ev {
        PullEvent::Phase(p) => {
            let _ = writeln!(std::io::stdout(), "  · {}", p);
        }
        PullEvent::Progress {
            phase,
            completed,
            total,
        } => {
            let pct = ((completed as f64 / total as f64) * 100.0) as i32;
            if pct != last_pct {
                last_pct = pct;
                let _ = writeln!(
                    std::io::stdout(),
                    "  · {} {}% ({} / {})",
                    phase,
                    pct,
                    human_bytes(completed),
                    human_bytes(total)
                );
            }
        }
        PullEvent::Done => {
            let _ = writeln!(std::io::stdout(), "  ✓ pulled {}", model);
        }
        PullEvent::Error(e) => {
            let _ = writeln!(std::io::stdout(), "  ✗ {}", e);
        }
    })
    .await
    .map_err(oli::error::AgentError::Config)?;
    Ok(())
}

fn human_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = n as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    format!("{:.1}{}", size, UNITS[unit])
}

fn prompt_provider() -> Result<oli::wizard_init::WizardProvider> {
    use oli::wizard_init::WizardProvider;
    use std::io::{BufRead, Write};

    let mut stdout = std::io::stdout();
    let _ = writeln!(stdout, "Pick a provider:");
    for (i, p) in WizardProvider::all().iter().enumerate() {
        let _ = writeln!(stdout, "  [{}] {}", i + 1, p.label());
    }
    let _ = write!(stdout, "Choice [1-3]: ");
    let _ = stdout.flush();
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| oli::error::AgentError::Config(format!("stdin read failed: {}", e)))?;
    let idx: usize = line
        .trim()
        .parse()
        .map_err(|_| oli::error::AgentError::Config(format!("`{}` is not 1-3", line.trim())))?;
    WizardProvider::all()
        .get(idx.wrapping_sub(1))
        .copied()
        .ok_or_else(|| oli::error::AgentError::Config(format!("choice {} out of range (1-3)", idx)))
}

fn prompt_api_key(provider: oli::wizard_init::WizardProvider) -> Result<String> {
    use std::io::{BufRead, Write};
    let mut stdout = std::io::stdout();
    let _ = write!(stdout, "API key for {}: ", provider.label());
    let _ = stdout.flush();
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| oli::error::AgentError::Config(format!("stdin read failed: {}", e)))?;
    let key = line.trim().to_string();
    if key.is_empty() {
        return Err(oli::error::AgentError::Config(
            "api key is required for paid providers".into(),
        ));
    }
    Ok(key)
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
            // isn't a TTY (piped invocations). When the binary
            // was built `--no-default-features` (without `tui`),
            // `use_tui` is forced false and we always use the
            // line-mode REPL.
            let tty = std::io::IsTerminal::is_terminal(&std::io::stdin())
                && std::io::IsTerminal::is_terminal(&std::io::stdout());
            #[cfg(feature = "tui")]
            let use_tui = !args.plain && tty;
            #[cfg(not(feature = "tui"))]
            let use_tui = {
                let _ = (args.plain, tty);
                false
            };
            if let Some(id) = &session_id {
                if !use_tui {
                    println!("session: {}", id);
                }
            }
            let agent = agent_base
                .with_approver(Box::new(ReadlineApprover))
                .pin_system_prompt(system_prompt)
                .await;
            #[cfg(feature = "tui")]
            {
                if use_tui {
                    // Viewport resolution order: explicit CLI flag,
                    // then `[ui].viewport`, then the W2 auto-detected
                    // default. W1 hands `Fullscreen` as the auto
                    // fallback — auto-mode lights up in W2.
                    let flag = match (args.inline, args.fullscreen) {
                        (true, _) => Some(tui::Viewport::Inline),
                        (_, true) => Some(tui::Viewport::Fullscreen),
                        _ => None,
                    };
                    let cfg_choice = cfg
                        .ui
                        .viewport
                        .as_deref()
                        .map(tui::ViewportChoice::parse)
                        .unwrap_or_default();
                    let caps = tui::caps::Capabilities::detect();
                    let viewport = tui::resolve_mode(flag, cfg_choice, caps.auto_viewport());
                    let mouse = tui::resolve_mouse(cfg.ui.mouse, caps.mouse, viewport);
                    let osc52 = tui::caps::resolve_osc52(cfg.ui.osc52.as_deref(), caps.osc52);
                    let host_hint = caps.host.clone();
                    let theme = tui::theme::load(cfg.ui.theme.as_deref().unwrap_or("dark"));
                    return tui::run(
                        agent,
                        plugin_slashes,
                        Some(plugin_reloader),
                        session_id,
                        viewport,
                        mouse,
                        osc52,
                        host_hint,
                        theme,
                    )
                    .await;
                }
            }
            let _ = use_tui; // referenced in cfg branch above
            repl::run(agent, plugin_slashes, Some(plugin_reloader)).await
        }
    }
}
