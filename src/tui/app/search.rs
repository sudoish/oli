//! In-transcript search state. Substring (case-insensitive) match
//! across the rendered transcript; `Ctrl+F` opens the bar, typing
//! refines the query, `Enter`/`n`/`N` cycle through matches, `Esc`
//! closes and clears highlights.
//!
//! Match positions are computed at render time against the
//! laid-out transcript lines — the state itself only carries the
//! query and the index of the currently-focused match, so the
//! state is cheap to keep in sync with a freshly-streaming
//! transcript.

use ratatui::text::Line;

/// One-line search bar overlay. Lives in `App.overlay` like the
/// other modals, but visually it's a thin top-of-transcript bar
/// rather than a centered modal.
#[derive(Debug, Clone, Default)]
pub struct SearchState {
    pub query: String,
    /// Index into the most-recently-computed match list. The
    /// match list itself is recomputed every render from the
    /// transcript lines + this query, so it doesn't live on the
    /// state.
    pub current: usize,
}

/// Find every line index (into `lines`) that contains `query` as a
/// case-insensitive substring. Returns an empty vec for an empty
/// query — search highlights nothing until the user types
/// something.
pub fn match_line_indices(lines: &[Line<'_>], query: &str) -> Vec<usize> {
    if query.is_empty() {
        return Vec::new();
    }
    let needle = query.to_lowercase();
    lines
        .iter()
        .enumerate()
        .filter_map(|(i, line)| {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            if text.to_lowercase().contains(&needle) {
                Some(i)
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::text::Span;

    fn ln(s: &str) -> Line<'static> {
        Line::from(Span::raw(s.to_string()))
    }

    #[test]
    fn empty_query_returns_no_matches() {
        let lines = vec![ln("hello world"), ln("goodbye")];
        assert!(match_line_indices(&lines, "").is_empty());
    }

    #[test]
    fn substring_matches_are_returned_in_line_order() {
        let lines = vec![ln("a panic happened"), ln("ok"), ln("another panic")];
        assert_eq!(match_line_indices(&lines, "panic"), vec![0, 2]);
    }

    #[test]
    fn match_is_case_insensitive() {
        let lines = vec![ln("PANIC"), ln("Panic"), ln("panic")];
        assert_eq!(match_line_indices(&lines, "panic"), vec![0, 1, 2]);
    }

    #[test]
    fn no_match_returns_empty() {
        let lines = vec![ln("hello"), ln("world")];
        assert!(match_line_indices(&lines, "xyz").is_empty());
    }

    #[test]
    fn search_spans_match_across_styled_spans() {
        // A line built from multiple spans should still match a
        // needle that spans the boundary, because we concatenate
        // the span text before searching.
        let line = Line::from(vec![
            Span::raw("hel".to_string()),
            Span::raw("lo".to_string()),
        ]);
        assert_eq!(match_line_indices(&[line], "hello"), vec![0]);
    }
}
