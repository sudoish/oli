//! Policy engine — gates every tool call between the model deciding to
//! run it and the tool actually executing.
//!
//! Two layers:
//! - `Policy::check(tool, args)` is pure and synchronous: it returns
//!   `Allow`, `Deny`, or `Ask` based on tool name + args + config.
//! - `Approver::approve(...)` resolves an `Ask` decision asynchronously.
//!   In the REPL it prompts the user; in one-shot mode it auto-approves.
//!
//! Splitting the two keeps the policy itself testable without TTY
//! plumbing, and lets tests script approval outcomes independently.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Ask(String),
    /// Hard deny — surfaces as a "policy denied" tool result. Reserved
    /// for future config flags (e.g. a `denylist` block) and for custom
    /// policies; the default `ConfigPolicy` only emits `Allow` and `Ask`.
    #[allow(dead_code)]
    Deny(String),
}

pub trait Policy: Send + Sync {
    fn check(&self, tool: &str, args: &Value) -> Decision;
}

#[async_trait]
pub trait Approver: Send + Sync {
    /// Resolve a `Decision::Ask`. Returning `false` surfaces a
    /// "user declined" tool result back to the model.
    async fn approve(&self, tool: &str, args: &Value, reason: &str) -> bool;
}

/// Config-shaped policy parameters. `[policy]` section in the user's
/// TOML; defaults are baked in so a missing section produces a sensible
/// starting point rather than empty allowlists.
#[derive(Clone, Debug, Deserialize)]
pub struct PolicyConfig {
    #[serde(default = "PolicyConfig::default_auto_allow")]
    pub auto_allow: Vec<String>,

    #[serde(default = "PolicyConfig::default_ask")]
    pub ask: Vec<String>,

    #[serde(default = "PolicyConfig::default_bash_allowlist")]
    pub bash_allowlist: Vec<String>,
}

impl PolicyConfig {
    fn default_auto_allow() -> Vec<String> {
        ["Read", "Glob", "Grep"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    fn default_ask() -> Vec<String> {
        ["Write", "Edit", "Bash"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    fn default_bash_allowlist() -> Vec<String> {
        [
            "git status",
            "git diff",
            "git diff *",
            "git log",
            "git log *",
            "git show *",
            "git branch",
            "git branch *",
            "cargo build",
            "cargo build *",
            "cargo test",
            "cargo test *",
            "cargo check",
            "cargo check *",
            "cargo fmt",
            "cargo fmt *",
            "cargo clippy",
            "cargo clippy *",
            "ls",
            "ls *",
            "pwd",
            "echo *",
            "wc *",
            "head *",
            "tail *",
            "rg *",
            "find *",
            "which *",
            "true",
            "false",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            auto_allow: Self::default_auto_allow(),
            ask: Self::default_ask(),
            bash_allowlist: Self::default_bash_allowlist(),
        }
    }
}

/// Default policy. Tool names against `auto_allow`/`ask` lists plus
/// glob-style patterns for `Bash` `command` strings. Anything unmatched
/// falls through to `Ask` so a misconfiguration errs on the safe side.
pub struct ConfigPolicy {
    auto_allow: HashSet<String>,
    ask: HashSet<String>,
    bash_patterns: Vec<glob::Pattern>,
}

impl ConfigPolicy {
    pub fn from_config(cfg: &PolicyConfig) -> Self {
        let bash_patterns = cfg
            .bash_allowlist
            .iter()
            .filter_map(|s| glob::Pattern::new(s).ok())
            .collect();
        Self {
            auto_allow: cfg.auto_allow.iter().cloned().collect(),
            ask: cfg.ask.iter().cloned().collect(),
            bash_patterns,
        }
    }

    /// Convenience for tests and the binary's startup path when no config
    /// override is present.
    pub fn defaults() -> Self {
        Self::from_config(&PolicyConfig::default())
    }
}

impl Policy for ConfigPolicy {
    fn check(&self, tool: &str, args: &Value) -> Decision {
        if tool == "Bash" {
            let cmd = args
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            if !cmd.is_empty() && self.bash_patterns.iter().any(|p| p.matches(cmd)) {
                return Decision::Allow;
            }
            return Decision::Ask(format!("run shell command: {}", cmd));
        }
        if self.auto_allow.contains(tool) {
            return Decision::Allow;
        }
        if self.ask.contains(tool) {
            return Decision::Ask(format!("invoke {}", tool));
        }
        Decision::Ask(format!("invoke unknown tool {}", tool))
    }
}

/// Approver that always says yes. Used for `-p` one-shot mode and tests
/// where the policy infrastructure should be transparent.
pub struct AlwaysApprove;

#[async_trait]
impl Approver for AlwaysApprove {
    async fn approve(&self, _: &str, _: &Value, _: &str) -> bool {
        true
    }
}

/// Approver that always says no. Useful for testing the deny path and as
/// a building block for a future strict-mode flag.
#[allow(dead_code)]
pub struct AlwaysDeny;

#[async_trait]
impl Approver for AlwaysDeny {
    async fn approve(&self, _: &str, _: &Value, _: &str) -> bool {
        false
    }
}

/// Interactive approver that prompts the user via stdin/stdout. Reads
/// y/N on a blocking task so the runtime stays responsive. Renders a
/// tool-specific preview so the user sees what's about to land before
/// answering — for `Edit` and `Write` that means a diff/content view,
/// not just the raw JSON args.
pub struct ReadlineApprover;

#[async_trait]
impl Approver for ReadlineApprover {
    async fn approve(&self, tool: &str, args: &Value, reason: &str) -> bool {
        let preview = preview_for(tool, args);
        let prompt = if preview.is_empty() {
            format!("[approve] {} — {} [y/N] ", tool, reason)
        } else {
            format!("[approve] {} — {}\n{}\n[y/N] ", tool, reason, preview)
        };
        tokio::task::spawn_blocking(move || {
            use std::io::{BufRead, Write};
            let mut stdout = std::io::stdout();
            let _ = stdout.write_all(prompt.as_bytes());
            let _ = stdout.flush();
            let stdin = std::io::stdin();
            let mut line = String::new();
            if stdin.lock().read_line(&mut line).is_err() {
                return false;
            }
            let trimmed = line.trim();
            trimmed.eq_ignore_ascii_case("y") || trimmed.eq_ignore_ascii_case("yes")
        })
        .await
        .unwrap_or(false)
    }
}

/// Tool-aware preview. Edit and Write get diff/content views so the
/// user sees what's about to land; everything else falls through to a
/// compact JSON dump truncated at 200 chars.
fn preview_for(tool: &str, args: &Value) -> String {
    match tool {
        "Edit" => render_edit_preview(args),
        "Write" => render_write_preview(args),
        _ => preview_args(args),
    }
}

fn render_edit_preview(args: &Value) -> String {
    let path = args
        .get("file_path")
        .and_then(|v| v.as_str())
        .unwrap_or("(unknown)");
    let old = args
        .get("old_string")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let new = args
        .get("new_string")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let replace_all = args
        .get("replace_all")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mut out = format!("  file: {}", path);
    if replace_all {
        out.push_str("  (replace_all)");
    }
    out.push_str("\n  -- old:\n");
    out.push_str(&indent_with("    | ", &truncate_lines(old, 30)));
    out.push_str("\n  ++ new:\n");
    out.push_str(&indent_with("    | ", &truncate_lines(new, 30)));
    out
}

fn render_write_preview(args: &Value) -> String {
    let path = args
        .get("file_path")
        .and_then(|v| v.as_str())
        .unwrap_or("(unknown)");
    let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
    let bytes = content.len();
    let lines = content.lines().count();
    format!(
        "  file: {}\n  ({} bytes, {} lines)\n  ++ content:\n{}",
        path,
        bytes,
        lines,
        indent_with("    | ", &truncate_lines(content, 30))
    )
}

fn indent_with(prefix: &str, body: &str) -> String {
    body.lines()
        .map(|l| format!("{}{}", prefix, l))
        .collect::<Vec<_>>()
        .join("\n")
}

fn truncate_lines(s: &str, max_lines: usize) -> String {
    let total = s.lines().count();
    if total <= max_lines {
        return s.to_string();
    }
    let head: Vec<&str> = s.lines().take(max_lines).collect();
    format!(
        "{}\n... ({} more lines)",
        head.join("\n"),
        total - max_lines
    )
}

/// Compact rendering of tool arguments for an approval prompt. Truncates
/// long fields so a 50KB Write doesn't flood the terminal.
fn preview_args(args: &Value) -> String {
    let raw = args.to_string();
    if raw == "{}" {
        return String::new();
    }
    const LIMIT: usize = 200;
    if raw.len() <= LIMIT {
        return raw;
    }
    let mut cut = LIMIT;
    while cut > 0 && !raw.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}… ({} bytes)", &raw[..cut], raw.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn default_policy_auto_allows_read_glob_grep() {
        let p = ConfigPolicy::defaults();
        assert_eq!(p.check("Read", &json!({})), Decision::Allow);
        assert_eq!(p.check("Glob", &json!({})), Decision::Allow);
        assert_eq!(p.check("Grep", &json!({})), Decision::Allow);
    }

    #[test]
    fn default_policy_asks_for_edit_and_write() {
        let p = ConfigPolicy::defaults();
        assert!(matches!(p.check("Edit", &json!({})), Decision::Ask(_)));
        assert!(matches!(p.check("Write", &json!({})), Decision::Ask(_)));
    }

    #[test]
    fn default_policy_asks_for_unknown_tools() {
        let p = ConfigPolicy::defaults();
        let dec = p.check("Frobnicate", &json!({}));
        assert!(matches!(dec, Decision::Ask(_)));
        match dec {
            Decision::Ask(msg) => assert!(msg.contains("Frobnicate")),
            _ => unreachable!(),
        }
    }

    #[test]
    fn default_policy_allows_known_bash_commands() {
        let p = ConfigPolicy::defaults();
        assert_eq!(
            p.check("Bash", &json!({"command":"git status"})),
            Decision::Allow
        );
        assert_eq!(
            p.check("Bash", &json!({"command":"cargo test --release"})),
            Decision::Allow
        );
        assert_eq!(
            p.check("Bash", &json!({"command":"ls -la"})),
            Decision::Allow
        );
    }

    #[test]
    fn default_policy_asks_for_unknown_bash_commands() {
        let p = ConfigPolicy::defaults();
        let dec = p.check("Bash", &json!({"command":"rm -rf /"}));
        assert!(matches!(dec, Decision::Ask(_)));
        match dec {
            Decision::Ask(msg) => assert!(msg.contains("rm -rf /")),
            _ => unreachable!(),
        }
    }

    #[test]
    fn empty_bash_command_does_not_match_glob_star() {
        // `*` matches empty string, but we shouldn't allow a literally
        // empty command — a model that emits Bash with no `command` field
        // is up to no good.
        let p = ConfigPolicy::defaults();
        let dec = p.check("Bash", &json!({"command":""}));
        assert!(matches!(dec, Decision::Ask(_)));
    }

    #[test]
    fn config_overrides_replace_defaults() {
        let cfg = PolicyConfig {
            auto_allow: vec!["Edit".into()],
            ask: vec![],
            bash_allowlist: vec![],
        };
        let p = ConfigPolicy::from_config(&cfg);
        assert_eq!(p.check("Edit", &json!({})), Decision::Allow);
        // Read isn't on the override list — falls through to Ask.
        assert!(matches!(p.check("Read", &json!({})), Decision::Ask(_)));
    }

    #[tokio::test]
    async fn always_approve_returns_true() {
        assert!(AlwaysApprove.approve("X", &json!({}), "r").await);
    }

    #[tokio::test]
    async fn always_deny_returns_false() {
        assert!(!AlwaysDeny.approve("X", &json!({}), "r").await);
    }

    #[test]
    fn preview_truncates_long_args() {
        let big = "x".repeat(500);
        let v = json!({"content": big});
        let s = preview_args(&v);
        assert!(s.contains("…"));
        assert!(s.contains("bytes"));
    }

    #[test]
    fn preview_returns_empty_for_empty_object() {
        assert!(preview_args(&json!({})).is_empty());
    }

    #[test]
    fn edit_preview_shows_old_and_new_with_path() {
        let args = json!({
            "file_path": "src/x.rs",
            "old_string": "let a = 1;",
            "new_string": "let a = 2;",
        });
        let s = render_edit_preview(&args);
        assert!(s.contains("file: src/x.rs"));
        assert!(s.contains("-- old:"));
        assert!(s.contains("let a = 1;"));
        assert!(s.contains("++ new:"));
        assert!(s.contains("let a = 2;"));
    }

    #[test]
    fn edit_preview_marks_replace_all() {
        let args = json!({
            "file_path": "x",
            "old_string": "a",
            "new_string": "b",
            "replace_all": true,
        });
        let s = render_edit_preview(&args);
        assert!(s.contains("(replace_all)"));
    }

    #[test]
    fn write_preview_reports_size_and_truncates_long_content() {
        let big = "line\n".repeat(100);
        let args = json!({"file_path":"big.txt","content": big});
        let s = render_write_preview(&args);
        assert!(s.contains("file: big.txt"));
        assert!(s.contains("500 bytes"));
        assert!(s.contains("100 lines"));
        assert!(s.contains("more lines"));
    }

    #[test]
    fn preview_for_routes_to_tool_specific_renderers() {
        let edit_args = json!({"file_path":"x","old_string":"a","new_string":"b"});
        assert!(preview_for("Edit", &edit_args).contains("-- old:"));
        let write_args = json!({"file_path":"x","content":"c"});
        assert!(preview_for("Write", &write_args).contains("++ content:"));
        // Unknown tools fall through to JSON dump.
        let other = json!({"command":"ls"});
        assert_eq!(preview_for("Bash", &other), other.to_string());
    }
}
