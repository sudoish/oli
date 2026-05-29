//! Thin wrapper around `nucleo-matcher` for the ranking we need
//! across slash autocomplete, `@path` completion, and the various
//! pickers (`/sessions`, `/model`, `/provider`, Ctrl-R history).
//!
//! The matcher itself is a stateful, reusable struct. We allocate
//! one per `rank()` call for simplicity — the caller can hold a
//! cached `Matcher` and use the lower-level API if it ever shows
//! up in a hot loop. Today's call sites are interactive (one query
//! per keystroke against ≤ a few hundred items), so the per-call
//! allocation cost isn't worth optimizing.
//!
//! Tie-break: nucleo scores already favor tighter matches, but for
//! the edge case of two items with identical scores we prefer the
//! shorter haystack (so `/help` beats `/help-debug` on query
//! `help`).

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// Rank `items` by fuzzy match against `query`. Returns
/// `(original_index, score)` pairs, highest-score first. Items
/// that don't match are filtered out. An empty `query` returns
/// all items in their original order with score 0.
pub fn rank<T>(query: &str, items: &[T], key: impl Fn(&T) -> &str) -> Vec<(usize, u16)> {
    if query.is_empty() {
        return items.iter().enumerate().map(|(i, _)| (i, 0u16)).collect();
    }

    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);

    let mut scored: Vec<(usize, u16, usize)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| {
            let s = key(item);
            let mut buf = Vec::new();
            let haystack = Utf32Str::new(s, &mut buf);
            let score = pattern.score(haystack, &mut matcher)?;
            Some((i, score.min(u16::MAX as u32) as u16, s.chars().count()))
        })
        .collect();

    // Higher score first; then shorter haystack first (tighter
    // match); then earlier index first (stable for equal items).
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)).then(a.0.cmp(&b.0)));
    scored.into_iter().map(|(i, s, _)| (i, s)).collect()
}

/// Match positions of `query` in `haystack`, for highlighting.
/// Returns the char indices in `haystack` that contributed to the
/// match. Empty query returns an empty vec.
pub fn match_positions(query: &str, haystack: &str) -> Vec<u32> {
    if query.is_empty() {
        return Vec::new();
    }
    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut buf = Vec::new();
    let utf = Utf32Str::new(haystack, &mut buf);
    let mut positions = Vec::new();
    pattern.indices(utf, &mut matcher, &mut positions);
    positions.sort_unstable();
    positions.dedup();
    positions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_keeps_all_items_in_order() {
        let items = vec!["alpha", "beta", "gamma"];
        let ranked = rank("", &items, |s| s);
        assert_eq!(
            ranked.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn matches_subsequence() {
        let items = vec!["/help", "/sessions", "/model"];
        let ranked = rank("sn", &items, |s| s);
        assert_eq!(items[ranked[0].0], "/sessions");
    }

    #[test]
    fn prefers_exact_prefix_over_longer_match() {
        let items = vec!["/help", "/help-debug"];
        let ranked = rank("help", &items, |s| s);
        assert_eq!(items[ranked[0].0], "/help");
    }

    #[test]
    fn filters_out_non_matches() {
        let items = vec!["alpha", "beta", "gamma"];
        let ranked = rank("xyz", &items, |s| s);
        assert!(ranked.is_empty());
    }

    #[test]
    fn case_insensitive() {
        let items = vec!["README.md", "src/"];
        let ranked = rank("readme", &items, |s| s);
        assert!(!ranked.is_empty());
        assert_eq!(items[ranked[0].0], "README.md");
    }

    #[test]
    fn key_extractor_is_applied() {
        struct Item {
            name: String,
        }
        let items = vec![
            Item {
                name: "alpha".into(),
            },
            Item {
                name: "beta".into(),
            },
        ];
        let ranked = rank("be", &items, |i| i.name.as_str());
        assert_eq!(ranked.len(), 1);
        assert_eq!(items[ranked[0].0].name, "beta");
    }

    #[test]
    fn match_positions_returns_indices() {
        let pos = match_positions("hl", "hello");
        // Should match the `h` (index 0) and one of the `l`s.
        assert!(!pos.is_empty());
        assert!(pos.contains(&0));
    }

    #[test]
    fn match_positions_empty_query_returns_empty() {
        let pos = match_positions("", "hello");
        assert!(pos.is_empty());
    }
}
