//! Interactive and headless agent startup.

use std::io::{IsTerminal, Read};
use std::sync::Arc;

use serde::Serialize;

use crate::agent::Agent;
use crate::agent::context::SystemPromptBuilder;
use crate::bootstrap::{
    DefaultAgentSpawner, build_default_tools, build_memory, build_run_accounting,
};
use crate::config::Config;
use crate::error::{AgentError, Result};
use crate::hooks;
use crate::ledger::RunSummary;
use crate::policy::DenyAll;
use crate::providers::{Provider as ProviderTrait, UsageTotals};
use crate::runtime::{
    CancellationToken, CommandOutcome, RuntimeCommand, SessionRuntime, UsageSnapshot,
};
use crate::tools::task::{SubagentSpawner, Task};
use crate::{mcp, notes, plugins, providers, repl};

/// Completed result format for a headless run.
#[derive(Clone, Copy, Debug, Default)]
pub enum OutputMode {
    #[default]
    Text,
    Json,
}

/// Inputs to a persisted headless run.
#[derive(Debug)]
pub struct Options {
    pub prompt: Option<String>,
    pub conversation: Option<String>,
    pub continue_session: bool,
    pub max_turns: Option<usize>,
    pub strict: bool,
    pub output: OutputMode,
}

/// Resolve the prompt, assemble an agent, run it once, and print the result.
pub async fn headless(options: Options) -> Result<()> {
    let prompt = resolve_prompt(options.prompt.clone())?;
    run_agent(Some((options, prompt))).await
}

/// Assemble an agent and enter the line REPL.
pub async fn interactive() -> Result<()> {
    run_agent(None).await
}

fn select_prompt(argument: Option<String>, piped: &str) -> Result<String> {
    let piped_is_empty = piped.trim().is_empty();
    let arg_was_supplied = argument.is_some();
    let arg_is_empty_or_whitespace = argument.as_ref().map_or(true, |p| p.trim().is_empty());

    if arg_was_supplied && arg_is_empty_or_whitespace {
        return Err(AgentError::Config(
            "prompt argument is empty or whitespace-only".into(),
        ));
    }

    match (argument.filter(|p| !p.trim().is_empty()), piped_is_empty) {
        (Some(_), false) => Err(AgentError::Config(
            "prompt provided both by --prompt and stdin".into(),
        )),
        (Some(prompt), true) => Ok(prompt),
        (None, false) => Ok(piped.to_string()),
        (None, true) => Err(AgentError::Config(
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
    fn from(totals: UsageTotals) -> Self {
        Self {
            prompt_tokens: totals.prompt_tokens.reported(),
            completion_tokens: totals.completion_tokens.reported(),
            total_tokens: totals.total_tokens.reported(),
            cache_read_tokens: totals.cache_read_tokens.reported(),
            cache_write_tokens: totals.cache_write_tokens.reported(),
            reasoning_tokens: totals.reasoning_tokens.reported(),
            calls: totals.calls,
            unreported_calls: UnreportedCalls {
                prompt_tokens: totals.prompt_tokens.unreported_calls,
                completion_tokens: totals.completion_tokens.unreported_calls,
                total_tokens: totals.total_tokens.unreported_calls,
                cache_read_tokens: totals.cache_read_tokens.unreported_calls,
                cache_write_tokens: totals.cache_write_tokens.unreported_calls,
                reasoning_tokens: totals.reasoning_tokens.unreported_calls,
            },
        }
    }
}

impl From<UsageSnapshot> for UsageOutput {
    fn from(usage: UsageSnapshot) -> Self {
        Self {
            prompt_tokens: usage.prompt_tokens.reported,
            completion_tokens: usage.completion_tokens.reported,
            total_tokens: usage.total_tokens.reported,
            cache_read_tokens: usage.cache_read_tokens.reported,
            cache_write_tokens: usage.cache_write_tokens.reported,
            reasoning_tokens: usage.reasoning_tokens.reported,
            calls: usage.calls,
            unreported_calls: UnreportedCalls {
                prompt_tokens: usage.prompt_tokens.unreported_calls,
                completion_tokens: usage.completion_tokens.unreported_calls,
                total_tokens: usage.total_tokens.unreported_calls,
                cache_read_tokens: usage.cache_read_tokens.unreported_calls,
                cache_write_tokens: usage.cache_write_tokens.unreported_calls,
                reasoning_tokens: usage.reasoning_tokens.unreported_calls,
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
    accounting: RunSummary,
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
    accounting: RunSummary,
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

async fn run_agent(headless: Option<(Options, String)>) -> Result<()> {
    let cfg = Arc::new(Config::load_or_default()?);
    let provider_name = cfg.default_provider.clone();
    let model = cfg.model_for(&provider_name)?;
    let output_provider = provider_name.clone();
    let output_model = model.clone();
    let provider: Box<dyn ProviderTrait> = providers::build(&cfg, &provider_name)?;

    // One notes store is shared across the parent and subagents.
    let notes_store: Arc<dyn notes::NotesStore> = Arc::new(notes::FilesystemNotesStore::at(
        notes::FilesystemNotesStore::default_dir().unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."))
                .join(".oli")
                .join("notes")
        }),
    ));
    let mut tools = build_default_tools(&cfg, notes_store.clone());

    // Down MCP servers remain in the handles so `/mcp` can explain failures.
    let mcp_handles = Arc::new(mcp::connect_all(&cfg.mcp).await);
    for tool in mcp::build_tools(&mcp_handles).await {
        tools.register_box(tool);
    }

    let spawner: Arc<dyn SubagentSpawner> = Arc::new(DefaultAgentSpawner {
        cfg: cfg.clone(),
        provider_name: provider_name.clone(),
        notes_store: notes_store.clone(),
        mcp_handles: mcp_handles.clone(),
    });
    // Only the parent gets Task, preventing recursive subagent spawning.
    tools.register(Task::new(spawner.clone()));

    // Plugins get a baseline tool snapshot without Task or other plugins.
    let plugin_host_tools = Arc::new(tokio::sync::Mutex::new(build_default_tools(
        &cfg,
        notes_store.clone(),
    )));
    let plugin_reloader = Arc::new(plugins::PluginReloader::new(
        plugin_host_tools.clone(),
        Some(spawner.clone()),
    ));
    let plugins = plugins::load_all(plugin_host_tools, Some(spawner)).await;
    let plugin_manifest = plugins.manifest;
    for tool in plugins.tools {
        tools.register_box(tool);
    }
    let mut plugin_slashes = plugins.slash_commands;
    // `/mcp` uses the extra-slash channel because it also owns startup state.
    plugin_slashes.push(Box::new(repl::slash::Mcp::new(mcp_handles.clone())));
    let plugin_hooks = plugins.hooks;

    let system_prompt = SystemPromptBuilder::from_env().build().await;
    let (conversation, continue_session) = headless
        .as_ref()
        .map(|(options, _)| (options.conversation.as_deref(), options.continue_session))
        .unwrap_or((None, false));
    let (session_id, is_fresh) = crate::cli::sessions::resolve(conversation, continue_session)?;

    let max_turns = headless
        .as_ref()
        .and_then(|(options, _)| options.max_turns)
        .unwrap_or(cfg.agent.max_turns);
    // Session identity belongs in the transcript; measurements use its sibling ledger.
    let (session_meta, ledger) = build_run_accounting(
        &cfg,
        &output_provider,
        &output_model,
        &session_id,
        is_fresh,
        max_turns,
    );
    let ledger_path = ledger.path().map(|path| path.display().to_string());
    let (memory, replayed_reads, read_logger) =
        build_memory(Some(&session_id), is_fresh, Some(session_meta)).await?;

    let mut hooks = hooks::HookRegistry::new();
    for hook in plugin_hooks {
        hooks.register_box(hook);
    }
    let agent_base = Agent::new(provider, tools, model)
        .with_config(cfg.clone(), provider_name)
        .with_memory(memory)
        .with_hooks(hooks)
        .with_plugin_manifest(plugin_manifest)
        .with_mcp_handles(mcp_handles.clone())
        .with_ledger(ledger)
        .with_max_turns(max_turns);

    // Restore prior reads before logging new reads back to the transcript.
    {
        let context = agent_base.tool_context();
        for path in replayed_reads {
            context.insert_canonical_read(path).await;
        }
        if let Some(logger) = read_logger {
            context.set_read_logger(logger).await;
        }
    }

    match headless {
        Some((options, prompt)) => {
            let agent_base = if options.strict {
                agent_base.with_policy(Box::new(DenyAll))
            } else {
                agent_base
            };
            let agent = agent_base.pin_system_prompt(system_prompt).await?;
            let mut runtime = SessionRuntime::new(agent, session_id.clone());
            let outcome = runtime
                .execute(
                    RuntimeCommand::Prompt(prompt),
                    &CancellationToken::new(),
                    |_| {},
                )
                .await;
            let accounting = runtime.finish().await;
            let outcome = outcome?;
            let snapshot = runtime.snapshot().await;
            // Omit usage entirely when no provider call reported any category.
            let usage = snapshot
                .usage
                .any_reported()
                .then(|| UsageOutput::from(snapshot.usage));
            let response = match outcome {
                CommandOutcome::Completed(response) => response,
                CommandOutcome::MaxTurnsExhausted { limit, message } => {
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
                                accounting,
                            };
                            println!("{}", serde_json::to_string(&result)?);
                        }
                    }
                    return Err(AgentError::MaxTurnsExhausted(limit));
                }
                CommandOutcome::Cancelled => unreachable!("headless cancellation is not requested"),
            };
            match options.output {
                OutputMode::Text => {
                    if !response.is_empty() {
                        println!("{response}");
                    }
                    eprintln!("conversation: {session_id}");
                    if let Some(path) = &ledger_path {
                        eprintln!("accounting: {path}");
                    }
                }
                OutputMode::Json => {
                    let result = CompletedRun {
                        status: "completed",
                        conversation_id: &session_id,
                        response: &response,
                        provider: &output_provider,
                        model: &output_model,
                        usage,
                        accounting,
                    };
                    println!("{}", serde_json::to_string(&result)?);
                }
            }
            Ok(())
        }
        None => {
            println!("session: {session_id}");
            let agent = agent_base.pin_system_prompt(system_prompt).await?;
            let runtime = SessionRuntime::new(agent, session_id);
            repl::run(runtime, plugin_slashes, Some(plugin_reloader)).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::Usage;

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
            accounting: accounting(),
        };
        let value = serde_json::to_value(run).unwrap();
        assert_eq!(value["status"], "completed");
        assert_eq!(value["conversation_id"], "c1");
        assert_eq!(value["response"], "done");
        assert_eq!(value["usage"]["total_tokens"], 5);
    }

    fn accounting() -> RunSummary {
        crate::ledger::Ledger::default().summary()
    }

    #[test]
    fn a_completed_run_carries_bounded_accounting_aggregates_and_no_content() {
        let value = serde_json::to_value(CompletedRun {
            status: "completed",
            conversation_id: "c1",
            response: "done",
            provider: "test",
            model: "test-model",
            usage: None,
            accounting: accounting(),
        })
        .unwrap();
        let accounting = &value["accounting"];
        assert_eq!(accounting["kind"], "summary");
        assert_eq!(accounting["schema"], crate::ledger::SCHEMA);
        assert_eq!(accounting["calls"], 0);
        assert!(accounting["context"]["total"].is_number());
        assert!(accounting["cost"]["amount"].is_null());
        assert!(accounting["first_request"].is_null());
    }

    fn totals(calls: &[Usage]) -> UsageTotals {
        let mut totals = UsageTotals::default();
        for usage in calls {
            totals.add(Some(*usage));
        }
        totals
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
            accounting: accounting(),
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
}
