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

pub mod persisted_allow;
pub use persisted_allow::PersistedAllowList;

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
/// TOML; a missing section defaults to automatic tool execution.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PolicyMode {
    #[default]
    Auto,
    Ask,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PolicyConfig {
    /// Whether config rules may return `Ask`. Auto mode allows every tool
    /// invocation; ask mode applies the granular lists below.
    #[serde(default)]
    pub mode: PolicyMode,

    #[serde(default = "PolicyConfig::default_auto_allow")]
    pub auto_allow: Vec<String>,

    #[serde(default = "PolicyConfig::default_ask")]
    pub ask: Vec<String>,

    #[serde(default = "PolicyConfig::default_bash_allowlist")]
    pub bash_allowlist: Vec<String>,

    /// Auto-allow MCP tools whose bare name (after the `<server>__`
    /// prefix) starts with a pure-read verb. Per the spec the agent
    /// shouldn't make users hit y/N for every `linear__get_issue` —
    /// reads are cheap and the model loops on them. Default true. Off
    /// by setting `auto_allow_pure_reads = false`.
    #[serde(default = "PolicyConfig::default_auto_allow_pure_reads")]
    pub auto_allow_pure_reads: bool,
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

    fn default_auto_allow_pure_reads() -> bool {
        true
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
            mode: PolicyMode::default(),
            auto_allow: Self::default_auto_allow(),
            ask: Self::default_ask(),
            bash_allowlist: Self::default_bash_allowlist(),
            auto_allow_pure_reads: Self::default_auto_allow_pure_reads(),
        }
    }
}

/// Default policy. Tool names against `auto_allow`/`ask` lists plus
/// glob-style patterns for `Bash` `command` strings. Anything unmatched
/// falls through to `Ask` when approval mode is enabled.
pub struct ConfigPolicy {
    mode: PolicyMode,
    auto_allow: HashSet<String>,
    ask: HashSet<String>,
    bash_patterns: Vec<glob::Pattern>,
    /// Mirror of `PolicyConfig::auto_allow_pure_reads`. When true,
    /// MCP tools (recognized by the `<server>__<tool>` namespacing
    /// convention) whose bare-name verb is a pure read get auto-allow
    /// treatment without an explicit allow-list entry.
    auto_allow_pure_reads: bool,
}

/// Verbs treated as pure reads when `auto_allow_pure_reads` is on.
/// Every published MCP server we've seen so far uses one of these
/// prefixes for its read endpoints; everything else (save_, create_,
/// update_, delete_, run_, exec_, ...) stays in the Ask path.
const PURE_READ_PREFIXES: &[&str] = &[
    "get_",
    "list_",
    "search_",
    "fetch_",
    "read_",
    "describe_",
    "show_",
    "find_",
    "query_",
];

impl ConfigPolicy {
    pub fn from_config(cfg: &PolicyConfig) -> Self {
        let bash_patterns = cfg
            .bash_allowlist
            .iter()
            .filter_map(|s| glob::Pattern::new(s).ok())
            .collect();
        Self {
            mode: cfg.mode,
            auto_allow: cfg.auto_allow.iter().cloned().collect(),
            ask: cfg.ask.iter().cloned().collect(),
            bash_patterns,
            auto_allow_pure_reads: cfg.auto_allow_pure_reads,
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
        if self.mode == PolicyMode::Auto {
            return Decision::Allow;
        }
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
        // MCP-pure-reads heuristic: tools register as `<server>__<bare>`.
        // If the bare name starts with a known read verb, auto-allow
        // unless the user has turned the heuristic off.
        if self.auto_allow_pure_reads {
            if let Some(bare) = mcp_bare_name(tool) {
                if PURE_READ_PREFIXES.iter().any(|p| bare.starts_with(p)) {
                    return Decision::Allow;
                }
            }
        }
        Decision::Ask(format!("invoke unknown tool {}", tool))
    }
}

/// Extract the bare tool name from an MCP-namespaced identifier
/// (`<server>__<tool>`). Returns `None` for non-MCP tools so the
/// heuristic only fires where it's intended.
fn mcp_bare_name(tool: &str) -> Option<&str> {
    tool.split_once("__").map(|(_, bare)| bare)
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
pub(crate) fn preview_for(tool: &str, args: &Value) -> String {
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
    out.push('\n');
    out.push_str(&render_unified_diff(old, new, 30));
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

    // If the file already exists, render as a unified diff against
    // its current contents — much more readable than a raw dump for
    // edits to existing files. New-file Writes still show the
    // content body inline since "diff against empty" doesn't add value.
    let path_exists = std::fs::metadata(path).is_ok();
    let mut out = format!("  file: {}\n  ({} bytes, {} lines)\n", path, bytes, lines);
    if path_exists {
        let existing = std::fs::read_to_string(path).unwrap_or_default();
        out.push_str(&render_unified_diff(&existing, content, 30));
    } else {
        out.push_str("  ++ content:\n");
        out.push_str(&indent_with("    | ", &truncate_lines(content, 30)));
    }
    out
}

/// Render a unified diff between `old` and `new` with three lines of
/// context. Long bodies get truncated to roughly `max_lines` of diff
/// output before a `... (more lines)` marker. Color-free; the REPL
/// approval prompt is line-oriented and we keep this terminal-agnostic.
fn render_unified_diff(old: &str, new: &str, max_lines: usize) -> String {
    use similar::{ChangeTag, TextDiff};
    let diff = TextDiff::from_lines(old, new);
    let mut out = String::new();
    let mut emitted = 0usize;
    let mut more = 0usize;
    for op in diff.grouped_ops(3) {
        for change in op.iter().flat_map(|o| diff.iter_changes(o)) {
            if emitted >= max_lines {
                more += 1;
                continue;
            }
            let sign = match change.tag() {
                ChangeTag::Equal => " ",
                ChangeTag::Delete => "-",
                ChangeTag::Insert => "+",
            };
            // `change.value()` already includes a trailing newline on
            // most lines; trim and re-add so we don't get blank gaps.
            let body = change.value().trim_end_matches('\n');
            out.push_str(&format!("    {} {}\n", sign, body));
            emitted += 1;
        }
    }
    if more > 0 {
        out.push_str(&format!("    ... ({} more lines)\n", more));
    }
    if out.is_empty() {
        // No changes (both bodies identical) — say so, otherwise the
        // approval prompt looks like it's missing content.
        out.push_str("    (no diff — old and new are identical)\n");
    }
    out
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
    fn default_policy_auto_allows_mutations_unknown_tools_and_bash() {
        let p = ConfigPolicy::defaults();
        assert_eq!(p.check("Edit", &json!({})), Decision::Allow);
        assert_eq!(p.check("Write", &json!({})), Decision::Allow);
        assert_eq!(p.check("Frobnicate", &json!({})), Decision::Allow);
        assert_eq!(
            p.check("Bash", &json!({"command":"rm -rf target"})),
            Decision::Allow
        );
    }

    #[test]
    fn ask_mode_allows_known_bash_commands() {
        let cfg = PolicyConfig {
            mode: PolicyMode::Ask,
            ..PolicyConfig::default()
        };
        let p = ConfigPolicy::from_config(&cfg);
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
    fn ask_mode_asks_for_mutations_unknown_tools_and_unknown_bash() {
        let cfg = PolicyConfig {
            mode: PolicyMode::Ask,
            ..PolicyConfig::default()
        };
        let p = ConfigPolicy::from_config(&cfg);
        assert!(matches!(p.check("Edit", &json!({})), Decision::Ask(_)));
        assert!(matches!(p.check("Write", &json!({})), Decision::Ask(_)));
        assert!(matches!(
            p.check("Frobnicate", &json!({})),
            Decision::Ask(_)
        ));
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
        let cfg = PolicyConfig {
            mode: PolicyMode::Ask,
            ..PolicyConfig::default()
        };
        let p = ConfigPolicy::from_config(&cfg);
        let dec = p.check("Bash", &json!({"command":""}));
        assert!(matches!(dec, Decision::Ask(_)));
    }

    #[test]
    fn config_overrides_replace_defaults() {
        let cfg = PolicyConfig {
            mode: PolicyMode::Ask,
            auto_allow: vec!["Edit".into()],
            ask: vec![],
            bash_allowlist: vec![],
            auto_allow_pure_reads: false,
        };
        let p = ConfigPolicy::from_config(&cfg);
        assert_eq!(p.check("Edit", &json!({})), Decision::Allow);
        // Read isn't on the override list — falls through to Ask.
        assert!(matches!(p.check("Read", &json!({})), Decision::Ask(_)));
    }

    #[test]
    fn pure_read_heuristic_auto_allows_get_list_search_etc() {
        let cfg = PolicyConfig {
            mode: PolicyMode::Ask,
            ..PolicyConfig::default()
        };
        let p = ConfigPolicy::from_config(&cfg);
        for tool in &[
            "linear__get_issue",
            "github__list_pull_requests",
            "sentry__search_issues",
            "notion__fetch_database",
            "vault__read_secret",
            "playwright__describe_page",
            "linear__find_user",
            "datadog__query_metrics",
        ] {
            assert!(
                matches!(p.check(tool, &json!({})), Decision::Allow),
                "expected Allow for {}",
                tool
            );
        }
    }

    #[test]
    fn pure_read_heuristic_does_not_auto_allow_writes() {
        let cfg = PolicyConfig {
            mode: PolicyMode::Ask,
            ..PolicyConfig::default()
        };
        let p = ConfigPolicy::from_config(&cfg);
        for tool in &[
            "linear__save_issue",
            "linear__delete_user",
            "github__create_pull_request",
            "sentry__update_alert",
            "playwright__browser_click",
        ] {
            assert!(
                matches!(p.check(tool, &json!({})), Decision::Ask(_)),
                "expected Ask for {}",
                tool
            );
        }
    }

    #[test]
    fn pure_read_heuristic_off_falls_through_to_ask() {
        let cfg = PolicyConfig {
            mode: PolicyMode::Ask,
            auto_allow_pure_reads: false,
            ..PolicyConfig::default()
        };
        let p = ConfigPolicy::from_config(&cfg);
        assert!(matches!(
            p.check("linear__get_issue", &json!({})),
            Decision::Ask(_)
        ));
    }

    #[test]
    fn pure_read_heuristic_does_not_match_non_mcp_tools() {
        // `Read` is a built-in already on the auto_allow list. The
        // heuristic shouldn't fire on tool names that don't have the
        // `<server>__<tool>` shape — otherwise a custom plugin tool
        // named `get_thing` would silently bypass the Ask gate.
        let cfg = PolicyConfig {
            mode: PolicyMode::Ask,
            auto_allow: vec![],
            ask: vec![],
            bash_allowlist: vec![],
            auto_allow_pure_reads: true,
        };
        let p = ConfigPolicy::from_config(&cfg);
        assert!(matches!(p.check("get_thing", &json!({})), Decision::Ask(_)));
        // Multiple underscores but no `__` separator: still not MCP-shaped.
        assert!(matches!(
            p.check("get_my_thing", &json!({})),
            Decision::Ask(_)
        ));
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
    fn edit_preview_shows_unified_diff_with_path() {
        let args = json!({
            "file_path": "src/x.rs",
            "old_string": "let a = 1;",
            "new_string": "let a = 2;",
        });
        let s = render_edit_preview(&args);
        assert!(s.contains("file: src/x.rs"));
        // Both directions of the change appear in the diff.
        assert!(s.contains("- let a = 1;"), "missing deletion line: {}", s);
        assert!(s.contains("+ let a = 2;"), "missing addition line: {}", s);
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
    fn edit_preview_reports_no_diff_when_unchanged() {
        let args = json!({
            "file_path": "x",
            "old_string": "same",
            "new_string": "same",
        });
        let s = render_edit_preview(&args);
        assert!(s.contains("no diff"), "expected no-diff marker: {}", s);
    }

    #[test]
    fn write_preview_reports_size_and_truncates_long_content_for_new_file() {
        let big = "line\n".repeat(100);
        let args =
            json!({"file_path":"/tmp/__nonexistent_file_for_write_preview__","content": big});
        let s = render_write_preview(&args);
        assert!(s.contains("file: /tmp/"));
        assert!(s.contains("500 bytes"));
        assert!(s.contains("100 lines"));
        // New-file path still uses the inline truncation marker.
        assert!(s.contains("more lines"));
    }

    #[test]
    fn write_preview_renders_unified_diff_when_file_exists() {
        use tempfile::NamedTempFile;
        let f = NamedTempFile::new().unwrap();
        std::fs::write(f.path(), "alpha\nbeta\ngamma\n").unwrap();
        let args = json!({
            "file_path": f.path().to_str().unwrap(),
            "content": "alpha\nBETA\ngamma\n",
        });
        let s = render_write_preview(&args);
        assert!(s.contains("- beta"), "missing deletion: {}", s);
        assert!(s.contains("+ BETA"), "missing addition: {}", s);
    }

    #[test]
    fn preview_for_routes_to_tool_specific_renderers() {
        let edit_args = json!({"file_path":"x","old_string":"a","new_string":"b"});
        // Diff lines now use the unified-diff format.
        let edit = preview_for("Edit", &edit_args);
        assert!(edit.contains("- a"));
        assert!(edit.contains("+ b"));
        let write_args = json!({"file_path":"x","content":"c"});
        assert!(preview_for("Write", &write_args).contains("++ content:"));
        // Unknown tools fall through to JSON dump.
        let other = json!({"command":"ls"});
        assert_eq!(preview_for("Bash", &other), other.to_string());
    }

    #[test]
    fn unified_diff_truncates_long_diffs_with_marker() {
        let old = (0..200)
            .map(|i| format!("line {}\n", i))
            .collect::<String>();
        let new = (0..200)
            .map(|i| format!("LINE {}\n", i))
            .collect::<String>();
        let s = render_unified_diff(&old, &new, 20);
        assert!(
            s.contains("more lines"),
            "expected truncation marker: {}",
            s
        );
    }
}
