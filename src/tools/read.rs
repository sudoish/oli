use async_trait::async_trait;
use serde_json::{Value, json};

use crate::error::{Result, ToolError};
use crate::tools::util::{DEFAULT_MAX_OUTPUT_BYTES, truncate};
use crate::tools::{Tool, ToolContext};

/// File extensions that produce an `[Image: ...]` marker instead of a
/// `read_to_string` attempt. The marker is a useful signal to the
/// model that the path *is* an image, vs. the
/// `read_to_string` UTF-8 error we'd otherwise produce.
pub const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp"];

pub struct Read;

/// Build the `[Image: <abs-path> <WxH|?> <FORMAT>]` marker returned to the model.
pub fn format_image_marker(abs_path: &str, format: &str, dims: Option<(u32, u32)>) -> String {
    let size = match dims {
        Some((w, h)) => format!("{}x{}", w, h),
        None => "?x?".into(),
    };
    format!(
        "[Image: {} {} {}]",
        abs_path,
        size,
        format.to_ascii_uppercase()
    )
}

/// Cheap dimension probe for the marker. Reads only the first few KB
/// of the file and pattern-matches on PNG / JPEG / GIF magic bytes.
/// Returns `None` on any unrecognized header — the marker then uses
/// `?x?` instead of fabricating a size. This keeps `read.rs` free of
/// a heavyweight image-decoding dependency.
fn image_dimensions(path: &str) -> Option<(u32, u32)> {
    use std::fs::File;
    use std::io::{BufReader, Read as _};
    let mut buf = [0u8; 4096];
    let mut f = BufReader::new(File::open(path).ok()?);
    let n = f.read(&mut buf).ok()?;
    let bytes = &buf[..n];
    // PNG: 8-byte signature, then 8-byte IHDR length+type, then 4-byte W, 4-byte H.
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.len() >= 24 {
        let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
        let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
        return Some((w, h));
    }
    // GIF: 6-byte sig "GIF87a"/"GIF89a", then 2-byte LE width, 2-byte LE height.
    if (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) && bytes.len() >= 10 {
        let w = u16::from_le_bytes(bytes[6..8].try_into().ok()?) as u32;
        let h = u16::from_le_bytes(bytes[8..10].try_into().ok()?) as u32;
        return Some((w, h));
    }
    // BMP: "BM", 18 bytes in, 4-byte LE width, 4-byte LE height (signed).
    if bytes.starts_with(b"BM") && bytes.len() >= 26 {
        let w = i32::from_le_bytes(bytes[18..22].try_into().ok()?).unsigned_abs();
        let h = i32::from_le_bytes(bytes[22..26].try_into().ok()?).unsigned_abs();
        return Some((w, h));
    }
    // JPEG: 0xFF 0xD8 0xFF. Walk segments looking for SOF0/SOF2 marker.
    if bytes.len() >= 4 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF {
        let mut i = 2;
        while i + 9 < bytes.len() {
            if bytes[i] != 0xFF {
                return None;
            }
            let marker = bytes[i + 1];
            // SOF0 (baseline) / SOF1 / SOF2 (progressive) carry dims.
            if matches!(marker, 0xC0 | 0xC1 | 0xC2) {
                let h = u16::from_be_bytes(bytes[i + 5..i + 7].try_into().ok()?) as u32;
                let w = u16::from_be_bytes(bytes[i + 7..i + 9].try_into().ok()?) as u32;
                return Some((w, h));
            }
            let seg_len = u16::from_be_bytes(bytes[i + 2..i + 4].try_into().ok()?) as usize;
            i += 2 + seg_len;
        }
    }
    // WebP: "RIFF....WEBP" + chunk. Skip dims parsing for now.
    None
}

fn image_format_for_extension(ext: &str) -> Option<String> {
    let lower = ext.to_ascii_lowercase();
    if !IMAGE_EXTENSIONS.contains(&lower.as_str()) {
        return None;
    }
    Some(match lower.as_str() {
        "jpg" => "jpeg".into(),
        _ => lower,
    })
}

#[async_trait]
impl Tool for Read {
    fn name(&self) -> &str {
        "Read"
    }

    fn description(&self) -> &str {
        "Read and return the contents of a file. Optional `offset` (1-indexed line) and `limit` (line count) for paginating large files. Image files (PNG/JPEG/GIF/WEBP/BMP) return a text marker instead of raw bytes."
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

        // Phase Y3: image files don't round-trip through `read_to_string`
        // (binary → invalid UTF-8). Detect by extension and emit a
        // standardized text marker for non-binary model context.
        let ext = std::path::Path::new(file_path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if let Some(format) = image_format_for_extension(ext) {
            let abs = tokio::fs::canonicalize(file_path)
                .await
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| file_path.to_string());
            let dims = tokio::task::spawn_blocking({
                let abs = abs.clone();
                move || image_dimensions(&abs)
            })
            .await
            .ok()
            .flatten();
            ctx.mark_read(file_path).await;
            return Ok(format_image_marker(&abs, &format, dims));
        }

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
    async fn png_file_emits_image_marker_with_dimensions() {
        // Smallest valid PNG: 1x1 transparent pixel. Hand-crafted bytes
        // — keeps the test free of the `image` crate dep.
        let png_bytes: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // signature
            0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR length + type
            0x00, 0x00, 0x00, 0x01, // width = 1
            0x00, 0x00, 0x00, 0x01, // height = 1
            0x08, 0x06, 0x00, 0x00, 0x00, // bit depth + color type + ...
            0x1F, 0x15, 0xC4, 0x89, // CRC (not validated by our probe)
        ];
        let mut f = NamedTempFile::with_suffix(".png").unwrap();
        f.write_all(png_bytes).unwrap();
        let path = f.path().to_str().unwrap().to_string();

        let ctx = ToolContext::new();
        let out = Read.run(json!({ "file_path": path }), &ctx).await.unwrap();
        assert!(out.starts_with("[Image: "), "expected marker, got {out}");
        assert!(out.contains(" 1x1 PNG]"), "expected 1x1 PNG, got {out}");
        assert!(ctx.was_read(&f.path()).await);
    }

    #[tokio::test]
    async fn jpeg_extension_emits_image_marker_unknown_dims_when_unrecognized() {
        // A tiny non-image file with a .jpg extension. The dimension
        // probe should fail-soft to `?x?` rather than refusing to emit
        // the marker.
        let mut f = NamedTempFile::with_suffix(".jpg").unwrap();
        f.write_all(b"not really a jpeg").unwrap();
        let path = f.path().to_str().unwrap().to_string();

        let ctx = ToolContext::new();
        let out = Read.run(json!({ "file_path": path }), &ctx).await.unwrap();
        assert!(out.starts_with("[Image: "));
        assert!(out.contains(" ?x? JPEG]"), "got {out}");
    }

    #[tokio::test]
    async fn image_marker_uses_absolute_path() {
        let png_bytes: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x03, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        let mut f = NamedTempFile::with_suffix(".png").unwrap();
        f.write_all(png_bytes).unwrap();
        let path = f.path().to_str().unwrap().to_string();
        let ctx = ToolContext::new();
        let out = Read.run(json!({ "file_path": path }), &ctx).await.unwrap();
        // Marker contains an absolute path (starts with `/` on unix).
        let after_image = out.strip_prefix("[Image: ").unwrap();
        assert!(after_image.starts_with('/'), "expected abs path, got {out}");
        assert!(out.contains(" 2x3 PNG]"));
    }

    #[test]
    fn format_image_marker_handles_known_format() {
        assert_eq!(
            format_image_marker("/tmp/x.png", "png", Some((10, 20))),
            "[Image: /tmp/x.png 10x20 PNG]"
        );
        assert_eq!(
            format_image_marker("/tmp/y.jpg", "jpeg", None),
            "[Image: /tmp/y.jpg ?x? JPEG]"
        );
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
