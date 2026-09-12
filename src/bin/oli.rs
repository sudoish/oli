//! Clap entrypoint and top-level command dispatch for `oli`.

use std::process;

use clap::{Parser, Subcommand, ValueEnum};
use oli::{cli, error::Result};

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

impl From<OutputMode> for cli::run::OutputMode {
    fn from(mode: OutputMode) -> Self {
        match mode {
            OutputMode::Text => Self::Text,
            OutputMode::Json => Self::Json,
        }
    }
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
        /// Deny every tool call.
        #[arg(long)]
        strict: bool,
        /// Completed result format written to stdout.
        #[arg(long, value_enum, default_value_t)]
        output: OutputMode,
    },
    /// Compare captured memory strategies without contacting a provider.
    Replay {
        /// Immutable JSON fixture with transcript, ledger, and optional outcomes.
        #[arg(long)]
        fixture: std::path::PathBuf,
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
    /// Add, authorize, and inspect Model Context Protocol servers.
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
}

#[derive(Subcommand, Debug)]
enum McpCommand {
    /// Add an HTTP MCP server and authorize it in the browser.
    Add {
        /// Name used for config, tools, and later MCP commands.
        name: String,
        /// Streamable-HTTP MCP endpoint.
        url: String,
        /// Paste the browser redirect URL back instead of binding localhost.
        #[arg(long)]
        paste: bool,
        /// Request only the OAuth `read` scope.
        #[arg(long)]
        read_only: bool,
    },
    /// Reauthorize an OAuth MCP server already present in config.
    Login {
        name: String,
        #[arg(long)]
        paste: bool,
        #[arg(long)]
        read_only: bool,
    },
    /// Discard one MCP server's stored OAuth credential.
    Logout { name: String },
    /// Show OAuth status for one server, or all configured OAuth servers.
    Status { name: Option<String> },
}

#[tokio::main]
async fn main() {
    if let Err(error) = dispatch(Args::parse()).await {
        if !matches!(error, oli::error::AgentError::MaxTurnsExhausted(_)) {
            eprintln!("{error}");
        }
        process::exit(1);
    }
}

async fn dispatch(args: Args) -> Result<()> {
    match args.cmd {
        Some(Cmd::Run {
            prompt,
            conversation,
            continue_session,
            max_turns,
            strict,
            output,
        }) => {
            cli::run::headless(cli::run::Options {
                prompt,
                conversation,
                continue_session,
                max_turns,
                strict,
                output: output.into(),
            })
            .await
        }
        Some(Cmd::Replay { fixture }) => cli::replay::run(&fixture),
        Some(Cmd::Init {
            provider,
            api_key,
            force,
            skip_ollama_check,
            pull,
        }) => {
            cli::init::run(cli::init::Options {
                provider,
                api_key,
                force,
                skip_ollama_check,
                pull,
            })
            .await
        }
        Some(Cmd::Login {
            check,
            no_browser,
            device_auth,
            paste,
            no_config,
        }) => {
            cli::auth::login(cli::auth::Options {
                check,
                no_browser,
                device_auth,
                paste,
                no_config,
            })
            .await
        }
        Some(Cmd::Logout) => cli::auth::logout(),
        Some(Cmd::Mcp { command }) => dispatch_mcp(command).await,
        None => cli::run::interactive().await,
    }
}

async fn dispatch_mcp(command: McpCommand) -> Result<()> {
    match command {
        McpCommand::Add {
            name,
            url,
            paste,
            read_only,
        } => cli::mcp::add(&name, &url, paste, read_only).await,
        McpCommand::Login {
            name,
            paste,
            read_only,
        } => cli::mcp::login(&name, paste, read_only).await,
        McpCommand::Logout { name } => cli::mcp::logout(&name),
        McpCommand::Status { name } => cli::mcp::status(name).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn mcp_add_parses_the_one_command_oauth_flow() {
        let args = Args::try_parse_from([
            "oli",
            "mcp",
            "add",
            "linear",
            "https://mcp.linear.app/mcp",
            "--paste",
            "--read-only",
        ])
        .unwrap();
        let Some(Cmd::Mcp {
            command:
                McpCommand::Add {
                    name,
                    url,
                    paste,
                    read_only,
                },
        }) = args.cmd
        else {
            panic!("expected mcp add command");
        };
        assert_eq!(name, "linear");
        assert_eq!(url, "https://mcp.linear.app/mcp");
        assert!(paste);
        assert!(read_only);
    }

    #[test]
    fn strict_one_shot_flag_selects_deterministic_tool_denial() {
        let args = Args::try_parse_from(["oli", "run", "--strict", "-p", "test"]).unwrap();
        assert!(matches!(args.cmd, Some(Cmd::Run { strict: true, .. })));
    }
}
