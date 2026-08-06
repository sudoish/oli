//! `McpTool` — adapts one MCP server-side tool into the harness's
//! `Tool` trait. The agent loop, the policy gate, and the hook
//! dispatcher don't know they're talking to a remote process.
//!
//! Naming is namespaced (`<server>__<tool>`) so two servers can both
//! ship a `get_issue` without collision. The display name in `/tools`
//! shows both.

use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::error::Result;
use crate::mcp::server::{McpServer, ToolMeta};
use crate::tools::{Tool, ToolContext};

pub struct McpTool {
    server: Arc<Mutex<McpServer>>,
    namespaced_name: String,
    bare_name: String,
    description: String,
    input_schema: Value,
}

impl McpTool {
    pub fn new(server: Arc<Mutex<McpServer>>, server_name: String, meta: ToolMeta) -> Self {
        let namespaced_name = format!("{}__{}", server_name, meta.name);
        Self {
            server,
            namespaced_name,
            bare_name: meta.name,
            description: meta.description,
            input_schema: meta.input_schema,
        }
    }
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.namespaced_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> Value {
        self.input_schema.clone()
    }

    async fn run(&self, args: Value, _ctx: &ToolContext) -> Result<String> {
        let server = self.server.lock().await;
        match server.call_tool(&self.bare_name, args).await {
            Ok(result) => Ok(format_tool_result(&result)),
            // Server-side errors come back as `AgentError::Provider`
            // strings; surface them as tool result text the model can
            // react to rather than aborting the agent loop.
            Err(e) => Ok(format!("MCP error: {}", e)),
        }
    }
}

/// Convert an MCP `tools/call` result into a string the model can read.
/// MCP wraps text in a `content` array of `{type, text}` blocks; we
/// concatenate the text blocks and ignore non-text content for now.
/// `isError: true` results are prefixed so the model knows the call
/// failed semantically (vs. transport-level failure).
fn format_tool_result(result: &Value) -> String {
    let is_error = result
        .get("isError")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let content = result.get("content").and_then(|v| v.as_array());
    let text = match content {
        Some(blocks) => {
            let mut out = String::new();
            for block in blocks {
                let kind = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match kind {
                    "text" => {
                        if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                            if !out.is_empty() {
                                out.push('\n');
                            }
                            out.push_str(t);
                        }
                    }
                    other => {
                        // image / resource / etc — surface a marker so
                        // the model knows non-text content was returned
                        // without us having to forward bytes.
                        if !out.is_empty() {
                            out.push('\n');
                        }
                        out.push_str(&format!("(non-text content: {})", other));
                    }
                }
            }
            out
        }
        None => {
            // Some servers return `result` as a bare value rather than
            // the spec's content-array shape. Fall back to JSON.
            serde_json::to_string(result).unwrap_or_else(|_| "(unprintable)".into())
        }
    };
    if is_error {
        format!("(server reported error)\n{}", text)
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn format_text_blocks_concatenated_with_newline() {
        let v = json!({
            "content": [
                {"type": "text", "text": "first"},
                {"type": "text", "text": "second"},
            ]
        });
        assert_eq!(format_tool_result(&v), "first\nsecond");
    }

    #[test]
    fn format_marks_is_error_results() {
        let v = json!({
            "isError": true,
            "content": [
                {"type": "text", "text": "boom"},
            ]
        });
        let out = format_tool_result(&v);
        assert!(out.starts_with("(server reported error)"));
        assert!(out.contains("boom"));
    }

    #[test]
    fn format_non_text_blocks_get_marker() {
        let v = json!({
            "content": [
                {"type": "image", "data": "..."},
            ]
        });
        let out = format_tool_result(&v);
        assert!(out.contains("non-text content: image"));
    }

    #[test]
    fn format_falls_back_to_json_for_legacy_shape() {
        let v = json!({"plain": "result"});
        let out = format_tool_result(&v);
        assert!(out.contains("plain"));
        assert!(out.contains("result"));
    }

    #[test]
    fn name_is_namespaced_with_double_underscore() {
        // Tool::run requires a live server, so we only inspect the
        // metadata accessors here.
        let server = Arc::new(Mutex::new(McpServer {
            name: "linear".into(),
            cfg: crate::mcp::config::McpServerConfig {
                kind: crate::mcp::config::McpTransportKind::Stdio,
                command: Some("true".into()),
                args: vec![],
                env: Default::default(),
                url: None,
                headers: Default::default(),
                init_timeout_ms: 5000,
                call_timeout_ms: 60000,
                tools: Default::default(),
                enabled: true,
            },
            transport: None,
            capabilities: Default::default(),
            tools: vec![],
            health: crate::mcp::HealthState::Healthy,
            stderr_source: None,
        }));
        let t = McpTool::new(
            server,
            "linear".into(),
            ToolMeta {
                name: "get_issue".into(),
                description: "fetch one".into(),
                input_schema: json!({"type": "object"}),
            },
        );
        assert_eq!(t.name(), "linear__get_issue");
        assert_eq!(t.description(), "fetch one");
    }
}
