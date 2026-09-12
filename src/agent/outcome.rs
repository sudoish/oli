use crate::error::{AgentError, Result};

/// Typed terminal state for one agent invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    Completed(String),
    MaxTurnsExhausted { limit: usize, message: String },
}

impl RunOutcome {
    /// Require a completed response at a non-interactive composition boundary.
    pub fn into_completed(self) -> Result<String> {
        match self {
            Self::Completed(text) => Ok(text),
            Self::MaxTurnsExhausted { limit, .. } => Err(AgentError::MaxTurnsExhausted(limit)),
        }
    }
}
