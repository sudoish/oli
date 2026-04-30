use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::process::Command;

use crate::error::{Result, ToolError};
use crate::tools::util::{DEFAULT_MAX_OUTPUT_BYTES, truncate_with_cache};
use crate::tools::{Tool, ToolContext};

pub struct Grep;

#[async_trait]
impl Tool for Grep {
    fn name(&self) -> &str {
        "Grep"
    }

    fn description(&self) -> &str {
        "Search files for a regex pattern using ripgrep. Returns matching \
         lines with file path and line numbers. Optional `path` to scope to \
         a directory and `glob` to filter file types."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern":          { "type": "string",  "description": "Regex pattern to search for" },
                "path":             { "type": "string",  "description": "Directory or file to search (default: cwd)" },
                "glob":             { "type": "string",  "description": "Glob pattern for file filter (e.g. \"*.rs\")" },
                "case_insensitive": { "type": "boolean", "description": "Case-insensitive match (default false)" }
            },
            "required": ["pattern"]
        })
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let pattern = args["pattern"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArguments {
                tool: "Grep".into(),
                detail: "missing or non-string `pattern`".into(),
            })?;
        let path = args.get("path").and_then(|v| v.as_str());
        let glob = args.get("glob").and_then(|v| v.as_str());
        let case_insensitive = args
            .get("case_insensitive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let mut cmd = Command::new("rg");
        cmd.arg("--no-heading")
            .arg("--line-number")
            .arg("--with-filename")
            .arg("--color=never");
        if case_insensitive {
            cmd.arg("--ignore-case");
        }
        if let Some(g) = glob {
            cmd.arg("--glob").arg(g);
        }
        cmd.arg("--").arg(pattern);
        if let Some(p) = path {
            cmd.arg(p);
        }

        let output = match cmd.output().await {
            Ok(o) => o,
            Err(e) => {
                return Ok(format!(
                    "Error running rg: {} (is ripgrep installed and on PATH?)",
                    e
                ));
            }
        };

        // rg exits 1 when no matches are found — that's a normal outcome,
        // not a tool error. Anything ≥2 is a real error (bad regex, etc.).
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let code = output.status.code().unwrap_or(-1);

        let body = match code {
            0 => stdout.into_owned(),
            1 => "No matches found.".to_string(),
            _ => format!("rg failed (exit {}): {}", code, stderr.trim()),
        };

        Ok(truncate_with_cache(ctx, &body, DEFAULT_MAX_OUTPUT_BYTES))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn rg_available() -> bool {
        std::process::Command::new("rg")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[tokio::test]
    async fn finds_a_known_pattern_in_a_tempdir() {
        if !rg_available() {
            eprintln!("skipping: rg not on PATH");
            return;
        }
        let dir = tempdir().unwrap();
        let p = dir.path().join("hello.txt");
        tokio::fs::write(&p, "needle\nhaystack\n").await.unwrap();

        let ctx = ToolContext::new();
        let out = Grep
            .run(
                json!({
                    "pattern": "needle",
                    "path": dir.path().to_str().unwrap()
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(out.contains("needle"));
        assert!(out.contains("hello.txt"));
    }

    #[tokio::test]
    async fn no_match_returns_friendly_message() {
        if !rg_available() {
            eprintln!("skipping: rg not on PATH");
            return;
        }
        let dir = tempdir().unwrap();
        tokio::fs::write(dir.path().join("a.txt"), "xyz\n")
            .await
            .unwrap();

        let ctx = ToolContext::new();
        let out = Grep
            .run(
                json!({
                    "pattern": "definitely-not-here",
                    "path": dir.path().to_str().unwrap()
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out, "No matches found.");
    }

    #[tokio::test]
    async fn case_insensitive_search() {
        if !rg_available() {
            eprintln!("skipping: rg not on PATH");
            return;
        }
        let dir = tempdir().unwrap();
        tokio::fs::write(dir.path().join("a.txt"), "Hello\n")
            .await
            .unwrap();

        let ctx = ToolContext::new();
        let out = Grep
            .run(
                json!({
                    "pattern": "hello",
                    "path": dir.path().to_str().unwrap(),
                    "case_insensitive": true
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(out.contains("Hello"));
    }

    #[tokio::test]
    async fn glob_filter_narrows_files() {
        if !rg_available() {
            eprintln!("skipping: rg not on PATH");
            return;
        }
        let dir = tempdir().unwrap();
        tokio::fs::write(dir.path().join("a.rs"), "needle in rust\n")
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("b.txt"), "needle in txt\n")
            .await
            .unwrap();

        let ctx = ToolContext::new();
        let out = Grep
            .run(
                json!({
                    "pattern": "needle",
                    "path": dir.path().to_str().unwrap(),
                    "glob": "*.rs"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(out.contains("a.rs"));
        assert!(!out.contains("b.txt"));
    }
}
