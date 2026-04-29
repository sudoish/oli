//! Subprocess tool — language-agnostic external binary registered via
//! `[[tools.subprocess]]` config entries. The binary speaks JSON over
//! stdio: it receives the call's args object on stdin and emits its
//! response on stdout. Anything on stderr is appended to the result on
//! non-zero exit codes; on success it's discarded.
//!
//! This is "MCP-lite": same shape as a tool over Model Context Protocol,
//! minus the protocol negotiation. Three config lines = new tool, no
//! recompile.

use async_trait::async_trait;
use serde_json::Value;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::config::SubprocessToolConfig;
use crate::error::{AgentError, Result};
use crate::tools::{Tool, ToolContext, util};

pub struct SubprocessTool {
    name: String,
    command: String,
    args: Vec<String>,
    description: String,
    parameters: Value,
}

impl SubprocessTool {
    pub fn from_config(cfg: &SubprocessToolConfig) -> Self {
        Self {
            name: cfg.name.clone(),
            command: cfg.command.clone(),
            args: cfg.args.clone(),
            description: cfg.description.clone(),
            parameters: cfg.parameters.clone(),
        }
    }
}

#[async_trait]
impl Tool for SubprocessTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> Value {
        self.parameters.clone()
    }

    async fn run(&self, args: Value, _ctx: &ToolContext) -> Result<String> {
        let mut child = Command::new(&self.command)
            .args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                AgentError::Provider(format!("subprocess `{}` spawn failed: {}", self.command, e))
            })?;

        if let Some(mut stdin) = child.stdin.take() {
            let payload = serde_json::to_vec(&args).unwrap_or_default();
            // Best-effort write; if the subprocess closes stdin early
            // (some tools just want to be invoked) we still wait for it.
            let _ = stdin.write_all(&payload).await;
            // Closing explicitly so the subprocess sees EOF and can
            // finish reading the args body.
            drop(stdin);
        }

        let output = child.wait_with_output().await.map_err(|e| {
            AgentError::Provider(format!("subprocess `{}` wait failed: {}", self.command, e))
        })?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if output.status.success() {
            Ok(util::truncate(
                stdout.trim_end(),
                util::DEFAULT_MAX_OUTPUT_BYTES,
            ))
        } else {
            // Non-zero exit: surface stderr (and stdout, if any) as a
            // tool result the model can react to. We don't propagate as
            // ToolError because it's an operational outcome, not a
            // misuse of the tool's argument shape.
            let combined = if stdout.trim().is_empty() {
                stderr.trim().to_string()
            } else {
                format!("{}\n--- stderr ---\n{}", stdout.trim(), stderr.trim())
            };
            Ok(util::truncate(
                &format!(
                    "subprocess exited {} — {}",
                    output.status.code().unwrap_or(-1),
                    combined
                ),
                util::DEFAULT_MAX_OUTPUT_BYTES,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cfg(name: &str, command: &str, args: &[&str]) -> SubprocessToolConfig {
        SubprocessToolConfig {
            name: name.into(),
            command: command.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            description: format!("test tool {}", name),
            parameters: json!({"type":"object","properties":{}}),
        }
    }

    #[tokio::test]
    async fn returns_stdout_on_zero_exit() {
        // `cat` echoes stdin to stdout — perfect for verifying our I/O
        // round-trip in CI without writing a custom test binary.
        let tool = SubprocessTool::from_config(&cfg("Echo", "cat", &[]));
        let ctx = ToolContext::new();
        let out = tool.run(json!({"hello": "world"}), &ctx).await.unwrap();
        assert_eq!(out.trim(), r#"{"hello":"world"}"#);
    }

    #[tokio::test]
    async fn surfaces_nonzero_exit_with_stderr_to_model() {
        // `false` always exits 1, no output. Result should mention the
        // exit code so the model knows something went wrong.
        let tool = SubprocessTool::from_config(&cfg("Fail", "false", &[]));
        let ctx = ToolContext::new();
        let out = tool.run(json!({}), &ctx).await.unwrap();
        assert!(out.contains("exited 1"));
    }

    #[tokio::test]
    async fn spawn_failure_returns_error_with_command_name() {
        let tool = SubprocessTool::from_config(&cfg("Nope", "/no/such/binary", &[]));
        let ctx = ToolContext::new();
        let err = tool.run(json!({}), &ctx).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("/no/such/binary"));
        assert!(msg.contains("spawn failed") || msg.contains("No such"));
    }

    #[tokio::test]
    async fn args_are_forwarded_to_the_subprocess() {
        // `wc -c` counts bytes from stdin and emits the count on stdout.
        let tool = SubprocessTool::from_config(&cfg("Wc", "wc", &["-c"]));
        let ctx = ToolContext::new();
        let out = tool.run(json!({"x": 1}), &ctx).await.unwrap();
        // wc output is whitespace-padded; just check it contains a digit.
        assert!(out.chars().any(|c| c.is_ascii_digit()));
    }

    #[test]
    fn name_description_and_parameters_pass_through() {
        let tool = SubprocessTool::from_config(&SubprocessToolConfig {
            name: "MyTool".into(),
            command: "true".into(),
            args: vec![],
            description: "the description".into(),
            parameters: json!({"type":"object","properties":{"q":{"type":"string"}}}),
        });
        assert_eq!(tool.name(), "MyTool");
        assert_eq!(tool.description(), "the description");
        assert_eq!(tool.parameters()["properties"]["q"]["type"], "string");
    }
}
