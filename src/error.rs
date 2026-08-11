//! Error types — `AgentError` covers everything that can fail at
//! the agent / harness boundary, from provider HTTP errors and
//! tool I/O to config parsing and policy denials. `Result<T>` is
//! the crate-wide alias.
//!
//! `ToolError` is a thin wrapper used by `Tool::call` impls so a
//! tool can fail without having to teach `AgentError` about every
//! tool-specific variant. The agent loop catches it and surfaces
//! the message to the model as a tool-result error string,
//! letting the model recover.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, AgentError>;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("config error: {0}")]
    Config(String),

    #[error("provider error: {0}")]
    Provider(String),

    #[error("tool error: {0}")]
    Tool(#[from] ToolError),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error("plugin error: {0}")]
    Plugin(String),

    /// Subscription-auth failure (`src/auth/`). Separate from
    /// `Config` so the login flow and the request path can be verbose
    /// about *why* auth failed and point at the API-key fallback,
    /// without that phrasing leaking into ordinary config errors.
    #[error("auth error: {0}")]
    Auth(String),

    /// The headless client already rendered a structured incomplete result.
    /// The binary maps this to a non-zero exit without printing a second error.
    #[error("max turns exhausted: {0}")]
    MaxTurnsExhausted(usize),
}

impl From<mlua::Error> for AgentError {
    fn from(e: mlua::Error) -> Self {
        AgentError::Plugin(e.to_string())
    }
}

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("unknown tool: {0}")]
    Unknown(String),

    #[error("invalid arguments for {tool}: {detail}")]
    InvalidArguments { tool: String, detail: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_error_unknown_displays_name() {
        let e = ToolError::Unknown("Frobnicate".into());
        assert_eq!(e.to_string(), "unknown tool: Frobnicate");
    }

    #[test]
    fn tool_error_invalid_args_includes_tool_and_detail() {
        let e = ToolError::InvalidArguments {
            tool: "Read".into(),
            detail: "missing file_path".into(),
        };
        assert_eq!(
            e.to_string(),
            "invalid arguments for Read: missing file_path"
        );
    }

    #[test]
    fn agent_error_wraps_tool_error_via_from() {
        let inner = ToolError::Unknown("X".into());
        let wrapped: AgentError = inner.into();
        assert!(matches!(wrapped, AgentError::Tool(_)));
        assert_eq!(wrapped.to_string(), "tool error: unknown tool: X");
    }

    #[test]
    fn agent_error_wraps_io_via_from() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "nope");
        let wrapped: AgentError = io.into();
        assert!(matches!(wrapped, AgentError::Io(_)));
    }

    #[test]
    fn agent_error_wraps_json_via_from() {
        let bad = serde_json::from_str::<serde_json::Value>("{not json").unwrap_err();
        let wrapped: AgentError = bad.into();
        assert!(matches!(wrapped, AgentError::Json(_)));
    }
}
