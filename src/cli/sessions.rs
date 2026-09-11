//! Session selection for top-level CLI runs.

use crate::error::Result;

/// Resolve an explicit, latest, or fresh persisted session.
pub fn resolve(conversation: Option<&str>, continue_session: bool) -> Result<(String, bool)> {
    crate::bootstrap::resolve_session_id(conversation, continue_session)
}
