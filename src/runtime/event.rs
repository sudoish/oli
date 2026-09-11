use serde::{Deserialize, Serialize};
use serde_json::Value;

/// An owned observation emitted while a frontend command is running.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeEvent {
    Content {
        text: String,
    },
    ToolCallDelta {
        provider_tool_id: String,
        name: String,
        partial_json: String,
        accumulated_json: String,
    },
    ToolStarted {
        name: String,
        args: Value,
    },
    Completed {
        response: String,
    },
    MaxTurnsExhausted {
        limit: usize,
        message: String,
    },
    Cancelled,
    Error {
        message: String,
    },
}
