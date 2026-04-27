use async_trait::async_trait;
use serde_json::{Value, json};

use crate::error::{Result, ToolError};
use crate::tools::{Tool, ToolContext};

pub struct Edit;

#[async_trait]
impl Tool for Edit {
    fn name(&self) -> &str {
        "Edit"
    }

    fn description(&self) -> &str {
        "Replace `old_string` with `new_string` in a file. The file must have \
         been Read first in this session. Fails if `old_string` is not found, \
         or appears multiple times unless `replace_all` is true."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path":  { "type": "string",  "description": "Absolute or relative path to the file" },
                "old_string": { "type": "string",  "description": "Exact string to replace" },
                "new_string": { "type": "string",  "description": "Replacement string" },
                "replace_all":{ "type": "boolean", "description": "Replace every occurrence (default false)" }
            },
            "required": ["file_path", "old_string", "new_string"]
        })
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let file_path = args["file_path"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArguments {
                tool: "Edit".into(),
                detail: "missing or non-string `file_path`".into(),
            })?;
        let old_string =
            args["old_string"]
                .as_str()
                .ok_or_else(|| ToolError::InvalidArguments {
                    tool: "Edit".into(),
                    detail: "missing or non-string `old_string`".into(),
                })?;
        let new_string =
            args["new_string"]
                .as_str()
                .ok_or_else(|| ToolError::InvalidArguments {
                    tool: "Edit".into(),
                    detail: "missing or non-string `new_string`".into(),
                })?;
        let replace_all = args
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if !ctx.was_read(file_path).await {
            return Ok(format!(
                "Error: file {} has not been read this session. Call Read first.",
                file_path
            ));
        }

        let body = match tokio::fs::read_to_string(file_path).await {
            Ok(s) => s,
            Err(e) => return Ok(format!("Error reading {}: {}", file_path, e)),
        };

        let occurrences = body.matches(old_string).count();
        if occurrences == 0 {
            return Ok(format!("Error: old_string not found in {}", file_path));
        }
        if occurrences > 1 && !replace_all {
            return Ok(format!(
                "Error: old_string occurs {} times in {} (set replace_all=true to replace every match)",
                occurrences, file_path
            ));
        }

        let updated = if replace_all {
            body.replace(old_string, new_string)
        } else {
            // Single occurrence — replace once. `replacen(_, _, 1)` does exactly that.
            body.replacen(old_string, new_string, 1)
        };

        if let Err(e) = tokio::fs::write(file_path, &updated).await {
            return Ok(format!("Error writing {}: {}", file_path, e));
        }

        Ok(format!(
            "Successfully edited {} ({} replacement{})",
            file_path,
            if replace_all { occurrences } else { 1 },
            if !replace_all || occurrences == 1 {
                ""
            } else {
                "s"
            }
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use tempfile::NamedTempFile;

    fn write_file(contents: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        f
    }

    #[tokio::test]
    async fn refuses_edit_when_file_was_not_read_first() {
        let f = write_file("hello world");
        let path = f.path().to_str().unwrap().to_string();
        let ctx = ToolContext::new();

        let out = Edit
            .run(
                json!({
                    "file_path": path,
                    "old_string": "hello",
                    "new_string": "goodbye"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(out.contains("has not been read this session"));
    }

    #[tokio::test]
    async fn happy_path_after_read() {
        let f = write_file("hello world");
        let path = f.path().to_str().unwrap().to_string();
        let ctx = ToolContext::new();
        ctx.mark_read(&path).await;

        let out = Edit
            .run(
                json!({
                    "file_path": path,
                    "old_string": "world",
                    "new_string": "harness"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(out.starts_with("Successfully edited"));
        assert_eq!(
            tokio::fs::read_to_string(f.path()).await.unwrap(),
            "hello harness"
        );
    }

    #[tokio::test]
    async fn old_string_not_found_returns_error_message() {
        let f = write_file("hello world");
        let path = f.path().to_str().unwrap().to_string();
        let ctx = ToolContext::new();
        ctx.mark_read(&path).await;

        let out = Edit
            .run(
                json!({
                    "file_path": path,
                    "old_string": "nope",
                    "new_string": "x"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(out.contains("old_string not found"));
    }

    #[tokio::test]
    async fn refuses_when_not_unique_without_replace_all() {
        let f = write_file("aa\nbb\naa\n");
        let path = f.path().to_str().unwrap().to_string();
        let ctx = ToolContext::new();
        ctx.mark_read(&path).await;

        let out = Edit
            .run(
                json!({
                    "file_path": path,
                    "old_string": "aa",
                    "new_string": "X"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(out.contains("occurs 2 times"));
        // Original content unchanged
        assert_eq!(
            tokio::fs::read_to_string(f.path()).await.unwrap(),
            "aa\nbb\naa\n"
        );
    }

    #[tokio::test]
    async fn replace_all_replaces_every_occurrence() {
        let f = write_file("aa\nbb\naa\n");
        let path = f.path().to_str().unwrap().to_string();
        let ctx = ToolContext::new();
        ctx.mark_read(&path).await;

        let out = Edit
            .run(
                json!({
                    "file_path": path,
                    "old_string": "aa",
                    "new_string": "X",
                    "replace_all": true
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(out.contains("2 replacements"));
        assert_eq!(
            tokio::fs::read_to_string(f.path()).await.unwrap(),
            "X\nbb\nX\n"
        );
    }
}
