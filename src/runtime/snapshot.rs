use serde::{Deserialize, Serialize};

use crate::ledger::RunSummary;
use crate::providers::UsageTotals;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    #[default]
    Idle,
    Running,
    Completed,
    MaxTurnsExhausted,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum McpHealthSnapshot {
    Healthy,
    Down { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpSnapshot {
    pub name: String,
    pub health: McpHealthSnapshot,
    pub tool_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenTotalSnapshot {
    pub reported: Option<u64>,
    pub unreported_calls: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageSnapshot {
    pub calls: u32,
    pub prompt_tokens: TokenTotalSnapshot,
    pub completion_tokens: TokenTotalSnapshot,
    pub total_tokens: TokenTotalSnapshot,
    pub cache_read_tokens: TokenTotalSnapshot,
    pub cache_write_tokens: TokenTotalSnapshot,
    pub reasoning_tokens: TokenTotalSnapshot,
}

impl UsageSnapshot {
    pub fn any_reported(&self) -> bool {
        [
            &self.prompt_tokens,
            &self.completion_tokens,
            &self.total_tokens,
            &self.cache_read_tokens,
            &self.cache_write_tokens,
            &self.reasoning_tokens,
        ]
        .iter()
        .any(|total| total.reported.is_some())
    }
}

impl From<UsageTotals> for UsageSnapshot {
    fn from(usage: UsageTotals) -> Self {
        let token = |total: crate::providers::TokenTotal| TokenTotalSnapshot {
            reported: total.reported(),
            unreported_calls: total.unreported_calls,
        };
        Self {
            calls: usage.calls,
            prompt_tokens: token(usage.prompt_tokens),
            completion_tokens: token(usage.completion_tokens),
            total_tokens: token(usage.total_tokens),
            cache_read_tokens: token(usage.cache_read_tokens),
            cache_write_tokens: token(usage.cache_write_tokens),
            reasoning_tokens: token(usage.reasoning_tokens),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub session_id: String,
    pub run_state: RunState,
    pub provider: Option<String>,
    pub model: String,
    pub tools: Vec<String>,
    pub mcp: Vec<McpSnapshot>,
    pub usage: UsageSnapshot,
    pub accounting: RunSummary,
}
