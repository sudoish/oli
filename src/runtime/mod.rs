//! Frontend-neutral ownership boundary for one agent session.

mod event;
mod snapshot;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::{Notify, mpsc};

use crate::agent::{Agent, RunOutcome};
use crate::error::Result;
use crate::hooks::{Hook, HookOutcome, HookPayload};
use crate::mcp::HealthState;
use crate::providers::StreamEvent;

pub use event::RuntimeEvent;
pub use snapshot::{
    McpHealthSnapshot, McpSnapshot, RunState, SessionSnapshot, TokenTotalSnapshot, UsageSnapshot,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeCommand {
    Prompt(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandOutcome {
    Completed(String),
    MaxTurnsExhausted { limit: usize, message: String },
    Cancelled,
}

/// A one-shot cancellation signal that may be triggered from another task.
#[derive(Clone, Default)]
pub struct CancellationToken {
    inner: Arc<CancellationState>,
}

#[derive(Default)]
struct CancellationState {
    cancelled: AtomicBool,
    notify: Notify,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        if !self.inner.cancelled.swap(true, Ordering::SeqCst) {
            self.inner.notify.notify_one();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    async fn cancelled(&self) {
        let notified = self.inner.notify.notified();
        if self.is_cancelled() {
            return;
        }
        notified.await;
    }
}

type EventSender = mpsc::UnboundedSender<RuntimeEvent>;

struct RuntimeProgressHook {
    sender: Arc<Mutex<Option<EventSender>>>,
}

#[async_trait]
impl Hook for RuntimeProgressHook {
    fn name(&self) -> &str {
        "runtime-events"
    }

    async fn handle(&self, payload: &HookPayload<'_>) -> HookOutcome {
        let event = match payload {
            HookPayload::PreToolUse { tool, args } => Some(RuntimeEvent::ToolStarted {
                name: (*tool).to_string(),
                args: (*args).clone(),
            }),
            HookPayload::PostToolUse { .. } | HookPayload::Stop { .. } => None,
        };
        if let Some(event) = event {
            if let Some(sender) = self
                .sender
                .lock()
                .expect("runtime event lock poisoned")
                .as_ref()
            {
                let _ = sender.send(event);
            }
        }
        HookOutcome::Continue
    }
}

/// Owns a single [`Agent`] and translates its borrowed stream into owned events.
pub struct SessionRuntime {
    agent: Agent,
    session_id: String,
    run_state: RunState,
    event_sender: Arc<Mutex<Option<EventSender>>>,
}

impl SessionRuntime {
    pub fn new(mut agent: Agent, session_id: impl Into<String>) -> Self {
        let event_sender = Arc::new(Mutex::new(None));
        agent.hooks.register(RuntimeProgressHook {
            sender: event_sender.clone(),
        });
        Self {
            agent,
            session_id: session_id.into(),
            run_state: RunState::Idle,
            event_sender,
        }
    }

    /// Transitional access for the line frontend's existing slash-command API.
    pub fn agent_mut(&mut self) -> &mut Agent {
        &mut self.agent
    }

    pub async fn finish(&self) -> crate::ledger::RunSummary {
        self.agent.ledger.finish().await
    }

    pub async fn snapshot(&self) -> SessionSnapshot {
        let mut mcp = Vec::with_capacity(self.agent.mcp_handles.len());
        for handle in self.agent.mcp_handles.iter() {
            let server = handle.server.lock().await;
            let health = match &server.health {
                HealthState::Healthy => McpHealthSnapshot::Healthy,
                HealthState::Down(reason) => McpHealthSnapshot::Down {
                    reason: reason.clone(),
                },
            };
            mcp.push(McpSnapshot {
                name: handle.name.clone(),
                health,
                tool_count: server.tools.len(),
            });
        }
        SessionSnapshot {
            session_id: self.session_id.clone(),
            run_state: self.run_state,
            provider: (!self.agent.provider_name.is_empty())
                .then(|| self.agent.provider_name.clone()),
            model: self.agent.model.clone(),
            tools: self
                .agent
                .tools
                .iter()
                .map(|tool| tool.name().to_string())
                .collect(),
            mcp,
            usage: self.agent.session_usage.into(),
            accounting: self.agent.ledger.summary(),
        }
    }

    pub async fn execute<F>(
        &mut self,
        command: RuntimeCommand,
        cancellation: &CancellationToken,
        mut sink: F,
    ) -> Result<CommandOutcome>
    where
        F: FnMut(RuntimeEvent) + Send,
    {
        let RuntimeCommand::Prompt(prompt) = command;
        let saved_len = self.agent.memory.len();
        let (sender, mut receiver) = mpsc::unbounded_channel();
        *self
            .event_sender
            .lock()
            .expect("runtime event lock poisoned") = Some(sender.clone());
        self.run_state = RunState::Running;

        let result = {
            let stream_sender = sender.clone();
            let mut stream_sink = move |event: StreamEvent<'_>| {
                let owned = match event {
                    StreamEvent::Content(text) => RuntimeEvent::Content {
                        text: text.to_string(),
                    },
                    StreamEvent::ToolArgsChunk {
                        provider_tool_id,
                        name,
                        partial_json,
                        accumulated_json,
                    } => RuntimeEvent::ToolCallDelta {
                        provider_tool_id: provider_tool_id.to_string(),
                        name: name.to_string(),
                        partial_json: partial_json.to_string(),
                        accumulated_json: accumulated_json.to_string(),
                    },
                };
                let _ = stream_sender.send(owned);
            };
            let run = self.agent.run_streaming(&prompt, &mut stream_sink);
            tokio::pin!(run);
            loop {
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => break None,
                    event = receiver.recv() => {
                        if let Some(event) = event {
                            sink(event);
                        }
                    }
                    result = &mut run => break Some(result),
                }
            }
        };

        *self
            .event_sender
            .lock()
            .expect("runtime event lock poisoned") = None;
        drop(sender);
        while let Ok(event) = receiver.try_recv() {
            sink(event);
        }

        let Some(result) = result else {
            if let Err(error) = self.agent.memory.truncate(saved_len).await {
                sink(RuntimeEvent::Error {
                    message: format!("failed to truncate memory on cancel: {error}"),
                });
            }
            self.run_state = RunState::Cancelled;
            sink(RuntimeEvent::Cancelled);
            return Ok(CommandOutcome::Cancelled);
        };

        match result {
            Ok(RunOutcome::Completed(response)) => {
                self.run_state = RunState::Completed;
                sink(RuntimeEvent::Completed {
                    response: response.clone(),
                });
                Ok(CommandOutcome::Completed(response))
            }
            Ok(RunOutcome::MaxTurnsExhausted { limit, message }) => {
                self.run_state = RunState::MaxTurnsExhausted;
                sink(RuntimeEvent::MaxTurnsExhausted {
                    limit,
                    message: message.clone(),
                });
                Ok(CommandOutcome::MaxTurnsExhausted { limit, message })
            }
            Err(error) => {
                self.run_state = RunState::Failed;
                sink(RuntimeEvent::Error {
                    message: error.to_string(),
                });
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future;

    use async_trait::async_trait;
    use serde_json::json;

    use super::*;
    use crate::providers::fake::FakeProvider;
    use crate::providers::{ChatRequest, ChatResponse, Provider, StreamSink, Usage};
    use crate::tools::{Registry, Tool, ToolContext};

    fn test_runtime(provider: Box<dyn Provider>) -> SessionRuntime {
        SessionRuntime::new(
            Agent::new(provider, Registry::new(), "test-model".into()),
            "s1",
        )
    }

    #[tokio::test]
    async fn prompt_emits_owned_content_before_completion() {
        fn assert_owned<T: Send + 'static>() {}
        assert_owned::<RuntimeEvent>();

        let provider = FakeProvider::new(vec![json!({"role":"assistant","content":"hello"})]);
        let mut runtime = test_runtime(Box::new(provider));
        let mut events = Vec::new();
        let outcome = runtime
            .execute(
                RuntimeCommand::Prompt("hi".into()),
                &CancellationToken::new(),
                |event| events.push(event),
            )
            .await
            .unwrap();

        assert_eq!(outcome, CommandOutcome::Completed("hello".into()));
        assert_eq!(
            events,
            vec![
                RuntimeEvent::Content { text: "he".into() },
                RuntimeEvent::Content { text: "llo".into() },
                RuntimeEvent::Completed {
                    response: "hello".into()
                },
            ]
        );
    }

    struct ToolDeltaProvider;

    #[async_trait]
    impl Provider for ToolDeltaProvider {
        async fn chat(&self, _: ChatRequest) -> Result<ChatResponse> {
            unreachable!("streaming path is required")
        }

        async fn chat_stream(&self, _: ChatRequest, sink: StreamSink<'_>) -> Result<ChatResponse> {
            let id = "call-1".to_string();
            let partial = "{\"x\":".to_string();
            sink(StreamEvent::ToolArgsChunk {
                provider_tool_id: &id,
                name: "Echo",
                partial_json: &partial,
                accumulated_json: &partial,
            });
            Ok(ChatResponse {
                message: json!({"role":"assistant","content":"ok"}),
                usage: None,
            })
        }
    }

    #[tokio::test]
    async fn borrowed_tool_deltas_are_owned_at_the_runtime_boundary() {
        let mut runtime = test_runtime(Box::new(ToolDeltaProvider));
        let mut events = Vec::new();
        runtime
            .execute(
                RuntimeCommand::Prompt("hi".into()),
                &CancellationToken::new(),
                |event| events.push(event),
            )
            .await
            .unwrap();

        assert!(matches!(
            &events[0],
            RuntimeEvent::ToolCallDelta {
                provider_tool_id,
                partial_json,
                ..
            } if provider_tool_id == "call-1" && partial_json == "{\"x\":"
        ));
    }

    #[tokio::test]
    async fn snapshot_reports_authoritative_session_tools_usage_and_accounting() {
        let provider = FakeProvider::with_usage(vec![(
            json!({"role":"assistant","content":"ok"}),
            Some(Usage {
                total_tokens: Some(7),
                ..Usage::default()
            }),
        )]);
        let mut runtime = test_runtime(Box::new(provider));
        runtime
            .execute(
                RuntimeCommand::Prompt("hi".into()),
                &CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();

        let snapshot = runtime.snapshot().await;
        assert_eq!(snapshot.session_id, "s1");
        assert_eq!(snapshot.run_state, RunState::Completed);
        assert_eq!(snapshot.provider, None);
        assert_eq!(snapshot.model, "test-model");
        assert!(snapshot.tools.is_empty());
        assert_eq!(snapshot.usage.calls, 1);
        assert_eq!(snapshot.usage.total_tokens.reported, Some(7));
        assert_eq!(snapshot.accounting.calls, 1);
    }

    struct Echo;

    #[async_trait]
    impl Tool for Echo {
        fn name(&self) -> &str {
            "Echo"
        }

        fn description(&self) -> &str {
            "echo"
        }

        fn parameters(&self) -> serde_json::Value {
            json!({"type":"object"})
        }

        async fn run(&self, args: serde_json::Value, _: &ToolContext) -> Result<String> {
            Ok(args["text"].as_str().unwrap_or_default().to_string())
        }
    }

    #[tokio::test]
    async fn tool_progress_is_ordered_between_streaming_and_completion() {
        let provider = FakeProvider::new(vec![
            json!({
                "role":"assistant",
                "content":null,
                "tool_calls":[{
                    "id":"call-1",
                    "type":"function",
                    "function":{"name":"Echo","arguments":"{\"text\":\"hi\"}"}
                }]
            }),
            json!({"role":"assistant","content":"done"}),
        ]);
        let mut tools = Registry::new();
        tools.register(Echo);
        let agent = Agent::new(Box::new(provider), tools, "test-model".into());
        let mut runtime = SessionRuntime::new(agent, "s1");
        let mut events = Vec::new();

        runtime
            .execute(
                RuntimeCommand::Prompt("use echo".into()),
                &CancellationToken::new(),
                |event| events.push(event),
            )
            .await
            .unwrap();

        let started = events
            .iter()
            .position(
                |event| matches!(event, RuntimeEvent::ToolStarted { name, .. } if name == "Echo"),
            )
            .unwrap();
        let content = events
            .iter()
            .position(|event| matches!(event, RuntimeEvent::Content { .. }))
            .unwrap();
        let completed = events
            .iter()
            .position(|event| matches!(event, RuntimeEvent::Completed { .. }))
            .unwrap();
        assert!(started < content);
        assert!(content < completed);
    }

    #[tokio::test]
    async fn max_turns_and_errors_emit_typed_terminal_events() {
        let tool_call = json!({
            "role":"assistant",
            "content":null,
            "tool_calls":[{
                "id":"call-1",
                "type":"function",
                "function":{"name":"Echo","arguments":"{}"}
            }]
        });
        let mut tools = Registry::new();
        tools.register(Echo);
        let agent = Agent::new(
            Box::new(FakeProvider::new(vec![tool_call])),
            tools,
            "m".into(),
        )
        .with_max_turns(1);
        let mut runtime = SessionRuntime::new(agent, "s1");
        let mut events = Vec::new();
        let outcome = runtime
            .execute(
                RuntimeCommand::Prompt("loop".into()),
                &CancellationToken::new(),
                |event| events.push(event),
            )
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            CommandOutcome::MaxTurnsExhausted { limit: 1, .. }
        ));
        assert!(matches!(
            events.last(),
            Some(RuntimeEvent::MaxTurnsExhausted { limit: 1, .. })
        ));

        let mut failed = test_runtime(Box::new(FakeProvider::new(Vec::new())));
        let mut error_events = Vec::new();
        assert!(
            failed
                .execute(
                    RuntimeCommand::Prompt("fail".into()),
                    &CancellationToken::new(),
                    |event| error_events.push(event),
                )
                .await
                .is_err()
        );
        assert!(matches!(
            error_events.as_slice(),
            [RuntimeEvent::Error { message }] if message.contains("FakeProvider exhausted")
        ));
        assert_eq!(failed.snapshot().await.run_state, RunState::Failed);
    }

    struct PendingProvider;

    #[async_trait]
    impl Provider for PendingProvider {
        async fn chat(&self, _: ChatRequest) -> Result<ChatResponse> {
            future::pending().await
        }

        async fn chat_stream(&self, _: ChatRequest, _: StreamSink<'_>) -> Result<ChatResponse> {
            future::pending().await
        }
    }

    #[tokio::test]
    async fn cancellation_rolls_memory_back_and_emits_cancelled() {
        let mut runtime = test_runtime(Box::new(PendingProvider));
        let cancellation = CancellationToken::new();
        let trigger = cancellation.clone();
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            trigger.cancel();
        });
        let mut events = Vec::new();

        let outcome = runtime
            .execute(
                RuntimeCommand::Prompt("discard me".into()),
                &cancellation,
                |event| events.push(event),
            )
            .await
            .unwrap();

        assert_eq!(outcome, CommandOutcome::Cancelled);
        assert_eq!(events, vec![RuntimeEvent::Cancelled]);
        assert_eq!(runtime.agent.memory.len(), 0);
        assert_eq!(runtime.snapshot().await.run_state, RunState::Cancelled);
    }
}
