//! `TuiHook` — bridges the agent's `HookRegistry` events into
//! `UiEvent::ToolStart` / `UiEvent::ToolDone` for the render loop.
//!
//! The hook trait's payload doesn't include a `tool_call_id` — only
//! `tool` name and `args` — but the agent loop dispatches tools
//! sequentially within a turn (Pre then Post for the same call,
//! never interleaved). So we use a small in-memory stack: push a
//! synthetic id + start timestamp on Pre, pop on Post. Subagent
//! tool calls don't fire this hook (subagents get a fresh agent
//! without our hooks attached), so we never see nested rounds.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::hooks::{Hook, HookOutcome, HookPayload};
use crate::tui::event::UiEvent;

pub struct TuiHook {
    tx: UnboundedSender<UiEvent>,
    /// Pending Pre→Post correlation. Sequential firing means a
    /// stack with one slot at a time is sufficient; we keep a
    /// `Vec` so a future variant of the agent loop that fires Pre
    /// in batches doesn't break correlation.
    pending: Mutex<Vec<PendingCall>>,
    next_id: AtomicU64,
}

struct PendingCall {
    id: u64,
    started_at: Instant,
    tool: String,
}

impl TuiHook {
    pub fn new(tx: UnboundedSender<UiEvent>) -> Self {
        Self {
            tx,
            pending: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
        }
    }
}

#[async_trait]
impl Hook for TuiHook {
    fn name(&self) -> &str {
        "tui-progress"
    }

    async fn handle(&self, payload: &HookPayload<'_>) -> HookOutcome {
        match payload {
            HookPayload::PreToolUse { tool, args } => {
                let id = self.next_id.fetch_add(1, Ordering::Relaxed);
                self.pending.lock().unwrap().push(PendingCall {
                    id,
                    started_at: Instant::now(),
                    tool: (*tool).to_string(),
                });
                let _ = self.tx.send(UiEvent::ToolStart {
                    id,
                    tool: (*tool).to_string(),
                    args_preview: render_args_preview(args, 60),
                });
            }
            HookPayload::PostToolUse { tool, result, .. } => {
                let popped = {
                    let mut p = self.pending.lock().unwrap();
                    // Pop the most recent matching entry. If the
                    // hook stack ever drifts (a Pre we never saw
                    // a Post for), this gracefully degrades to
                    // popping nothing for the current Post.
                    if let Some(idx) = p.iter().rposition(|c| c.tool == *tool) {
                        Some(p.remove(idx))
                    } else {
                        p.pop()
                    }
                };
                if let Some(c) = popped {
                    let duration = c.started_at.elapsed();
                    let (summary, ok) = render_result_summary(tool, result);
                    let full_output = truncate_full_output(result);
                    let _ = self.tx.send(UiEvent::ToolDone {
                        id: c.id,
                        duration,
                        summary,
                        ok,
                        full_output,
                    });
                }
            }
            _ => {}
        }
        HookOutcome::Continue
    }
}

/// Single-line, char-bounded preview of tool args. Picks the most
/// telling scalar field when present and falls through to a JSON
/// dump otherwise. Mirrors `repl::preview_args`; kept duplicated
/// to avoid coupling the TUI to the line-mode REPL's internals.
pub fn render_args_preview(args: &Value, max_len: usize) -> String {
    let priority = ["file_path", "command", "pattern", "path", "prompt"];
    for k in priority.iter() {
        if let Some(v) = args.get(*k).and_then(|v| v.as_str()) {
            return clip_one_line(&format!("{}={}", k, v), max_len);
        }
    }
    let raw = args.to_string();
    if raw == "{}" {
        return String::new();
    }
    clip_one_line(&raw, max_len)
}

/// Phase Y4: cap the captured tool output so a single multi-MB
/// Read or Bash result can't bloat the transcript struct. 16 KiB
/// is comfortably above any sensible "show me the output" view —
/// the renderer further caps at 40 lines when expanding.
const FULL_OUTPUT_CAP: usize = 16 * 1024;

pub fn truncate_full_output(result: &str) -> String {
    if result.len() <= FULL_OUTPUT_CAP {
        return result.to_string();
    }
    // Snap to a char boundary so the truncation point doesn't
    // split a multi-byte UTF-8 sequence.
    let mut cut = FULL_OUTPUT_CAP;
    while cut > 0 && !result.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut out = String::with_capacity(cut + 64);
    out.push_str(&result[..cut]);
    out.push_str("\n…(truncated)");
    out
}

fn clip_one_line(s: &str, max_len: usize) -> String {
    let one = s.replace('\n', " ");
    if one.chars().count() <= max_len {
        return one;
    }
    let mut out: String = one.chars().take(max_len.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Per-tool result summarizers. Returns `(summary, ok)` where
/// `summary` is the one-line tail rendered next to the card and
/// `ok` flips the card's check vs cross. Tools that put error
/// markers in their result string are detected by their conventions
/// (the agent surfaces tool errors as `"Error: …"` strings the
/// model can react to, by design).
pub fn render_result_summary(tool: &str, result: &str) -> (String, bool) {
    let trimmed = result.trim_end();
    let ok = !is_error_result(trimmed);

    let summary = match tool {
        "Read" => {
            if trimmed.is_empty() {
                "(empty file)".into()
            } else {
                let lines = trimmed.lines().count();
                if trimmed.starts_with("Error reading") {
                    first_line(trimmed)
                } else {
                    format!("{} lines", lines)
                }
            }
        }
        "Write" => {
            if let Some(rest) = trimmed.strip_prefix("Wrote ") {
                rest.into()
            } else {
                first_line(trimmed)
            }
        }
        "Edit" => {
            if let Some(rest) = trimmed.strip_prefix("Successfully edited ") {
                rest.into()
            } else if trimmed.starts_with("Error") {
                first_line(trimmed)
            } else {
                first_line(trimmed)
            }
        }
        "Bash" => {
            if trimmed.contains("timed out") {
                "timed out".into()
            } else if let Some(line) = trimmed.lines().rev().find(|l| !l.is_empty()) {
                if line.starts_with("Command exited with status: ") {
                    line.strip_prefix("Command exited with status: ")
                        .map(|s| format!("exit {}", s))
                        .unwrap_or_else(|| line.to_string())
                } else {
                    "exit 0".into()
                }
            } else {
                "(no output)".into()
            }
        }
        "Grep" => {
            // The Grep tool returns lines like `path:line:match`;
            // counting lines is a reasonable match-count proxy.
            if trimmed.is_empty() {
                "no matches".into()
            } else {
                let n = trimmed.lines().filter(|l| !l.is_empty()).count();
                format!(
                    "{} match{}",
                    n,
                    if n == 1 { "" } else { "es" }
                )
            }
        }
        "Glob" => {
            if trimmed.is_empty() || trimmed == "(no matches)" {
                "no matches".into()
            } else {
                let n = trimmed.lines().filter(|l| !l.is_empty()).count();
                format!(
                    "{} file{}",
                    n,
                    if n == 1 { "" } else { "s" }
                )
            }
        }
        "Task" => {
            // Subagent — show length so the user knows roughly how
            // much context just landed.
            format!("{} bytes", trimmed.len())
        }
        _ => first_line(trimmed),
    };

    (clip_one_line(&summary, 60), ok)
}

fn is_error_result(s: &str) -> bool {
    s.starts_with("Error")
        || s.starts_with("error")
        || s.contains("policy denied")
        || s.contains("user declined")
        || s.contains("timed out")
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn args_preview_picks_file_path_when_present() {
        let v = json!({"file_path": "src/main.rs", "limit": 50});
        assert_eq!(render_args_preview(&v, 60), "file_path=src/main.rs");
    }

    #[test]
    fn args_preview_picks_command_for_bash() {
        let v = json!({"command": "cargo test"});
        assert_eq!(render_args_preview(&v, 60), "command=cargo test");
    }

    #[test]
    fn args_preview_falls_back_to_json_dump_when_no_priority_key() {
        let v = json!({"x": 1, "y": 2});
        let s = render_args_preview(&v, 60);
        assert!(s.starts_with("{") && s.ends_with("}"));
    }

    #[test]
    fn args_preview_clips_to_max_with_ellipsis() {
        let big = "a".repeat(120);
        let v = json!({"file_path": big});
        let s = render_args_preview(&v, 30);
        assert!(s.chars().count() <= 30);
        assert!(s.ends_with('…'));
    }

    #[test]
    fn read_summary_shows_line_count_for_success() {
        let result = "line1\nline2\nline3\n";
        let (s, ok) = render_result_summary("Read", result);
        assert_eq!(s, "3 lines");
        assert!(ok);
    }

    #[test]
    fn read_summary_marks_failure_on_error_result() {
        let result = "Error reading /nope: file not found";
        let (_s, ok) = render_result_summary("Read", result);
        assert!(!ok);
    }

    #[test]
    fn bash_summary_extracts_exit_code_from_failure() {
        let result = "stuff went wrong\nCommand exited with status: 3";
        let (s, ok) = render_result_summary("Bash", result);
        assert_eq!(s, "exit 3");
        // The result doesn't start with "Error" but bash tools
        // signal failure via a non-zero exit. We treat the absence
        // of the `Error` prefix as "ok"; a more sophisticated
        // implementation would parse the trailing exit code. For
        // now, only the model-visible Error/declined strings flip
        // the cross.
        assert!(ok);
    }

    #[test]
    fn bash_summary_marks_timeout() {
        let result = "Command timed out after 200ms (child killed)";
        let (s, ok) = render_result_summary("Bash", result);
        assert_eq!(s, "timed out");
        assert!(!ok);
    }

    #[test]
    fn grep_summary_counts_matches() {
        let result = "src/a.rs:10:foo\nsrc/b.rs:42:foo";
        let (s, ok) = render_result_summary("Grep", result);
        assert_eq!(s, "2 matches");
        assert!(ok);
    }

    #[test]
    fn glob_summary_counts_files() {
        let result = "src/a.rs\nsrc/b.rs\nsrc/c.rs";
        let (s, _) = render_result_summary("Glob", result);
        assert_eq!(s, "3 files");
    }

    #[test]
    fn unknown_tool_summary_is_first_line() {
        let result = "first line\nsecond line";
        let (s, _) = render_result_summary("MyCustom", result);
        assert_eq!(s, "first line");
    }

    #[test]
    fn truncate_full_output_passes_through_short_strings() {
        let s = "hello world\nline 2";
        assert_eq!(truncate_full_output(s), s);
    }

    #[test]
    fn truncate_full_output_caps_long_strings_with_marker() {
        let s = "a".repeat(FULL_OUTPUT_CAP + 1000);
        let out = truncate_full_output(&s);
        assert!(out.ends_with("…(truncated)"));
        // Within the cap + the marker tail, comfortably bounded.
        assert!(out.len() <= FULL_OUTPUT_CAP + 32);
    }

    #[test]
    fn truncate_full_output_snaps_to_char_boundary() {
        // 4-byte UTF-8 emoji at the cap boundary: cut must back off
        // to avoid producing invalid UTF-8 in the truncated slice.
        let mut s = "a".repeat(FULL_OUTPUT_CAP - 2);
        s.push('🦀'); // 4 bytes, straddles the cap
        s.push_str(&"b".repeat(100));
        let out = truncate_full_output(&s);
        // Doesn't panic and is valid UTF-8 by construction (String).
        assert!(out.ends_with("…(truncated)"));
        // The 🦀 either lands fully inside or fully outside the
        // truncation — never split.
        let body = out.trim_end_matches("…(truncated)").trim_end_matches('\n');
        assert!(body.is_char_boundary(body.len()));
    }

    #[test]
    fn policy_denied_result_marks_failure() {
        let result = "policy denied Edit: invoke Edit";
        let (_s, ok) = render_result_summary("Edit", result);
        assert!(!ok);
    }
}
