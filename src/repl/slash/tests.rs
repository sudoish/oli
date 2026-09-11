#[cfg(test)]
mod tests {
    use super::*;
    use super::config::reload_config_at;
    use crate::agent::Agent;
    use crate::providers::fake::FakeProvider;
    use crate::tools::Registry;
    use async_trait::async_trait;
    use serde_json::json;

    fn fresh_agent() -> Agent {
        let provider = FakeProvider::new(vec![json!({"role":"assistant","content":"x"})]);
        Agent::new(Box::new(provider), Registry::new(), "m".into())
    }

    #[tokio::test]
    async fn dispatch_unknown_command_returns_none() {
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent();
        let out = reg.dispatch("nope", &mut agent).await;
        assert!(out.is_none());
    }

    #[tokio::test]
    async fn config_reload_picks_up_default_provider_change() {
        // The agent starts pointing at provider "ollama" (model "llama"),
        // then we write a config that switches the default to a fresh
        // openai-compat block with a different model. /config reload
        // should pick that up without losing memory.
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join(".oli").join("config.toml");
        std::fs::create_dir_all(cfg_path.parent().unwrap()).unwrap();
        std::fs::write(
            &cfg_path,
            r#"
default_provider = "alt"

[providers.alt]
kind          = "openai-compat"
base_url      = "http://example.invalid/v1"
api_key       = "k"
default_model = "alt-model"
"#,
        )
        .unwrap();
        let mut agent = fresh_agent();
        agent.provider_name = "ollama".into();
        agent.model = "llama".into();
        // Stash a memory entry so we can verify it survives.
        agent
            .memory
            .record(json!({"role":"user","content":"keep me"}))
            .await
            .unwrap();
        let mem_len_before = agent.memory.len();

        let out = reload_config_at(&mut agent, dir.path()).await;
        match out {
            SlashOutcome::Continue(Some(body)) => {
                assert!(body.contains("provider:"));
                assert!(body.contains("alt"));
                assert!(body.contains("alt-model"));
            }
            _ => panic!("expected reload summary, got {:?}", out),
        }
        assert_eq!(agent.provider_name, "alt");
        assert_eq!(agent.model, "alt-model");
        // Memory survived.
        assert_eq!(agent.memory.len(), mem_len_before);
    }

    #[tokio::test]
    async fn config_reload_unknown_subcommand_surfaces_help() {
        let mut agent = fresh_agent();
        let reg = SlashRegistry::default_set();
        let out = reg.dispatch("config nope", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(body)) => assert!(body.contains("/config reload")),
            _ => panic!("expected help text, got {:?}", out),
        }
    }

    #[tokio::test]
    async fn diagnostics_renders_recent_entries() {
        // Serialized with the diagnostics::tests cases that
        // share the process-wide ring buffer.
        let _g = crate::diagnostics::TEST_SERIAL.lock().unwrap();
        crate::diagnostics::clear();
        crate::diagnostics::push(
            crate::diagnostics::Level::Warn,
            "[plugins] foo failed to load: oops".into(),
        );
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent();
        let out = reg.dispatch("diagnostics", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(body)) => {
                assert!(body.contains("[warn]"));
                assert!(body.contains("foo failed to load"));
            }
            _ => panic!("expected Continue with body"),
        }
        crate::diagnostics::clear();
    }

    #[tokio::test]
    async fn diagnostics_clear_wipes_the_ring() {
        let _g = crate::diagnostics::TEST_SERIAL.lock().unwrap();
        crate::diagnostics::clear();
        crate::diagnostics::push(crate::diagnostics::Level::Info, "noise".into());
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent();
        let out = reg.dispatch("diagnostics clear", &mut agent).await.unwrap();
        assert!(matches!(out, SlashOutcome::Continue(Some(_))));
        assert!(crate::diagnostics::tail(usize::MAX).is_empty());
    }

    #[tokio::test]
    async fn diagnostics_empty_states_renders_a_note() {
        let _g = crate::diagnostics::TEST_SERIAL.lock().unwrap();
        crate::diagnostics::clear();
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent();
        let out = reg.dispatch("diagnostics", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(body)) => {
                assert!(body.contains("no diagnostics recorded"));
            }
            _ => panic!("expected note about empty buffer"),
        }
    }

    #[tokio::test]
    async fn clear_resets_agent_history_but_keeps_pinned_system_prompt() {
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent().pin_system_prompt("sys").await.unwrap();
        agent
            .memory
            .record(json!({"role":"user","content":"prior"}))
            .await
            .unwrap();

        let out = reg.dispatch("clear", &mut agent).await.unwrap();
        assert!(matches!(out, SlashOutcome::Continue(Some(_))));
        assert_eq!(agent.memory.len(), 0);

        // System prompt is pinned, so it survives `clear()` and reappears
        // at the head of the next snapshot.
        let snap = agent.memory.snapshot().await;
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0]["role"], "system");
        assert_eq!(snap[0]["content"], "sys");
    }

    #[tokio::test]
    async fn exit_returns_exit_outcome() {
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent();
        let out = reg.dispatch("exit", &mut agent).await.unwrap();
        assert_eq!(out, SlashOutcome::Exit);
    }

    #[tokio::test]
    async fn dispatch_strips_command_name_from_args() {
        struct CaptureArgs(std::sync::Arc<std::sync::Mutex<String>>);
        #[async_trait]
        impl SlashCommand for CaptureArgs {
            fn name(&self) -> &str {
                "capture"
            }
            fn description(&self) -> &str {
                "test"
            }
            async fn run(&self, args: &str, _agent: &mut Agent) -> SlashOutcome {
                *self.0.lock().unwrap() = args.to_string();
                SlashOutcome::Continue(None)
            }
        }
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let mut reg = SlashRegistry::new();
        reg.register(CaptureArgs(captured.clone()));

        let mut agent = fresh_agent();
        reg.dispatch("capture  with args", &mut agent)
            .await
            .unwrap();
        assert_eq!(*captured.lock().unwrap(), "with args");
    }

    #[test]
    fn render_help_lists_all_registered_commands_in_order() {
        let reg = SlashRegistry::default_set();
        let s = render_help(&reg);
        let clear_pos = s.find("/clear").unwrap();
        let help_pos = s.find("/help").unwrap();
        let exit_pos = s.find("/exit").unwrap();
        assert!(clear_pos < help_pos);
        assert!(help_pos < exit_pos);
        // Phase-2 batch is registered between help and exit.
        for name in &["/cost", "/tools", "/system", "/memory", "/compact"] {
            let pos = s.find(name).unwrap_or_else(|| panic!("missing {}", name));
            assert!(pos > help_pos, "{} should come after /help", name);
            assert!(pos < exit_pos, "{} should come before /exit", name);
        }
    }

    #[tokio::test]
    async fn cost_reports_no_usage_yet_when_agent_has_not_run() {
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent();
        let out = reg.dispatch("cost", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(msg)) => {
                assert!(msg.contains("no usage"));
            }
            _ => panic!("expected Continue(Some(_)), got {:?}", out),
        }
    }

    #[tokio::test]
    async fn cost_reports_token_breakdown_when_usage_present() {
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent();
        agent.last_usage = Some(crate::providers::Usage {
            prompt_tokens: Some(10),
            completion_tokens: Some(4),
            total_tokens: Some(14),
            ..crate::providers::Usage::default()
        });
        let out = reg.dispatch("cost", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(msg)) => {
                assert!(msg.contains("10 prompt"));
                assert!(msg.contains("4 completion"));
                assert!(msg.contains("14"));
            }
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn cost_reports_session_total_alongside_last_call() {
        // After multiple chat rounds the session total should equal
        // the sum, not just the last round.
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent();
        agent.last_usage = Some(crate::providers::Usage {
            prompt_tokens: Some(7),
            completion_tokens: Some(3),
            total_tokens: Some(10),
            ..crate::providers::Usage::default()
        });
        for (prompt, completion, total) in [(15, 6, 21), (7, 3, 10)] {
            agent.session_usage.add(Some(crate::providers::Usage {
                prompt_tokens: Some(prompt),
                completion_tokens: Some(completion),
                total_tokens: Some(total),
                ..crate::providers::Usage::default()
            }));
        }
        let out = reg.dispatch("cost", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(msg)) => {
                // last call line
                assert!(msg.contains("last call:"));
                assert!(msg.contains("7 prompt"));
                assert!(msg.contains("10 tokens"));
                // session line
                assert!(msg.contains("session (2 calls):"));
                assert!(msg.contains("22 prompt"));
                assert!(msg.contains("31 tokens"));
            }
            _ => panic!("expected Continue(Some(_)), got {:?}", out),
        }
    }

    #[tokio::test]
    async fn cost_renders_an_unreported_category_as_unknown_not_zero() {
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent();
        agent.last_usage = Some(crate::providers::Usage {
            prompt_tokens: Some(10),
            completion_tokens: Some(4),
            total_tokens: Some(14),
            cache_read_tokens: Some(2048),
            ..crate::providers::Usage::default()
        });
        let out = reg.dispatch("cost", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(msg)) => {
                assert!(msg.contains("cache read 2048"), "{msg}");
                assert!(msg.contains("cache write unknown"), "{msg}");
                assert!(msg.contains("reasoning unknown"), "{msg}");
                assert!(!msg.contains("cache write 0"), "{msg}");
            }
            _ => panic!("expected Continue(Some(_)), got {:?}", out),
        }
    }

    #[tokio::test]
    async fn cost_names_the_calls_missing_from_a_partial_session_sum() {
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent();
        agent.session_usage.add(Some(crate::providers::Usage {
            prompt_tokens: Some(10),
            completion_tokens: Some(4),
            total_tokens: Some(14),
            ..crate::providers::Usage::default()
        }));
        agent.session_usage.add(None);
        let out = reg.dispatch("cost", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(msg)) => {
                assert!(msg.contains("session (2 calls): 10 prompt"), "{msg}");
                assert!(msg.contains("cache read unknown"), "{msg}");
                assert!(
                    msg.contains("prompt (1 of 2 calls did not report)"),
                    "{msg}"
                );
                assert!(
                    msg.contains("cache read (2 of 2 calls did not report)"),
                    "{msg}"
                );
            }
            _ => panic!("expected Continue(Some(_)), got {:?}", out),
        }
    }

    #[tokio::test]
    async fn cost_explains_the_context_latency_and_price_of_what_ran() {
        use crate::ledger::{ContextEstimate, Latency};

        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent();
        agent
            .ledger
            .record(
                1,
                ContextEstimate {
                    pinned: 300,
                    tool_schemas: 120,
                    summary: 40,
                    recent: 90,
                    total: 550,
                },
                Some(crate::providers::Usage {
                    prompt_tokens: Some(600),
                    completion_tokens: Some(20),
                    total_tokens: Some(620),
                    ..crate::providers::Usage::default()
                }),
                Latency::default(),
            )
            .await;

        let out = reg.dispatch("cost", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(msg)) => {
                assert!(msg.contains("300 pinned"), "{msg}");
                assert!(msg.contains("120 tool schemas"), "{msg}");
                assert!(msg.contains("40 summary"), "{msg}");
                assert!(msg.contains("= 550 tokens"), "{msg}");
                // Nothing streamed and nothing is priced: both say so.
                assert!(msg.contains("first token unknown"), "{msg}");
                assert!(msg.contains("cost: unknown"), "{msg}");
                assert!(!msg.contains("cost: 0"), "{msg}");
            }
            _ => panic!("expected Continue(Some(_)), got {:?}", out),
        }
    }

    #[tokio::test]
    async fn cost_stays_quiet_about_accounting_before_anything_has_run() {
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent();
        let out = reg.dispatch("cost", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(msg)) => {
                assert!(!msg.contains("context (estimated)"), "{msg}");
                assert!(!msg.contains("cost:"), "{msg}");
            }
            _ => panic!("expected Continue(Some(_)), got {:?}", out),
        }
    }

    #[tokio::test]
    async fn cost_session_line_says_no_usage_when_zero() {
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent();
        let out = reg.dispatch("cost", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(msg)) => {
                assert!(msg.contains("session:"));
                assert!(msg.contains("no usage recorded"));
            }
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn tools_lists_registered_tool_names() {
        let reg = SlashRegistry::default_set();
        let mut tools = crate::tools::Registry::new();
        tools.register(crate::tools::read::Read);
        tools.register(crate::tools::glob::Glob);
        let provider = FakeProvider::new(vec![json!({"role":"assistant","content":"x"})]);
        let mut agent = Agent::new(Box::new(provider), tools, "m".into());

        let out = reg.dispatch("tools", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(msg)) => {
                assert!(msg.contains("Read"));
                assert!(msg.contains("Glob"));
                assert!(msg.contains("Registered tools (2)"));
            }
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn system_shows_pinned_content() {
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent()
            .pin_system_prompt("you are helpful")
            .await
            .unwrap();
        let out = reg.dispatch("system", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(msg)) => {
                assert!(msg.contains("you are helpful"));
                assert!(msg.contains("system"));
            }
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn system_reports_when_nothing_is_pinned() {
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent();
        let out = reg.dispatch("system", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(msg)) => {
                assert!(msg.to_lowercase().contains("no system prompt"));
            }
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn memory_stats_reports_record_and_pinned_counts() {
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent().pin_system_prompt("sys").await.unwrap();
        agent
            .memory
            .record(json!({"role":"user","content":"hi"}))
            .await
            .unwrap();
        let out = reg.dispatch("memory", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(msg)) => {
                assert!(msg.contains("records (logical): 1"));
                assert!(msg.contains("pinned messages:   1"));
            }
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn memory_dump_renders_each_message() {
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent();
        agent
            .memory
            .record(json!({"role":"user","content":"first"}))
            .await
            .unwrap();
        agent
            .memory
            .record(json!({"role":"assistant","content":"second"}))
            .await
            .unwrap();
        let out = reg.dispatch("memory dump", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(msg)) => {
                assert!(msg.contains("[0] user: first"));
                assert!(msg.contains("[1] assistant: second"));
            }
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn provider_lists_configured_entries_when_called_without_args() {
        let reg = SlashRegistry::default_set();
        let cfg = std::sync::Arc::new(crate::config::Config::env_default());
        let provider = FakeProvider::new(vec![json!({"role":"assistant","content":"x"})]);
        let mut agent = Agent::new(Box::new(provider), Registry::new(), "m".into())
            .with_config(cfg, "openrouter");

        let out = reg.dispatch("provider", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(msg)) => {
                assert!(msg.contains("Configured providers"));
                assert!(msg.contains("openrouter"));
                assert!(msg.contains("[active]"));
            }
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn provider_swap_updates_model_and_caps() {
        // Build a config with two providers; swap to the non-default one
        // and verify the agent's model + caps change accordingly.
        let toml = r#"
default_provider = "ollama"
[providers.ollama]
kind          = "openai-compat"
base_url      = "http://localhost:11434/v1"
api_key       = "ollama"
default_model = "qwen2.5-coder:7b"

[providers.cloud]
kind          = "openai-compat"
base_url      = "https://api.example.com/v1"
api_key       = "x"
default_model = "anthropic/claude-haiku-4.5"
"#;
        let cfg = std::sync::Arc::new(crate::config::Config::from_str(toml).unwrap());
        let provider = FakeProvider::new(vec![]);
        let mut agent = Agent::new(
            Box::new(provider),
            Registry::new(),
            "qwen2.5-coder:7b".into(),
        )
        .with_config(cfg, "ollama");

        assert_eq!(agent.model, "qwen2.5-coder:7b");
        assert!(!agent.caps.supports_native_tool_calls);

        let out = reg_dispatch_provider("provider cloud", &mut agent).await;
        assert!(out.contains("switched to provider cloud"));
        assert_eq!(agent.provider_name, "cloud");
        assert_eq!(agent.model, "anthropic/claude-haiku-4.5");
        assert!(agent.caps.supports_native_tool_calls);
    }

    #[tokio::test]
    async fn provider_swap_to_unknown_name_is_error_message_not_panic() {
        let cfg = std::sync::Arc::new(crate::config::Config::env_default());
        let provider = FakeProvider::new(vec![]);
        let mut agent = Agent::new(Box::new(provider), Registry::new(), "m".into())
            .with_config(cfg, "openrouter");
        let out = reg_dispatch_provider("provider missing", &mut agent).await;
        assert!(out.contains("unknown provider"));
    }

    #[tokio::test]
    async fn model_without_args_reports_current_and_lists_when_provider_supports_it() {
        let reg = SlashRegistry::default_set();
        let provider = FakeProvider::new(vec![]);
        let mut agent = Agent::new(Box::new(provider), Registry::new(), "m".into());
        let out = reg.dispatch("model", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(msg)) => {
                assert!(msg.contains("current model: m"));
                // FakeProvider uses the trait default, returns empty —
                // command should mention that gracefully.
                assert!(msg.contains("doesn't expose a model list"));
            }
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn model_swap_recomputes_caps() {
        let reg = SlashRegistry::default_set();
        let provider = FakeProvider::new(vec![]);
        let mut agent = Agent::new(
            Box::new(provider),
            Registry::new(),
            "qwen2.5-coder:7b".into(),
        );
        assert!(!agent.caps.supports_native_tool_calls);

        let out = reg
            .dispatch("model anthropic/claude-haiku-4.5", &mut agent)
            .await
            .unwrap();
        match out {
            SlashOutcome::Continue(Some(msg)) => assert!(msg.contains("model switched")),
            _ => panic!(),
        }
        assert_eq!(agent.model, "anthropic/claude-haiku-4.5");
        assert!(agent.caps.supports_native_tool_calls);
    }

    /// Helper: runs the `/provider` command and pulls the message out so
    /// callers don't need to repeat the SlashOutcome match.
    async fn reg_dispatch_provider(line: &str, agent: &mut Agent) -> String {
        let reg = SlashRegistry::default_set();
        match reg.dispatch(line, agent).await.unwrap() {
            SlashOutcome::Continue(Some(s)) => s,
            other => panic!("unexpected outcome: {:?}", other),
        }
    }

    #[tokio::test]
    async fn memory_unknown_subcommand_returns_helpful_message() {
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent();
        let out = reg.dispatch("memory wat", &mut agent).await.unwrap();
        match out {
            SlashOutcome::Continue(Some(msg)) => {
                assert!(msg.contains("unknown subcommand"));
                assert!(msg.contains("stats"));
                assert!(msg.contains("dump"));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn re_registering_replaces_but_keeps_position() {
        let mut reg = SlashRegistry::new();
        reg.register(Clear);
        reg.register(Clear);
        assert_eq!(reg.iter().count(), 1);
    }

    /// End-to-end `/plugins reload`: an empty agent, with a reloader
    /// pointed at a tempdir containing one plugin, picks up the
    /// plugin's tool, hook, and slash command after invoking
    /// `/plugins reload` — without any restart.
    #[tokio::test]
    async fn plugins_reload_picks_up_a_freshly_dropped_plugin() {
        use crate::plugins::PluginReloader;
        use std::sync::Arc;
        use tempfile::tempdir;

        let plugin_dir = tempdir().unwrap();
        std::fs::write(
            plugin_dir.path().join("hello.lua"),
            r#"
local p = { name = "hello", version = "0.1" }
p.tools = {
  { name = "Greet", description = "say hi",
    parameters = { type = "object", properties = {} },
    execute = function(args, ctx) return "hi" end },
}
p.slash_commands = {
  { name = "wave", description = "wave",
    execute = function(args, ctx) return "🌊" end },
}
p.hooks = {
  pre_tool_use = function(event, ctx) end,
}
return p
            "#,
        )
        .unwrap();

        let host_tools = Arc::new(tokio::sync::Mutex::new(Registry::new()));
        let reloader = Arc::new(PluginReloader::with_dirs(
            host_tools,
            None,
            vec![plugin_dir.path().to_path_buf()],
        ));

        let mut agent = fresh_agent();
        // Sanity: nothing plugin-y yet.
        assert!(agent.plugin_manifest.is_empty());
        assert_eq!(agent.tools.iter().count(), 0);
        assert_eq!(agent.hooks.len(), 0);

        let plugins_cmd = Plugins::new(Some(reloader.clone()));
        let outcome = plugins_cmd.run("reload", &mut agent).await;

        match outcome {
            SlashOutcome::Rebuild {
                removed_names,
                added_slashes,
                message,
            } => {
                // First reload: nothing to remove.
                assert!(removed_names.is_empty());
                // The plugin's slash command came back for the REPL
                // to install.
                assert_eq!(added_slashes.len(), 1);
                assert_eq!(added_slashes[0].name(), "wave");
                assert!(
                    message.contains("1 plugin"),
                    "expected reload summary, got: {}",
                    message
                );
            }
            other => panic!("expected Rebuild, got {:?}", other),
        }

        // Tool, hook, and manifest are all live without any restart.
        assert!(agent.tools.get("Greet").is_some());
        assert_eq!(agent.hooks.len(), 1);
        assert_eq!(agent.plugin_manifest.len(), 1);
        assert_eq!(agent.plugin_manifest[0].name, "hello");
        assert_eq!(agent.plugin_manifest[0].tools, vec!["Greet"]);
    }

    /// A second reload after a plugin file has been deleted on disk
    /// removes the prior plugin's contributions cleanly.
    #[tokio::test]
    async fn plugins_reload_removes_entries_when_file_disappears() {
        use crate::plugins::PluginReloader;
        use std::sync::Arc;
        use tempfile::tempdir;

        let plugin_dir = tempdir().unwrap();
        let plugin_path = plugin_dir.path().join("ephemeral.lua");
        std::fs::write(
            &plugin_path,
            r#"
local p = { name = "ephemeral" }
p.tools = {
  { name = "Boop", description = "",
    parameters = { type = "object", properties = {} },
    execute = function(args, ctx) return "boop" end },
}
p.slash_commands = {
  { name = "boop", description = "boop",
    execute = function(args, ctx) return "boop" end },
}
return p
            "#,
        )
        .unwrap();

        let host_tools = Arc::new(tokio::sync::Mutex::new(Registry::new()));
        let reloader = Arc::new(PluginReloader::with_dirs(
            host_tools,
            None,
            vec![plugin_dir.path().to_path_buf()],
        ));
        let plugins_cmd = Plugins::new(Some(reloader.clone()));
        let mut agent = fresh_agent();

        // First reload: plugin loads.
        let _ = plugins_cmd.run("reload", &mut agent).await;
        assert!(agent.tools.get("Boop").is_some());
        assert_eq!(agent.plugin_manifest.len(), 1);

        // Plugin file disappears between sessions.
        std::fs::remove_file(&plugin_path).unwrap();

        // Second reload: tool + slash names come back as `removed_names`,
        // nothing new to register.
        let outcome = plugins_cmd.run("reload", &mut agent).await;
        match outcome {
            SlashOutcome::Rebuild {
                removed_names,
                added_slashes,
                ..
            } => {
                assert!(
                    removed_names.contains(&"boop".to_string()),
                    "expected `boop` in removed_names, got {:?}",
                    removed_names
                );
                assert!(added_slashes.is_empty());
            }
            other => panic!("expected Rebuild, got {:?}", other),
        }
        assert!(agent.tools.get("Boop").is_none(), "Boop should be gone");
        assert!(agent.plugin_manifest.is_empty());
    }

    #[tokio::test]
    async fn plugins_reload_without_reloader_reports_unavailable() {
        let plugins_cmd = Plugins::new(None);
        let mut agent = fresh_agent();
        let outcome = plugins_cmd.run("reload", &mut agent).await;
        match outcome {
            SlashOutcome::Continue(Some(msg)) => {
                assert!(msg.contains("unavailable"));
            }
            other => panic!("expected Continue with unavailable msg, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn plugins_unknown_subcommand_reports_usage() {
        let plugins_cmd = Plugins::new(None);
        let mut agent = fresh_agent();
        let outcome = plugins_cmd.run("frobnicate", &mut agent).await;
        match outcome {
            SlashOutcome::Continue(Some(msg)) => {
                assert!(msg.contains("unknown /plugins subcommand"));
                assert!(msg.contains("reload"));
            }
            other => panic!("expected Continue, got {:?}", other),
        }
    }

    #[test]
    fn paths_is_registered_in_default_set() {
        let reg = SlashRegistry::default_set();
        assert!(
            reg.get("paths").is_some(),
            "default set should include /paths"
        );
    }

    #[tokio::test]
    async fn paths_renders_section_headers_and_active_model() {
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent();
        agent.provider_name = "ollama".into();

        let out = reg.dispatch("paths", &mut agent).await.unwrap();
        let body = match out {
            SlashOutcome::Continue(Some(msg)) => msg,
            other => panic!("expected Continue, got {:?}", other),
        };

        // Section headers anchor the layout — keep them stable so the
        // model can quote them when explaining customization to users.
        for header in &["# Working", "# Customization", "# Extensions", "# State"] {
            assert!(
                body.contains(header),
                "missing section header `{}` in:\n{}",
                header,
                body
            );
        }
        // Runtime fields surface so the user sees what the rest is
        // relative to.
        assert!(body.contains("active provider:"));
        assert!(body.contains("ollama"));
        assert!(body.contains("active model:"));
        // Customization filenames the docs reference.
        for filename in &["config.toml", "AGENTS.md", "CLAUDE.md"] {
            assert!(
                body.contains(filename),
                "missing customization filename `{}` in:\n{}",
                filename,
                body
            );
        }
    }

    #[tokio::test]
    async fn paths_marks_definitely_missing_files_as_not_present() {
        // We cannot sandbox every user-level path, but can assert that
        // any listed path absent on disk gets the marker.
        let reg = SlashRegistry::default_set();
        let mut agent = fresh_agent();
        let out = reg.dispatch("paths", &mut agent).await.unwrap();
        let body = match out {
            SlashOutcome::Continue(Some(msg)) => msg,
            other => panic!("expected Continue, got {:?}", other),
        };

        for line in body.lines() {
            // Section headers and the walk-up note don't carry paths.
            if !line.starts_with("  ") || line.trim_start().starts_with('(') {
                continue;
            }
            // Lines have shape `  Label:           /abs/path[/]  (not present)?`
            // — split on whitespace and grab the path token (last
            // non-marker word). If the path doesn't exist on disk,
            // the line MUST end with the marker.
            let trimmed = line.trim_end();
            let already_marked = trimmed.ends_with("(not present)");
            // Heuristic: extract the substring after the label colon.
            let Some(colon) = line.find(':') else {
                continue;
            };
            let rest = line[colon + 1..].trim();
            // Skip the env-missing fallback message.
            if rest.starts_with('(') {
                continue;
            }
            let path_str = rest
                .strip_suffix("(not present)")
                .unwrap_or(rest)
                .trim()
                .trim_end_matches('/');
            // Only filesystem paths get the marker. The Working
            // section's `active provider:` / `active model:` rows
            // hold short identifiers, not paths — skip them.
            if !path_str.starts_with('/') {
                continue;
            }
            let p = std::path::Path::new(path_str);
            if !p.exists() {
                assert!(
                    already_marked,
                    "non-existent path lacks `(not present)` marker:\n  line: {}",
                    line
                );
            }
        }
    }
}
