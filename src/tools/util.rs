/// Truncate a string at a UTF-8 char boundary, appending a marker that
/// includes the original size so the model can react appropriately.
///
/// Returns the input unchanged if it's already within `max_bytes`.
pub fn truncate(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut cut = max_bytes;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut out = String::with_capacity(cut + 64);
    out.push_str(&s[..cut]);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!(
        "[... output truncated, {} of {} bytes shown ...]",
        cut,
        s.len()
    ));
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
}
