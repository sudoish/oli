//! `ShowFull` — pull more bytes from a tool result that hit the
//! 30 KB truncation cap. Every tool that uses
//! [`util::truncate_with_cache`] stashes the full body in the
//! `ToolContext` result cache and embeds the resulting id in the
//! truncation marker. The model invokes this tool with that id
//! when it actually needs the deeper content, instead of every
//! tool call blanket-loading the full body into the context
//! window.
//!
//! [`util::truncate_with_cache`]: crate::tools::util::truncate_with_cache

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::error::{Result, ToolError};
use crate::tools::util::{DEFAULT_MAX_OUTPUT_BYTES, truncate_with_cache};
use crate::tools::{Tool, ToolContext};

/// Default size of the slice returned per `ShowFull` call. The
/// model can pass `limit` to override; values larger than the
/// agent's truncation cap re-truncate (and stash a fresh cache
/// entry, which the marker reflects).
const DEFAULT_LIMIT: usize = 20_000;

pub struct ShowFull;

#[async_trait]
impl Tool for ShowFull {
    fn name(&self) -> &str {
        "ShowFull"
    }

    fn description(&self) -> &str {
        "Pull more bytes from a previous tool result that was truncated. \
         The truncation marker on the original result includes an \
         `id`; pass that id here. Use `offset` to skip past content \
         already seen and `limit` to control how much to fetch."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "integer",
                    "description": "Cache id from the prior truncation marker."
                },
                "offset": {
                    "type": "integer",
                    "description": "Byte offset to start from. Default 0; pass the value the marker reported (the bytes-shown count) to continue from where truncation stopped."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum bytes to return. Default 20000. Larger windows still re-truncate at the agent's per-call cap."
                }
            },
            "required": ["id"]
        })
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let id =
            args.get("id")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| ToolError::InvalidArguments {
                    tool: "ShowFull".into(),
                    detail: "missing or non-integer `id`".into(),
                })?;
        let offset = args
            .get("offset")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(0);
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_LIMIT);

        let slice =
            ctx.read_full_result(id, offset, limit)
                .ok_or_else(|| ToolError::InvalidArguments {
                    tool: "ShowFull".into(),
                    detail: format!(
                        "cache id {} unknown (already evicted, or never recorded)",
                        id
                    ),
                })?;

        if slice.is_empty() {
            return Ok(format!(
                "[ShowFull(id={}): offset {} is at or past end of body]",
                id, offset
            ));
        }

        // Even ShowFull can hit the per-call truncation cap when
        // the user asks for more than fits — re-truncate and
        // recurse the marker so the model can keep paginating.
        Ok(truncate_with_cache(ctx, &slice, DEFAULT_MAX_OUTPUT_BYTES))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn show_full_returns_slice_at_offset() {
        let ctx = ToolContext::new();
        let body = "abcdefghij".repeat(1_000); // 10_000 bytes
        let id = ctx.cache_full_result(body.clone());

        let tool = ShowFull;
        let out = tool
            .run(json!({"id": id, "offset": 100, "limit": 20}), &ctx)
            .await
            .unwrap();
        assert_eq!(out, &body[100..120]);
    }

    #[tokio::test]
    async fn show_full_with_default_offset_returns_first_window() {
        let ctx = ToolContext::new();
        let body = "abcdef".repeat(2_000); // 12_000 bytes
        let id = ctx.cache_full_result(body.clone());

        let tool = ShowFull;
        let out = tool
            .run(json!({"id": id, "limit": 50}), &ctx)
            .await
            .unwrap();
        assert_eq!(out.len(), 50);
        assert_eq!(out, &body[..50]);
    }

    #[tokio::test]
    async fn show_full_unknown_id_returns_invalid_arguments() {
        let ctx = ToolContext::new();
        let tool = ShowFull;
        let err = tool.run(json!({"id": 9999}), &ctx).await.unwrap_err();
        assert!(err.to_string().contains("unknown"));
    }

    #[tokio::test]
    async fn show_full_offset_past_end_returns_explanatory_string() {
        let ctx = ToolContext::new();
        let id = ctx.cache_full_result("short".into());
        let tool = ShowFull;
        let out = tool
            .run(json!({"id": id, "offset": 1000}), &ctx)
            .await
            .unwrap();
        assert!(out.contains("at or past end of body"));
    }

    #[tokio::test]
    async fn show_full_re_truncates_when_slice_exceeds_cap() {
        // Body big enough that even the requested window
        // (50 KB) exceeds the per-call cap (30 KB). The output
        // should re-truncate with a fresh marker.
        let ctx = ToolContext::new();
        let body = "x".repeat(80_000);
        let id = ctx.cache_full_result(body);
        let tool = ShowFull;
        let out = tool
            .run(json!({"id": id, "offset": 0, "limit": 50_000}), &ctx)
            .await
            .unwrap();
        assert!(out.contains("[... output truncated"));
        assert!(out.contains("ShowFull(id="));
    }

    #[tokio::test]
    async fn missing_id_returns_invalid_arguments() {
        let ctx = ToolContext::new();
        let tool = ShowFull;
        let err = tool.run(json!({}), &ctx).await.unwrap_err();
        assert!(err.to_string().contains("missing or non-integer `id`"));
    }
}
