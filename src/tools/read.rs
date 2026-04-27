use async_trait::async_trait;
use serde_json::{Value, json};

use crate::error::{Result, ToolError};
use crate::tools::Tool;

pub struct Read;

#[async_trait]
impl Tool for Read {
    fn name(&self) -> &str {
        "Read"
    }

    fn description(&self) -> &str {
        "Read and return the contents of a file"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The path to the file to read"
                }
            },
            "required": ["file_path"]
        })
    }

    async fn run(&self, args: Value) -> Result<String> {
        let file_path = args["file_path"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArguments {
                tool: "Read".into(),
                detail: "missing or non-string `file_path`".into(),
            })?;

        match tokio::fs::read_to_string(file_path).await {
            Ok(s) => Ok(s),
            Err(e) => Ok(format!("Error reading {}: {}", file_path, e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn reads_existing_file() {
        let mut f = NamedTempFile::new().unwrap();
        write!(f, "hello world").unwrap();
        let path = f.path().to_str().unwrap().to_string();

        let out = Read.run(json!({ "file_path": path })).await.unwrap();
        assert_eq!(out, "hello world");
    }

    #[tokio::test]
    async fn missing_file_returns_error_string_not_failure() {
        // Behavior parity with original: errors come back as a tool result string
        // so the agent can react to them rather than aborting the loop.
        let out = Read
            .run(json!({ "file_path": "/tmp/__definitely_not_a_real_file__" }))
            .await
            .unwrap();
        assert!(out.starts_with("Error reading"));
    }

    #[tokio::test]
    async fn missing_argument_is_invalid_args_error() {
        let err = Read.run(json!({})).await.unwrap_err();
        assert!(err.to_string().contains("invalid arguments for Read"));
    }
}
