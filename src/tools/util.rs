/// Truncate a string at a UTF-8 char boundary, appending a marker that
/// includes the original size so the model can react appropriately.
///
/// Returns the input unchanged if it's already within `max_bytes`.
pub fn truncate(s: &str, max_bytes: usize) -> String {
    truncate_inner(s, max_bytes, |cut, total| {
        format!("[... output truncated, {} of {} bytes shown ...]", cut, total)
    })
}

/// Same as `truncate`, but also stashes the full body in the
/// `ToolContext`'s result cache so the model can pull deeper
/// detail via the `ShowFull` tool. The truncation marker
/// includes the cache id so the model knows what to call.
///
/// Returns the input unchanged if it's already within
/// `max_bytes` — no caching, no marker.
pub fn truncate_with_cache(ctx: &crate::tools::ToolContext, s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    // Cache before truncating: the marker has to name the id.
    let id = ctx.cache_full_result(s.to_string());
    truncate_inner(s, max_bytes, |cut, total| {
        format!(
            "[... output truncated, {} of {} bytes shown — call ShowFull(id={}, offset={}) for more ...]",
            cut, total, id, cut
        )
    })
}

/// Shared body: walk back to a char boundary, keep the prefix newline-
/// terminated, and append whatever marker the caller builds from the
/// shown/total byte counts.
fn truncate_inner(s: &str, max_bytes: usize, marker: impl FnOnce(usize, usize) -> String) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut cut = max_bytes;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    let marker = marker(cut, s.len());
    let mut out = String::with_capacity(cut + marker.len() + 1);
    out.push_str(&s[..cut]);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&marker);
    out
}

/// Default cap for tool output bodies. Big enough to be useful, small
/// enough to not blow up a 7B model's context.
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 30_000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_when_short_enough() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncates_with_marker_and_byte_counts() {
        let big = "x".repeat(100);
        let out = truncate(&big, 10);
        assert!(out.starts_with(&"x".repeat(10)));
        assert!(out.contains("[... output truncated, 10 of 100 bytes shown ...]"));
    }

    #[test]
    fn cuts_on_char_boundary() {
        // 'é' is 2 bytes in UTF-8. Asking for max=1 should not panic; we step
        // back to byte 0.
        let s = "é";
        let out = truncate(s, 1);
        assert!(out.contains("output truncated"));
    }

    #[tokio::test]
    async fn truncate_with_cache_passes_through_when_short_enough() {
        use crate::tools::ToolContext;
        let ctx = ToolContext::new();
        let out = truncate_with_cache(&ctx, "hello", 100);
        assert_eq!(out, "hello");
    }

    #[tokio::test]
    async fn truncate_with_cache_stashes_full_body_and_emits_show_full_marker() {
        use crate::tools::ToolContext;
        let ctx = ToolContext::new();
        let big = "x".repeat(10_000);
        let out = truncate_with_cache(&ctx, &big, 100);
        // Marker must mention ShowFull and an id.
        assert!(out.contains("call ShowFull(id="));
        // Pull the id out of the marker, then read the cache
        // back and verify the round-trip.
        let id_str = out
            .split("ShowFull(id=")
            .nth(1)
            .and_then(|s| s.split(',').next())
            .expect("id in marker");
        let id: u64 = id_str.parse().expect("numeric id");
        let full = ctx.read_full_result(id, 0, 0).expect("cache hit");
        assert_eq!(full, big);
    }
}
