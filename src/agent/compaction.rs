use std::time::Instant;

use crate::agent::{Agent, CompactContext, CompactionReport};
use crate::error::{AgentError, Result};
use crate::ledger::{CompactionMetrics, ContextEstimate, as_ms, estimate_context};
use crate::providers::ChatRequest;

const MAX_PASSES: usize = 8;

pub(crate) struct PreparedRequest {
    pub(crate) request: ChatRequest,
    pub(crate) estimated: ContextEstimate,
    pub(crate) compaction_ms: u64,
}

pub(crate) async fn prepare(agent: &mut Agent, turn: u32) -> Result<PreparedRequest> {
    let tool_schemas = agent.tools.openai_schemas();
    let mut parts = agent.memory.snapshot_parts().await;
    let mut estimated = estimate_context(&parts, &tool_schemas);
    let budget = agent.request_budget();
    let compaction_hard_limit = budget
        .hard_limit_is_authoritative
        .then_some(budget.hard_limit_tokens)
        .unwrap_or(usize::MAX);
    let compact_started = Instant::now();

    for _ in 0..MAX_PASSES {
        let compaction = match agent
            .memory
            .maybe_compact(CompactContext {
                provider: agent.provider.as_ref(),
                model: &agent.model,
                target_tokens: budget.target_tokens,
                hard_limit_tokens: compaction_hard_limit,
                next_request_tokens: usize::try_from(estimated.total).unwrap_or(usize::MAX),
            })
            .await
        {
            Ok(report) => report,
            Err(error) => {
                crate::log_warn!(
                    "compaction failed; retaining the unchanged request context: {error}"
                );
                break;
            }
        };
        let Some(report) = compaction else {
            break;
        };
        parts = agent.memory.snapshot_parts().await;
        let after = estimate_context(&parts, &tool_schemas);
        let metrics = CompactionMetrics::new(
            estimated,
            after,
            budget.target_tokens,
            budget.hard_limit_tokens,
            report.estimated,
            report.usage,
        );
        record(agent, turn, report, metrics).await;
        if after.total >= estimated.total || after.total <= budget.target_tokens as u64 {
            estimated = after;
            break;
        }
        estimated = after;
    }

    let compaction_ms = as_ms(compact_started.elapsed());
    if budget.hard_limit_is_authoritative && estimated.total > budget.hard_limit_tokens as u64 {
        return Err(AgentError::Config(format!(
            "materialized request estimate {} exceeds hard context limit {} after compaction",
            estimated.total, budget.hard_limit_tokens
        )));
    }

    Ok(PreparedRequest {
        request: ChatRequest {
            model: agent.model.clone(),
            messages: parts.flatten(),
            tools: tool_schemas,
        },
        estimated,
        compaction_ms,
    })
}

pub(crate) async fn force(agent: &mut Agent) -> Result<()> {
    let parts = agent.memory.snapshot_parts().await;
    let tool_schemas = agent.tools.openai_schemas();
    let before = estimate_context(&parts, &tool_schemas);
    let budget = agent.request_budget();
    let compaction_hard_limit = budget
        .hard_limit_is_authoritative
        .then_some(budget.hard_limit_tokens)
        .unwrap_or(usize::MAX);
    let report = agent
        .memory
        .maybe_compact(CompactContext {
            provider: agent.provider.as_ref(),
            model: &agent.model,
            target_tokens: 0,
            hard_limit_tokens: compaction_hard_limit,
            next_request_tokens: usize::try_from(before.total).unwrap_or(usize::MAX),
        })
        .await?;
    if let Some(report) = report {
        let after = estimate_context(&agent.memory.snapshot_parts().await, &tool_schemas);
        let metrics = CompactionMetrics::new(
            before,
            after,
            0,
            budget.hard_limit_tokens,
            report.estimated,
            report.usage,
        );
        let turn = agent.ledger.summary().turns.saturating_add(1);
        record(agent, turn, report, metrics).await;
    }
    Ok(())
}

async fn record(
    agent: &mut Agent,
    turn: u32,
    report: CompactionReport,
    metrics: CompactionMetrics,
) {
    agent.session_usage.add(report.usage);
    if let Some(usage) = report.usage {
        agent.last_usage = Some(usage);
    }
    agent
        .ledger
        .record_compaction_started(
            report.started_at_ms,
            turn,
            metrics,
            report.estimated,
            report.usage,
            report.latency,
        )
        .await;
}

pub(crate) fn context_build_latency(
    started: Instant,
    compaction_ms: u64,
) -> crate::ledger::Latency {
    crate::ledger::Latency {
        context_build_ms: as_ms(started.elapsed()),
        compaction_ms,
        ..crate::ledger::Latency::default()
    }
}
