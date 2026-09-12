use super::{SlashCommand, SlashOutcome};
use crate::agent::Agent;
use async_trait::async_trait;

use super::cost::render_tokens;

pub struct MemoryCmd;

#[async_trait]
impl SlashCommand for MemoryCmd {
    fn name(&self) -> &str {
        "memory"
    }
    fn description(&self) -> &str {
        "memory state — `stats` (default) or `dump`"
    }
    async fn run(&self, args: &str, agent: &mut Agent) -> SlashOutcome {
        let sub = args.trim();
        match sub {
            "" | "stats" => {
                let pinned = agent.memory.pinned().await.len();
                let recorded = agent.memory.len();
                let snap_len = agent.memory.snapshot().await.len();
                let synthesized = snap_len.saturating_sub(pinned + recorded);
                let mut out = format!(
                    "records (logical): {}\npinned messages:   {}\nsynthesized:       {} (compaction summary)",
                    recorded, pinned, synthesized
                );
                if let Some(u) = agent.last_usage {
                    out.push_str(&format!(
                        "\nlast prompt tokens: {}",
                        render_tokens(u.prompt_tokens)
                    ));
                }
                SlashOutcome::Continue(Some(out))
            }
            "dump" => {
                let snap = agent.memory.snapshot().await;
                let mut out = format!("snapshot ({} messages):\n", snap.len());
                for (i, m) in snap.iter().enumerate() {
                    let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("?");
                    let preview = render_message_preview(m);
                    out.push_str(&format!("  [{}] {}: {}\n", i, role, preview));
                }
                SlashOutcome::Continue(Some(out.trim_end().to_string()))
            }
            other => SlashOutcome::Continue(Some(format!(
                "unknown subcommand: {} (try `stats` or `dump`)",
                other
            ))),
        }
    }
}

pub struct Compact;

#[async_trait]
impl SlashCommand for Compact {
    fn name(&self) -> &str {
        "compact"
    }
    fn description(&self) -> &str {
        "force a compaction pass on memory now"
    }
    async fn run(&self, _args: &str, agent: &mut Agent) -> SlashOutcome {
        let before = agent.memory.snapshot().await.len();
        match agent.force_compact().await {
            Ok(()) => {
                let after = agent.memory.snapshot().await.len();
                let msg = if after < before {
                    format!("compacted: {} → {} messages in snapshot", before, after)
                } else {
                    "compaction declined (not enough live messages, or model returned empty summary)"
                        .into()
                };
                SlashOutcome::Continue(Some(msg))
            }
            Err(e) => SlashOutcome::Continue(Some(format!("compact failed: {}", e))),
        }
    }
}

/// One-line preview of a snapshot message for `/memory dump`.
fn render_message_preview(m: &serde_json::Value) -> String {
    if let Some(content) = m.get("content").and_then(|v| v.as_str()) {
        if !content.is_empty() {
            return truncate_for_preview(content);
        }
    }
    if let Some(tcs) = m.get("tool_calls").and_then(|v| v.as_array()) {
        let names: Vec<&str> = tcs
            .iter()
            .filter_map(|t| {
                t.get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
            })
            .collect();
        return format!("(tool_calls: {})", names.join(", "));
    }
    "(empty)".into()
}

fn truncate_for_preview(s: &str) -> String {
    const LIMIT: usize = 100;
    let one_line = s.replace('\n', " ");
    if one_line.len() <= LIMIT {
        return one_line;
    }
    let mut cut = LIMIT;
    while cut > 0 && !one_line.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &one_line[..cut])
}
