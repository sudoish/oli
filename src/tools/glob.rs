use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::PathBuf;

use crate::error::{Result, ToolError};
use crate::tools::util::{DEFAULT_MAX_OUTPUT_BYTES, truncate};
use crate::tools::{Tool, ToolContext};

pub struct Glob;

#[async_trait]
impl Tool for Glob {
    fn name(&self) -> &str {
        "Glob"
    }

    fn description(&self) -> &str {
        "List files matching a glob pattern (e.g. \"**/*.rs\"). Optional \
         `path` to scope to a directory (default: cwd)."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern, e.g. \"**/*.rs\"" },
                "path":    { "type": "string", "description": "Base directory (default: cwd)" }
            },
            "required": ["pattern"]
        })
    }

    async fn run(&self, args: Value, _ctx: &ToolContext) -> Result<String> {
        let pattern = args["pattern"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArguments {
                tool: "Glob".into(),
                detail: "missing or non-string `pattern`".into(),
            })?;
        let base = args.get("path").and_then(|v| v.as_str());

        // Combine base + pattern. The `glob` crate doesn't take a base dir,
        // so we prepend it to the pattern. If `pattern` is already absolute,
        // we honor that and ignore `path`.
        let combined = match base {
            Some(b) if !pattern.starts_with('/') => {
                format!("{}/{}", b.trim_end_matches('/'), pattern)
            }
            _ => pattern.to_string(),
        };

        let entries = match glob::glob(&combined) {
            Ok(it) => it,
            Err(e) => {
                return Ok(format!("Error: invalid glob pattern: {}", e));
            }
        };

        let mut paths: Vec<PathBuf> = entries.filter_map(|r| r.ok()).collect();
        paths.sort();

        if paths.is_empty() {
            return Ok("No files matched.".to_string());
        }

        let body = paths
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");

        Ok(truncate(&body, DEFAULT_MAX_OUTPUT_BYTES))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn matches_files_under_a_base_path() {
        let dir = tempdir().unwrap();
        tokio::fs::write(dir.path().join("a.rs"), "").await.unwrap();
        tokio::fs::write(dir.path().join("b.rs"), "").await.unwrap();
        tokio::fs::write(dir.path().join("c.txt"), "")
            .await
            .unwrap();

        let ctx = ToolContext::new();
        let out = Glob
            .run(
                json!({
                    "pattern": "*.rs",
                    "path": dir.path().to_str().unwrap()
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(out.contains("a.rs"));
        assert!(out.contains("b.rs"));
        assert!(!out.contains("c.txt"));
    }

    #[tokio::test]
    async fn recursive_glob_descends_into_subdirs() {
        let dir = tempdir().unwrap();
        tokio::fs::create_dir_all(dir.path().join("sub"))
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("sub/x.rs"), "")
            .await
            .unwrap();

        let ctx = ToolContext::new();
        let out = Glob
            .run(
                json!({
                    "pattern": "**/*.rs",
                    "path": dir.path().to_str().unwrap()
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(out.contains("sub/x.rs") || out.contains("sub\\x.rs"));
    }

    #[tokio::test]
    async fn no_match_returns_friendly_message() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new();
        let out = Glob
            .run(
                json!({
                    "pattern": "*.absolutely-no-match",
                    "path": dir.path().to_str().unwrap()
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out, "No files matched.");
    }

    #[tokio::test]
    async fn invalid_pattern_is_error_message() {
        let ctx = ToolContext::new();
        let out = Glob
            .run(json!({ "pattern": "[unclosed" }), &ctx)
            .await
            .unwrap();
        assert!(out.contains("Error: invalid glob pattern"));
    }
}
