use std::time::Instant;

use serde_json::{Value, json};

use crate::agent::{Agent, RunOutcome, compaction, streaming, tool_exec};
use crate::error::Result;
use crate::ledger::{as_ms, now_ms};
use crate::providers::StreamEvent;

pub(crate) async fn run<F>(agent: &mut Agent, prompt: &str, sink: &mut F) -> Result<RunOutcome>
where
    F: FnMut(StreamEvent<'_>) + Send,
{
    agent
        .memory
        .record(json!({ "role": "user", "content": prompt }))
        .await?;

    let first_turn = agent.ledger.next_turn();
    let mut invocation_turn = 0usize;
    loop {
        if let Some(cap) = agent.max_turns {
            if invocation_turn >= cap {
                let message = agent
                    .hooks
                    .dispatch_stop(format!("(max_turns reached: {})", cap))
                    .await;
                return Ok(RunOutcome::MaxTurnsExhausted {
                    limit: cap,
                    message,
                });
            }
        }
        let turn = first_turn.saturating_add(invocation_turn as u32);
        invocation_turn += 1;

        let build_started = Instant::now();
        refresh_mcp_tools(agent).await;
        let prepared = compaction::prepare(agent, turn).await?;
        let mut latency = compaction::context_build_latency(build_started, prepared.compaction_ms);

        let request_started_at_ms = now_ms();
        let response =
            streaming::request(agent.provider.as_ref(), prepared.request, agent.caps, sink).await?;
        latency.model_ms = response.model_ms;
        latency.ttft_ms = response.ttft_ms;
        agent.session_usage.add(response.usage);
        if let Some(usage) = response.usage {
            agent.last_usage = Some(usage);
        }

        agent.memory.record(response.message.clone()).await?;

        if response.tool_calls.is_empty() {
            agent
                .ledger
                .record_started(
                    request_started_at_ms,
                    turn,
                    prepared.estimated,
                    response.usage,
                    latency,
                )
                .await;
            let content = response
                .message
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let content = agent.hooks.dispatch_stop(content).await;
            return Ok(RunOutcome::Completed(content));
        }

        let tools_started = Instant::now();
        for call in &response.tool_calls {
            let id = call.get("id").and_then(Value::as_str).unwrap_or("");
            let name = call
                .get("function")
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let args = call
                .get("function")
                .and_then(|function| function.get("arguments"))
                .and_then(Value::as_str)
                .unwrap_or("{}");
            let args = serde_json::from_str(args).unwrap_or_else(|_| json!({}));
            let result = tool_exec::execute(
                &agent.tools,
                &agent.ctx,
                agent.policy.as_ref(),
                &agent.hooks,
                agent.tool_started_observer.as_deref(),
                name,
                args,
            )
            .await;
            agent
                .memory
                .record(json!({
                    "role": "tool",
                    "tool_call_id": id,
                    "content": result,
                }))
                .await?;
        }
        latency.tool_ms = as_ms(tools_started.elapsed());
        agent
            .ledger
            .record_started(
                request_started_at_ms,
                turn,
                prepared.estimated,
                response.usage,
                latency,
            )
            .await;
    }
}

async fn refresh_mcp_tools(agent: &mut Agent) {
    if agent.mcp_handles.is_empty() {
        return;
    }
    let deltas = crate::mcp::refresh_changed_tools(agent.mcp_handles.as_ref()).await;
    for delta in deltas {
        for name in delta.removed {
            agent.tools.remove(&name);
        }
        for tool in delta.added {
            agent.tools.register_box(tool);
        }
    }
}
