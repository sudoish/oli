use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use crate::error::{Result, ToolError};
use crate::tools::util::{DEFAULT_MAX_OUTPUT_BYTES, truncate};
use crate::tools::{Tool, ToolContext};

pub struct Bash;

const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const MAX_TIMEOUT_MS: u64 = 600_000;

#[async_trait]
impl Tool for Bash {
    fn name(&self) -> &str {
        "Bash"
    }

    fn description(&self) -> &str {
        "Execute a shell command and return its combined stdout and stderr. \
         Optional `cwd` (working directory; sticky across calls) and \
         `timeout_ms` (default 120000, max 600000). On timeout the child \
         process is killed and the model sees a timeout marker."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                },
                "cwd": {
                    "type": "string",
                    "description": "Working directory for the command. If set, future Bash calls inherit it until another `cwd` is supplied."
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Hard timeout in milliseconds (default 120000, max 600000). The child is killed on timeout."
                }
            },
            "required": ["command"]
        })
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let command = args["command"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArguments {
                tool: "Bash".into(),
                detail: "missing or non-string `command`".into(),
            })?;

        let timeout_ms = args
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_MS)
            .min(MAX_TIMEOUT_MS);

        let cwd_arg = args.get("cwd").and_then(|v| v.as_str()).map(PathBuf::from);
        if let Some(c) = &cwd_arg {
            ctx.set_cwd(c.clone()).await;
        }
        let cwd = match cwd_arg {
            Some(c) => Some(c),
            None => ctx.cwd().await,
        };

        Ok(truncate(
            &run_bash(command, cwd.as_deref(), Duration::from_millis(timeout_ms)).await,
            DEFAULT_MAX_OUTPUT_BYTES,
        ))
    }
}

async fn run_bash(command: &str, cwd: Option<&Path>, timeout: Duration) -> String {
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c").arg(command);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return format!("Error executing command: {}", e),
    };

    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => format_output(&output),
        Ok(Err(e)) => format!("Error waiting for command: {}", e),
        // The child handle is owned by `wait_with_output`; dropping the
        // future on timeout drops it, and `kill_on_drop(true)` ensures
        // the child gets a SIGKILL on the way out.
        Err(_) => format!(
            "Command timed out after {}ms (child killed)",
            timeout.as_millis()
        ),
    }
}

fn format_output(output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let mut result = String::new();
    result.push_str(&stdout);
    if !stderr.is_empty() {
        if !result.is_empty() && !result.ends_with('\n') {
            result.push('\n');
        }
        result.push_str(&stderr);
    }
    if !output.status.success() {
        if !result.is_empty() && !result.ends_with('\n') {
            result.push('\n');
        }
        let code = output
            .status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "unknown".into());
        result.push_str(&format!("Command exited with status: {}", code));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn captures_stdout() {
        let ctx = ToolContext::new();
        let out = Bash
            .run(json!({ "command": "printf hello" }), &ctx)
            .await
            .unwrap();
        assert_eq!(out, "hello");
    }

    #[tokio::test]
    async fn captures_stderr_and_exit_code_for_failure() {
        let ctx = ToolContext::new();
        let out = Bash
            .run(json!({ "command": "echo oops 1>&2 && exit 3" }), &ctx)
            .await
            .unwrap();
        assert!(out.contains("oops"));
        assert!(out.contains("Command exited with status: 3"));
    }

    #[tokio::test]
    async fn missing_arg_is_invalid_args_error() {
        let ctx = ToolContext::new();
        let err = Bash.run(json!({}), &ctx).await.unwrap_err();
        assert!(err.to_string().contains("invalid arguments for Bash"));
    }

    #[tokio::test]
    async fn truncates_oversized_output() {
        let ctx = ToolContext::new();
        // Generate 50k bytes of output, which is above DEFAULT_MAX_OUTPUT_BYTES.
        let cmd = format!("printf 'x%.0s' $(seq 1 50000)");
        let out = Bash.run(json!({ "command": cmd }), &ctx).await.unwrap();
        assert!(out.contains("[... output truncated"));
    }

    #[tokio::test]
    async fn cwd_arg_runs_command_in_directory() {
        let dir = tempdir().unwrap();
        let target = dir.path().to_str().unwrap().to_string();
        let ctx = ToolContext::new();
        let out = Bash
            .run(
                json!({ "command": "pwd", "cwd": target.clone() }),
                &ctx,
            )
            .await
            .unwrap();
        // macOS canonicalizes /var/folders/... → /private/var/folders/... so we just
        // check that the resolved tempdir basename is in the output rather than
        // string-equality.
        let leaf = dir.path().file_name().unwrap().to_string_lossy().to_string();
        assert!(
            out.contains(&leaf),
            "expected pwd output to contain {}, got {}",
            leaf,
            out
        );
    }

    #[tokio::test]
    async fn cwd_persists_across_subsequent_calls_until_overridden() {
        let dir = tempdir().unwrap();
        let target = dir.path().to_str().unwrap().to_string();
        let ctx = ToolContext::new();

        // Set cwd via first call.
        Bash.run(
            json!({ "command": "true", "cwd": target.clone() }),
            &ctx,
        )
        .await
        .unwrap();

        // Second call without cwd should inherit it.
        let out = Bash
            .run(json!({ "command": "pwd" }), &ctx)
            .await
            .unwrap();
        let leaf = dir.path().file_name().unwrap().to_string_lossy().to_string();
        assert!(
            out.contains(&leaf),
            "second call should inherit cwd, got {}",
            out
        );
    }

    #[tokio::test]
    async fn timeout_kills_long_running_command() {
        let ctx = ToolContext::new();
        let started = std::time::Instant::now();
        let out = Bash
            .run(
                json!({ "command": "sleep 10", "timeout_ms": 200 }),
                &ctx,
            )
            .await
            .unwrap();
        let elapsed = started.elapsed();
        assert!(out.contains("timed out"), "expected timeout marker: {}", out);
        assert!(
            elapsed < Duration::from_secs(2),
            "timeout should fire within ~2s, took {:?}",
            elapsed
        );
    }
}
