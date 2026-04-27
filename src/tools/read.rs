use async_trait::async_trait;
use serde_json::{Value, json};

use crate::error::{Result, ToolError};
use crate::tools::util::{DEFAULT_MAX_OUTPUT_BYTES, truncate};
use crate::tools::{Tool, ToolContext};

pub struct Read;

#[async_trait]
impl Tool for Read {
    fn name(&self) -> &str {
        "Read"
    }

    fn description(&self) -> &str {
        "Read and return the contents of a file. Optional `offset` (1-indexed line) and `limit` (line count) for paginating large files."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The path to the file to read"
                },
                "offset": {
                    "type": "integer",
                    "description": "1-indexed line number to start reading from"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to read"
                }
            },
            "required": ["file_path"]
        })
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let file_path = args["file_path"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArguments {
                tool: "Read".into(),
                detail: "missing or non-string `file_path`".into(),
            })?;

        let offset = args
            .get("offset")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);

        let body = match tokio::fs::read_to_string(file_path).await {
            Ok(s) => s,
            Err(e) => return Ok(format!("Error reading {}: {}", file_path, e)),
        };

        ctx.mark_read(file_path).await;

        let output = match (offset, limit) {
            (None, None) => body,
            _ => {
                let start = offset.unwrap_or(1).saturating_sub(1);
                let take = limit.unwrap_or(usize::MAX);
                body.lines()
                    .skip(start)
                    .take(take)
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        };

        Ok(truncate(&output, DEFAULT_MAX_OUTPUT_BYTES))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn reads_existing_file_and_records_in_ctx() {
        let mut f = NamedTempFile::new().unwrap();
        write!(f, "hello world").unwrap();
        let path = f.path().to_str().unwrap().to_string();

        let ctx = ToolContext::new();
        let out = Read.run(json!({ "file_path": path }), &ctx).await.unwrap();
        assert_eq!(out, "hello world");
        assert!(ctx.was_read(&f.path()).await);
    }

    #[tokio::test]
    async fn missing_file_returns_error_string_and_does_not_mark_read() {
        let ctx = ToolContext::new();
        let out = Read
            .run(
                json!({ "file_path": "/tmp/__definitely_not_a_real_file__" }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(out.starts_with("Error reading"));
        assert!(!ctx.was_read("/tmp/__definitely_not_a_real_file__").await);
    }

    #[tokio::test]
    async fn missing_argument_is_invalid_args_error() {
        let ctx = ToolContext::new();
        let err = Read.run(json!({}), &ctx).await.unwrap_err();
        assert!(err.to_string().contains("invalid arguments for Read"));
    }

    #[tokio::test]
    async fn offset_and_limit_paginate_lines() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "L1\nL2\nL3\nL4\nL5").unwrap();
        let path = f.path().to_str().unwrap().to_string();

        let ctx = ToolContext::new();
        let out = Read
            .run(json!({ "file_path": path, "offset": 2, "limit": 2 }), &ctx)
            .await
            .unwrap();
        assert_eq!(out, "L2\nL3");
    }

    #[tokio::test]
    async fn offset_alone_skips_to_line() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "L1\nL2\nL3").unwrap();
        let path = f.path().to_str().unwrap().to_string();

        let ctx = ToolContext::new();
        let out = Read
            .run(json!({ "file_path": path, "offset": 3 }), &ctx)
            .await
            .unwrap();
        assert_eq!(out, "L3");
    }

    #[tokio::test]
    async fn truncates_oversized_files() {
        let mut f = NamedTempFile::new().unwrap();
        let big = "x".repeat(crate::tools::util::DEFAULT_MAX_OUTPUT_BYTES + 1000);
        f.write_all(big.as_bytes()).unwrap();
        let path = f.path().to_str().unwrap().to_string();

        let ctx = ToolContext::new();
        let out = Read.run(json!({ "file_path": path }), &ctx).await.unwrap();
        assert!(out.contains("[... output truncated"));
    }
}
