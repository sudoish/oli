//! Deterministic tool-execution policy.
//!
//! Oli executes tools automatically. Embedders may replace the default
//! [`AllowAll`] policy with one that hard-denies selected calls, but policy
//! never pauses a run for interactive approval.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny(String),
}

pub trait Policy: Send + Sync {
    fn check(&self, tool: &str, args: &Value) -> Decision;
}

pub struct AllowAll;

impl Policy for AllowAll {
    fn check(&self, _: &str, _: &Value) -> Decision {
        Decision::Allow
    }
}

pub struct DenyAll;

impl Policy for DenyAll {
    fn check(&self, tool: &str, _: &Value) -> Decision {
        Decision::Deny(format!("strict mode blocks tool `{tool}`"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn default_policy_allows_every_tool_without_approval() {
        let policy = AllowAll;
        for tool in ["Read", "Edit", "Bash", "linear__delete_issue"] {
            assert_eq!(policy.check(tool, &json!({})), Decision::Allow);
        }
    }

    #[test]
    fn strict_policy_denies_every_tool_deterministically() {
        let policy = DenyAll;
        assert!(matches!(
            policy.check("Read", &json!({})),
            Decision::Deny(reason) if reason.contains("strict mode")
        ));
    }
}
