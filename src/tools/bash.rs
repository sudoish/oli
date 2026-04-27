use async_trait::async_trait;
use serde_json::{Value, json};

use crate::error::{Result, ToolError};
use crate::tools::util::{DEFAULT_MAX_OUTPUT_BYTES, truncate};
use crate::tools::{Tool, ToolContext};

pub struct Bash;

#[async_trait]
impl Tool for Bash {
    fn name(&self) -> &str {
        "Bash"
    }

    fn description(&self) -> &str {
        "Execute a shell command and return its combined stdout and stderr."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                }
            },
            "required": ["command"]
        })
    }

    async fn run(&self, args: Value, _ctx: &ToolContext) -> Result<String> {
        let command = args["command"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArguments {
                tool: "Bash".into(),
                detail: "missing or non-string `command`".into(),
            })?;
        Ok(truncate(&run_bash(command).await, DEFAULT_MAX_OUTPUT_BYTES))
    }
}

async fn run_bash(command: &str) -> String {
    let output = match tokio::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => return format!("Error executing command: {}", e),
    };

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
}
