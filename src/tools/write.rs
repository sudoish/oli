use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::Path;

use crate::error::{Result, ToolError};
use crate::tools::Tool;

pub struct Write;

#[async_trait]
impl Tool for Write {
    fn name(&self) -> &str {
        "Write"
    }

    fn description(&self) -> &str {
        "Write content to a file. Creates the file if it does not exist, overwrites it if it does."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The path of the file to write to"
                },
                "content": {
                    "type": "string",
                    "description": "The content to write to the file"
                }
            },
            "required": ["file_path", "content"]
        })
    }

    async fn run(&self, args: Value) -> Result<String> {
        let file_path = args["file_path"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArguments {
                tool: "Write".into(),
                detail: "missing or non-string `file_path`".into(),
            })?;
        let content = args["content"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArguments {
                tool: "Write".into(),
                detail: "missing or non-string `content`".into(),
            })?;

        match write_file(file_path, content).await {
            Ok(()) => Ok(format!("Successfully wrote to {}", file_path)),
            Err(e) => Ok(format!("Error writing {}: {}", file_path, e)),
        }
    }
}

async fn write_file(file_path: &str, content: &str) -> std::io::Result<()> {
    if let Some(parent) = Path::new(file_path).parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }
    tokio::fs::write(file_path, content).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn writes_file_and_reports_success() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hello.txt");
        let path_str = path.to_str().unwrap().to_string();

        let out = Write
            .run(json!({ "file_path": path_str, "content": "yo" }))
            .await
            .unwrap();
        assert!(out.starts_with("Successfully wrote to"));
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), "yo");
    }

    #[tokio::test]
    async fn creates_parent_directories() {
        let dir = tempdir().unwrap();
        let nested = dir.path().join("a/b/c.txt");
        let nested_str = nested.to_str().unwrap().to_string();

        Write
            .run(json!({ "file_path": nested_str, "content": "nested" }))
            .await
            .unwrap();
        assert_eq!(tokio::fs::read_to_string(&nested).await.unwrap(), "nested");
    }

    #[tokio::test]
    async fn missing_arg_is_invalid_args_error() {
        let err = Write
            .run(json!({ "file_path": "/tmp/x" }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("invalid arguments for Write"));
    }
}
