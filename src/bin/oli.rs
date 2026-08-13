//! Binary entry point for `oli`. Parses CLI args, builds the
//! agent + tool registry from config, and dispatches to a persisted
//! headless run or the line-mode REPL. Reaches into the
//! library at `oli::*` for everything substantive — this file
//! is intentionally a wiring shim, not where logic lives.

use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;
use std::io::{IsTerminal, Read};
use std::process;
use std::sync::Arc;

use oli::agent::context::SystemPromptBuilder;
use oli::agent::{Agent, RunOutcome};
use oli::bootstrap::{DefaultAgentSpawner, build_default_tools, build_memory, resolve_session_id};
use oli::config::Config;
use oli::error::Result;
use oli::policy::{AlwaysDeny, ConfigPolicy, PolicyConfig, PolicyMode, ReadlineApprover};
use oli::providers::{Provider as ProviderTrait, UsageTotals};
use oli::tools::task::{SubagentSpawner, Task};
use oli::{hooks, mcp, notes, plugins, providers, repl};

#[derive(Parser)]
#[command(
    author,
    version,
    about,
    long_about = "A minimal, hackable, scriptable coding-agent runtime.\n\n\
                  Use `oli run` for one-command/one-result automation, or invoke \
                  `oli` without a subcommand for the line-mode REPL."
)]
struct Args {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum OutputMode {
    #[default]
    Text,
    Json,
}

#[derive(Debug)]
struct RunOptions {
    prompt: Option<String>,
    conversation: Option<String>,
    continue_session: bool,
    max_turns: Option<usize>,
    strict: bool,
    output: OutputMode,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run one prompt, persist the conversation, print the result, and exit.
    Run {
        /// Prompt text. When omitted, Oli reads a non-terminal stdin to EOF.
        #[arg(short = 'p', long)]
        prompt: Option<String>,

        /// Continue a persisted conversation by id.
        #[arg(long, conflicts_with = "continue_session")]
        conversation: Option<String>,

        /// Continue the most recently modified conversation.
        #[arg(long = "continue", conflicts_with = "conversation")]
        continue_session: bool,

        /// Override the configured per-run turn cap.
        #[arg(long)]
        max_turns: Option<usize>,

        /// Deny every operation that requires an approval decision.
        #[arg(long)]
        strict: bool,

        /// Completed result format written to stdout.
        #[arg(long, value_enum, default_value_t)]
        output: OutputMode,
    },

    /// Write a starter `~/.config/oli/config.toml`.
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
        /// Validate stored credentials by forcing a token refresh,
        /// discovering models, and sending one real prompt.
        #[arg(long, conflicts_with_all = ["no_browser", "device_auth", "paste", "no_config"])]
        check: bool,

        /// Print the sign-in URL instead of launching a browser.
        /// Implied on a Linux session with no display.
        #[arg(long, conflicts_with_all = ["device_auth", "paste"])]
        no_browser: bool,

        /// Headless sign-in: shows a code to enter on another
        /// device, with no local browser or callback port. Use this
        /// over SSH.
        #[arg(long, conflicts_with = "paste")]
        device_auth: bool,

        /// Sign in with a browser on a different machine, then paste
        /// the redirect URL back here. Use this when the browser
        /// can't reach this host's localhost — SSH, Tailscale,
        /// containers — or when ports 1455/1457 are unavailable.
        #[arg(long)]
        paste: bool,

        /// Store credentials but leave config.toml alone. Without
        /// this, a successful login points `default_provider` at a
        /// `kind = "openai-chatgpt"` block, creating one if needed.
        #[arg(long)]
        no_config: bool,
    },

    /// Discard stored ChatGPT subscription credentials.
    Logout,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let result = match args.cmd {
        Some(Cmd::Run {
            prompt,
            conversation,
            continue_session,
            max_turns,
            strict,
            output,
        }) => {
            run_headless(RunOptions {
                prompt,
                conversation,
                continue_session,
                max_turns,
                strict,
                output,
            })
            .await
        }
        Some(Cmd::Init {
            provider,
            api_key,
            force,
            skip_ollama_check,
            pull,
        }) => init_command(provider, api_key, force, skip_ollama_check, pull).await,
        Some(Cmd::Login {
            check,
            no_browser,
            device_auth,
            paste,
            no_config,
        }) => login_command(check, no_browser, device_auth, paste, no_config).await,
        Some(Cmd::Logout) => logout_command(),
        None => run_interactive().await,
    };
    if let Err(e) = result {
        if !matches!(e, oli::error::AgentError::MaxTurnsExhausted(_)) {
            eprintln!("{}", e);
        }
        process::exit(1);
    }
}

/// `oli login`. Runs the ChatGPT subscription OAuth flow, stores the
/// tokens, then points `config.toml` at them — filling in `base_url`,
/// setting `default_model` from the subscription's own model list, and
/// switching `default_provider`. Other provider blocks are left
/// untouched, so switching back is a one-line edit. `--no-config`
/// stores credentials and stops there.
async fn login_command(
    check: bool,
    no_browser: bool,
    device_auth: bool,
    paste: bool,
    no_config: bool,
) -> Result<()> {
    use oli::auth::login::{LoginOptions, provision_config, run as run_login, run_paste};
    use oli::auth::store::AuthStore;
    use oli::auth::{ISSUER, client_id};

    let store = AuthStore::default_location()?;
    if check {
        println!("{}", oli::auth::login::run_release_check(&store).await?);
        return Ok(());
    }
    let opts = LoginOptions {
        open_browser: !no_browser,
        ..LoginOptions::default()
    };
    if device_auth {
        oli::auth::device::run(ISSUER, &client_id(), &store).await?;
    } else if paste {
        run_paste(&opts, &store).await?;
    } else {
        run_login(&opts, &store).await?;
    }

    if no_config {
        println!(
            "\nCredentials saved. --no-config was set, so config.toml is unchanged; \n\
             point a provider at them with `kind = \"openai-chatgpt\"`."
        );
    } else {
        provision_config(&store).await?;
    }
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

/// `oli init` writes config, falling back to stdin prompts for fields not given
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

fn select_prompt(argument: Option<String>, piped: &str) -> Result<String> {
    let piped_is_empty = piped.trim().is_empty();

    let arg_was_supplied = argument.is_some();
    let arg_is_empty_or_whitespace = argument.as_ref().map_or(true, |p| p.trim().is_empty());

    if arg_was_supplied && arg_is_empty_or_whitespace {
        return Err(oli::error::AgentError::Config(
            "prompt argument is empty or whitespace-only".into(),
        ));
    }

    match (argument.filter(|p| !p.trim().is_empty()), piped_is_empty) {
        (Some(_), false) => Err(oli::error::AgentError::Config(
            "prompt provided both by --prompt and stdin".into(),
        )),
        (Some(prompt), true) => Ok(prompt),
        (None, false) => Ok(piped.to_string()),
        (None, true) => Err(oli::error::AgentError::Config(
            "missing prompt: pass --prompt or pipe one to `oli run`".into(),
        )),
    }
}

fn resolve_prompt(argument: Option<String>) -> Result<String> {
    let mut piped = String::new();
    if !std::io::stdin().is_terminal() {
        std::io::stdin().read_to_string(&mut piped)?;
    }
    select_prompt(argument, &piped)
}

/// Session token accounting for `--output json`. A category no call
/// reported serializes as `null`; `unreported_calls` says how much of
/// each sum is missing, so a consumer can tell a complete total from a
/// partial one.
#[derive(Serialize)]
struct UsageOutput {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
    cache_read_tokens: Option<u64>,
    cache_write_tokens: Option<u64>,
    reasoning_tokens: Option<u64>,
    calls: u32,
    unreported_calls: UnreportedCalls,
}

#[derive(Serialize)]
struct UnreportedCalls {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
    cache_read_tokens: u32,
    cache_write_tokens: u32,
    reasoning_tokens: u32,
}

impl From<UsageTotals> for UsageOutput {
    fn from(t: UsageTotals) -> Self {
        Self {
            prompt_tokens: t.prompt_tokens.reported(),
            completion_tokens: t.completion_tokens.reported(),
            total_tokens: t.total_tokens.reported(),
            cache_read_tokens: t.cache_read_tokens.reported(),
            cache_write_tokens: t.cache_write_tokens.reported(),
            reasoning_tokens: t.reasoning_tokens.reported(),
            calls: t.calls,
            unreported_calls: UnreportedCalls {
                prompt_tokens: t.prompt_tokens.unreported_calls,
                completion_tokens: t.completion_tokens.unreported_calls,
                total_tokens: t.total_tokens.unreported_calls,
                cache_read_tokens: t.cache_read_tokens.unreported_calls,
                cache_write_tokens: t.cache_write_tokens.unreported_calls,
                reasoning_tokens: t.reasoning_tokens.unreported_calls,
            },
        }
    }
}

#[derive(Serialize)]
struct CompletedRun<'a> {
    status: &'static str,
    conversation_id: &'a str,
    response: &'a str,
    provider: &'a str,
    model: &'a str,
    usage: Option<UsageOutput>,
}

#[derive(Serialize)]
struct IncompleteRun<'a> {
    status: &'static str,
    conversation_id: &'a str,
    reason: &'static str,
    max_turns: usize,
    provider: &'a str,
    model: &'a str,
    usage: Option<UsageOutput>,
}

fn text_exhaustion_diagnostic(limit: usize, message: &str, conversation_id: &str) -> String {
    let default_message = format!("(max_turns reached: {limit})");
    let mut diagnostic = format!("max turns exhausted: {limit}\n");
    if !message.trim().is_empty() && message != default_message {
        diagnostic.push_str(&format!("detail: {message}\n"));
    }
    diagnostic.push_str(&format!("conversation: {conversation_id}\n"));
    diagnostic
}

async fn run_headless(options: RunOptions) -> Result<()> {
    let prompt = resolve_prompt(options.prompt.clone())?;
    run_agent(Some((options, prompt))).await
}

async fn run_interactive() -> Result<()> {
    run_agent(None).await
}

async fn run_agent(headless: Option<(RunOptions, String)>) -> Result<()> {
    let cfg = std::sync::Arc::new(Config::load_or_default()?);
    let provider_name = cfg.default_provider.clone();
    let model = cfg.model_for(&provider_name)?;
    let output_provider = provider_name.clone();
    let output_model = model.clone();

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
    let strict = headless.as_ref().is_some_and(|(options, _)| options.strict);
    let policy_config = policy_config_for_run(&cfg.policy, strict, headless.is_some());
    let policy = Box::new(ConfigPolicy::from_config(&policy_config));

    let interactive = headless.is_none();
    let (conversation, continue_session) = headless
        .as_ref()
        .map(|(options, _)| (options.conversation.as_deref(), options.continue_session))
        .unwrap_or((None, false));
    let (session_id, is_fresh) = resolve_session_id(conversation, continue_session)?;
    let (memory, replayed_reads, read_logger) = build_memory(Some(&session_id), is_fresh).await?;

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

    let max_turns = headless
        .as_ref()
        .and_then(|(options, _)| options.max_turns)
        .unwrap_or(cfg.agent.max_turns);

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

    match headless {
        Some((options, prompt)) => {
            let mut agent = agent_base
                .with_approver(Box::new(AlwaysDeny))
                .pin_system_prompt(system_prompt)
                .await?;
            let outcome = agent.run(&prompt).await?;
            // No call reported anything at all — say nothing rather than
            // emit a shape full of nulls.
            let usage = agent
                .session_usage
                .any_reported()
                .then(|| UsageOutput::from(agent.session_usage));
            let response = match outcome {
                RunOutcome::Completed(response) => response,
                RunOutcome::MaxTurnsExhausted { limit, message } => {
                    match options.output {
                        OutputMode::Text => {
                            eprint!(
                                "{}",
                                text_exhaustion_diagnostic(limit, &message, &session_id)
                            );
                        }
                        OutputMode::Json => {
                            let result = IncompleteRun {
                                status: "incomplete",
                                conversation_id: &session_id,
                                reason: "max_turns_exhausted",
                                max_turns: limit,
                                provider: &output_provider,
                                model: &output_model,
                                usage,
                            };
                            println!("{}", serde_json::to_string(&result)?);
                        }
                    }
                    return Err(oli::error::AgentError::MaxTurnsExhausted(limit));
                }
            };
            match options.output {
                OutputMode::Text => {
                    if !response.is_empty() {
                        println!("{response}");
                    }
                    eprintln!("conversation: {session_id}");
                }
                OutputMode::Json => {
                    let result = CompletedRun {
                        status: "completed",
                        conversation_id: &session_id,
                        response: &response,
                        provider: &output_provider,
                        model: &output_model,
                        usage,
                    };
                    println!("{}", serde_json::to_string(&result)?);
                }
            }
            Ok(())
        }
        None => {
            println!("session: {session_id}");
            let agent = agent_base
                .with_approver(Box::new(ReadlineApprover))
                .pin_system_prompt(system_prompt)
                .await?;
            repl::run(agent, plugin_slashes, Some(plugin_reloader)).await
        }
    }
}

fn policy_config_for_run(config: &PolicyConfig, strict: bool, one_shot: bool) -> PolicyConfig {
    let mut config = config.clone();
    if strict && one_shot {
        config.mode = PolicyMode::Ask;
    }
    config
}

#[cfg(test)]
mod tests {
    use super::*;
    use oli::providers::Usage;

    #[test]
    fn run_command_parses_the_headless_contract() {
        let args = Args::try_parse_from([
            "oli",
            "run",
            "--prompt",
            "hi",
            "--conversation",
            "abc",
            "--output",
            "json",
        ])
        .unwrap();
        let Some(Cmd::Run {
            prompt,
            conversation,
            output,
            ..
        }) = args.cmd
        else {
            panic!("expected run command");
        };
        assert_eq!(prompt.as_deref(), Some("hi"));
        assert_eq!(conversation.as_deref(), Some("abc"));
        assert!(matches!(output, OutputMode::Json));
    }

    #[test]
    fn removed_global_prompt_and_tui_flags_are_rejected() {
        assert!(Args::try_parse_from(["oli", "-p", "hi"]).is_err());
        assert!(Args::try_parse_from(["oli", "--inline"]).is_err());
        assert!(Args::try_parse_from(["oli", "--fullscreen"]).is_err());
        assert!(Args::try_parse_from(["oli", "--plain"]).is_err());
    }

    #[test]
    fn conversation_and_continue_are_mutually_exclusive() {
        assert!(
            Args::try_parse_from(["oli", "run", "--conversation", "abc", "--continue"]).is_err()
        );
    }

    #[test]
    fn prompt_selection_accepts_exactly_one_source() {
        assert_eq!(select_prompt(Some("arg".into()), "").unwrap(), "arg");
        assert_eq!(select_prompt(None, "pipe\n").unwrap(), "pipe\n");
        assert!(select_prompt(Some("arg".into()), "pipe").is_err());
        assert!(select_prompt(None, "  ").is_err());
    }

    #[test]
    fn completed_run_serializes_stable_machine_fields() {
        let run = CompletedRun {
            status: "completed",
            conversation_id: "c1",
            response: "done",
            provider: "openrouter",
            model: "model",
            usage: Some(UsageOutput::from(totals(&[Usage {
                prompt_tokens: Some(2),
                completion_tokens: Some(3),
                total_tokens: Some(5),
                ..Usage::default()
            }]))),
        };
        let value = serde_json::to_value(run).unwrap();
        assert_eq!(value["status"], "completed");
        assert_eq!(value["conversation_id"], "c1");
        assert_eq!(value["response"], "done");
        assert_eq!(value["usage"]["total_tokens"], 5);
    }

    fn totals(calls: &[Usage]) -> UsageTotals {
        let mut t = UsageTotals::default();
        for u in calls {
            t.add(Some(*u));
        }
        t
    }

    #[test]
    fn a_category_no_call_reported_serializes_as_null_not_zero() {
        let usage = UsageOutput::from(totals(&[Usage {
            prompt_tokens: Some(2),
            completion_tokens: Some(3),
            total_tokens: Some(5),
            ..Usage::default()
        }]));
        let value = serde_json::to_value(usage).unwrap();
        assert_eq!(value["prompt_tokens"], 2);
        assert!(value["cache_read_tokens"].is_null());
        assert!(value["cache_write_tokens"].is_null());
        assert!(value["reasoning_tokens"].is_null());
        assert_eq!(value["calls"], 1);
        assert_eq!(value["unreported_calls"]["cache_read_tokens"], 1);
        assert_eq!(value["unreported_calls"]["prompt_tokens"], 0);
    }

    #[test]
    fn a_partial_session_sum_says_how_many_calls_are_missing_from_it() {
        let usage = UsageOutput::from(totals(&[
            Usage {
                prompt_tokens: Some(2),
                cache_read_tokens: Some(64),
                ..Usage::default()
            },
            Usage {
                prompt_tokens: Some(3),
                ..Usage::default()
            },
        ]));
        let value = serde_json::to_value(usage).unwrap();
        assert_eq!(value["prompt_tokens"], 5);
        assert_eq!(value["cache_read_tokens"], 64);
        assert_eq!(value["calls"], 2);
        assert_eq!(value["unreported_calls"]["cache_read_tokens"], 1);
    }

    #[test]
    fn completed_run_uses_null_when_provider_omits_usage() {
        let run = CompletedRun {
            status: "completed",
            conversation_id: "c1",
            response: "done",
            provider: "local",
            model: "model",
            usage: None,
        };
        let value = serde_json::to_value(run).unwrap();
        assert!(value["usage"].is_null());
    }

    #[test]
    fn text_exhaustion_always_names_reason_even_when_hook_message_is_empty() {
        let output = text_exhaustion_diagnostic(3, "", "conversation-1");
        assert_eq!(
            output,
            "max turns exhausted: 3\nconversation: conversation-1\n"
        );
    }

    #[test]
    fn text_exhaustion_keeps_custom_hook_message_as_optional_detail() {
        let output = text_exhaustion_diagnostic(3, "resume when ready", "conversation-1");
        assert_eq!(
            output,
            "max turns exhausted: 3\ndetail: resume when ready\nconversation: conversation-1\n"
        );
    }

    #[test]
    fn strict_one_shot_runs_restore_ask_policy_before_denying_approvals() {
        let config = policy_config_for_run(&PolicyConfig::default(), true, true);
        assert_eq!(config.mode, PolicyMode::Ask);
    }

    #[test]
    fn strict_interactive_runs_preserve_configured_policy_mode() {
        let config = policy_config_for_run(&PolicyConfig::default(), true, false);
        assert_eq!(config.mode, PolicyMode::Auto);
    }

    #[test]
    fn regular_runs_preserve_configured_policy_mode() {
        let config = policy_config_for_run(&PolicyConfig::default(), false, true);
        assert_eq!(config.mode, PolicyMode::Auto);
    }
}
