use super::{SlashCommand, SlashOutcome};
use crate::agent::Agent;
use async_trait::async_trait;

pub struct Cost;

#[async_trait]
impl SlashCommand for Cost {
    fn name(&self) -> &str {
        "cost"
    }
    fn description(&self) -> &str {
        "show last call + session-total token usage"
    }
    async fn run(&self, _args: &str, agent: &mut Agent) -> SlashOutcome {
        let mut lines: Vec<String> = Vec::new();
        match agent.last_usage {
            Some(u) => {
                lines.push(format!(
                    "last call: {} prompt + {} completion = {} tokens",
                    render_tokens(u.prompt_tokens),
                    render_tokens(u.completion_tokens),
                    render_tokens(u.total_tokens)
                ));
                lines.push(format!(
                    "last call: cache read {}, cache write {}, reasoning {}",
                    render_tokens(u.cache_read_tokens),
                    render_tokens(u.cache_write_tokens),
                    render_tokens(u.reasoning_tokens)
                ));
            }
            None => lines.push("last call: (no usage yet — provider may not report it)".into()),
        }

        let s = agent.session_usage;
        if s.calls == 0 {
            lines.push("session: (no usage recorded yet)".into());
        } else {
            lines.push(format!(
                "session ({} {}): {} prompt + {} completion = {} tokens",
                s.calls,
                if s.calls == 1 { "call" } else { "calls" },
                render_total(&s.prompt_tokens),
                render_total(&s.completion_tokens),
                render_total(&s.total_tokens)
            ));
            lines.push(format!(
                "session: cache read {}, cache write {}, reasoning {}",
                render_total(&s.cache_read_tokens),
                render_total(&s.cache_write_tokens),
                render_total(&s.reasoning_tokens)
            ));
            if let Some(gaps) = unreported_line(&s) {
                lines.push(gaps);
            }
        }

        lines.extend(accounting_lines(agent));
        SlashOutcome::Continue(Some(lines.join("\n")))
    }
}

/// The measured half of `/cost`: where the context went, where the time
/// went, and what it cost. Silent until a request has actually been
/// accounted for.
fn accounting_lines(agent: &crate::agent::Agent) -> Vec<String> {
    let s = agent.ledger.summary();
    if s.calls == 0 {
        return Vec::new();
    }
    let mut lines = vec![
        format!(
            "context (estimated, since process start): {} pinned + {} tool schemas + {} summary + {} recent = {} tokens",
            s.context.pinned,
            s.context.tool_schemas,
            s.context.summary,
            s.context.recent,
            s.context.total
        ),
        format!(
            "latency: {} ms model, {} ms tools, {} ms context build (of which {} ms compaction), first token {}",
            s.latency_ms.model_ms,
            s.latency_ms.tool_ms,
            s.latency_ms.context_build_ms,
            s.latency_ms.compaction_ms,
            match s.latency_ms.best_ttft_ms {
                Some(ms) => format!("{ms} ms"),
                None => "unknown".into(),
            }
        ),
        match s.cost.amount {
            Some(amount) => format!(
                "cost: {:.6} {} over {} of {} calls",
                amount,
                s.cost.currency.as_deref().unwrap_or(""),
                s.cost.priced_calls,
                s.calls
            ),
            None => format!("cost: unknown — {}", s.cost.unknown.join("; ")),
        },
    ];
    if let Some(path) = agent.ledger.path() {
        lines.push(format!("per-request detail: {}", path.display()));
    }
    lines
}

pub(super) fn render_tokens(v: Option<u32>) -> String {
    match v {
        Some(n) => n.to_string(),
        None => "unknown".into(),
    }
}

fn render_total(t: &crate::providers::TokenTotal) -> String {
    match t.reported() {
        Some(n) => n.to_string(),
        None => "unknown".into(),
    }
}

/// Names the categories whose session sums are incomplete. Without it a
/// partial sum reads as a full one.
fn unreported_line(s: &crate::providers::UsageTotals) -> Option<String> {
    let gaps: Vec<String> = [
        ("prompt", &s.prompt_tokens),
        ("completion", &s.completion_tokens),
        ("total", &s.total_tokens),
        ("cache read", &s.cache_read_tokens),
        ("cache write", &s.cache_write_tokens),
        ("reasoning", &s.reasoning_tokens),
    ]
    .into_iter()
    .filter(|(_, t)| t.unreported_calls > 0)
    .map(|(name, t)| {
        format!(
            "{} ({} of {} calls did not report)",
            name, t.unreported_calls, s.calls
        )
    })
    .collect();
    (!gaps.is_empty()).then(|| format!("incomplete: {}", gaps.join(", ")))
}
