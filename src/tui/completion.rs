//! Completion logic for the TUI input box. Pure functions over a
//! line + cursor position; the App state machine in `app.rs`
//! drives the open/refresh/accept lifecycle. Keeping the
//! interesting decisions here makes them unit-testable without a
//! TextArea or a render loop.

use std::path::{Path, PathBuf};

use crate::tui::app::CompletionKind;
use crate::tui::fuzzy;

/// Result of inspecting a single line at a cursor position. `None`
/// means "no completion available right here." `Some` carries
/// what to look up + where to splice the replacement back in.
pub struct CompletionContext {
    pub kind: CompletionKind,
    pub query: String,
    /// Byte offset on the line where the trigger char (`/` or `@`)
    /// lives — i.e. the start of the substring we'll replace.
    pub replace_start_byte: usize,
}

/// Detect a completion context in `line` with the cursor at byte
/// offset `cursor_byte`. `is_first_line` lets the caller scope
/// slash completions to the very start of the input box (the
/// existing slash-command convention; a bare `/` mid-prompt is
/// just text).
pub fn detect(line: &str, cursor_byte: usize, is_first_line: bool) -> Option<CompletionContext> {
    let pre_cursor = &line[..cursor_byte.min(line.len())];

    // Slash completion: the line starts with `/` and the cursor
    // hasn't crossed a whitespace boundary into "args" territory.
    if is_first_line && pre_cursor.starts_with('/') {
        let after_slash = &pre_cursor[1..];
        if !after_slash.contains(char::is_whitespace) {
            return Some(CompletionContext {
                kind: CompletionKind::Slash,
                query: after_slash.to_string(),
                replace_start_byte: 0,
            });
        }
    }

    // Path completion: `@<query>` at a word boundary. We scan
    // backward from the cursor for the most recent `@` and check
    // that nothing whitespace-y came after it.
    if let Some(at_byte) = pre_cursor.rfind('@') {
        // Trigger has to be at the start of input or right after
        // a whitespace character — otherwise the user typed an
        // email address or similar, not a path completion.
        let trigger_ok = at_byte == 0
            || pre_cursor[..at_byte]
                .chars()
                .last()
                .map(|c| c.is_whitespace())
                .unwrap_or(true);
        let query = &pre_cursor[at_byte + 1..];
        let no_whitespace_after = !query.contains(char::is_whitespace);
        if trigger_ok && no_whitespace_after {
            let (base_dir, tail) = split_path_query(query);
            return Some(CompletionContext {
                kind: CompletionKind::Path { base_dir },
                query: tail,
                replace_start_byte: at_byte,
            });
        }
    }

    None
}

/// `@src/m` → (base_dir = `src`, tail = `m`). `@m` → (`.`, `m`).
/// `@src/` → (`src`, ``). Trailing slash means "list everything in
/// this dir."
fn split_path_query(query: &str) -> (PathBuf, String) {
    if let Some(slash) = query.rfind('/') {
        let dir = &query[..slash];
        let tail = &query[slash + 1..];
        let base = if dir.is_empty() {
            PathBuf::from("/")
        } else {
            PathBuf::from(dir)
        };
        (base, tail.to_string())
    } else {
        (PathBuf::from("."), query.to_string())
    }
}

/// Slash candidates ranked by fuzzy match against `query`. Empty
/// query returns all names sorted alphabetically (predictable popup
/// order). Non-empty query goes through `fuzzy::rank` so subsequence
/// matches like `ssn` → `/sessions` work.
pub fn slash_candidates(slash_names: &[String], query: &str) -> Vec<String> {
    if query.is_empty() {
        let mut out: Vec<String> = slash_names.to_vec();
        out.sort();
        return out;
    }
    fuzzy::rank(query, slash_names, |s| s.as_str())
        .into_iter()
        .map(|(i, _)| slash_names[i].clone())
        .collect()
}

/// Path candidates are entries in `base_dir` ranked by fuzzy
/// match against `query`. Hidden files (leading `.`) are surfaced
/// only when the query itself starts with `.`. Directories get a
/// trailing `/` so the user knows they're traversable. Empty query
/// returns entries sorted alphabetically.
pub fn path_candidates(base_dir: &Path, query: &str) -> Vec<String> {
    let read = match std::fs::read_dir(base_dir) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let want_hidden = query.starts_with('.');
    // Collect (raw_name, full_label) so we can fuzzy-rank by raw
    // name (what the user sees) and emit the full relative path.
    let mut entries: Vec<(String, String)> = Vec::new();
    for entry in read.flatten() {
        let raw = entry.file_name().to_string_lossy().to_string();
        if !want_hidden && raw.starts_with('.') {
            continue;
        }
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let label = if is_dir { format!("{}/", raw) } else { raw.clone() };
        let rel = if base_dir == Path::new(".") {
            label
        } else {
            format!("{}/{}", base_dir.display(), label)
        };
        entries.push((raw, rel));
    }
    if query.is_empty() {
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        let mut out: Vec<String> = entries.into_iter().map(|(_, r)| r).collect();
        out.truncate(20);
        return out;
    }
    let ranked = fuzzy::rank(query, &entries, |(raw, _)| raw.as_str());
    let mut out: Vec<String> = ranked
        .into_iter()
        .map(|(i, _)| entries[i].1.clone())
        .collect();
    out.truncate(20);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(line: &str, cursor: usize, first: bool) -> Option<CompletionContext> {
        detect(line, cursor, first)
    }

    #[test]
    fn slash_at_start_triggers_slash_completion() {
        let line = "/co";
        let c = ctx(line, line.len(), true).unwrap();
        assert!(matches!(c.kind, CompletionKind::Slash));
        assert_eq!(c.query, "co");
        assert_eq!(c.replace_start_byte, 0);
    }

    #[test]
    fn slash_with_args_does_not_trigger() {
        let line = "/model claude-haiku";
        let c = ctx(line, line.len(), true);
        assert!(c.is_none());
    }

    #[test]
    fn slash_on_non_first_line_does_not_trigger() {
        let line = "/co";
        assert!(ctx(line, line.len(), false).is_none());
    }

    #[test]
    fn at_path_at_start_triggers_path_completion() {
        let line = "@src/m";
        let c = ctx(line, line.len(), true).unwrap();
        match c.kind {
            CompletionKind::Path { base_dir } => assert_eq!(base_dir, PathBuf::from("src")),
            _ => panic!(),
        }
        assert_eq!(c.query, "m");
        assert_eq!(c.replace_start_byte, 0);
    }

    #[test]
    fn at_path_after_word_boundary_triggers() {
        let line = "look at @src/m";
        let c = ctx(line, line.len(), true).unwrap();
        assert!(matches!(c.kind, CompletionKind::Path { .. }));
        assert_eq!(c.query, "m");
        assert_eq!(c.replace_start_byte, "look at ".len());
    }

    #[test]
    fn at_in_email_position_does_not_trigger() {
        let line = "user@example.com";
        // `@` is preceded by a non-whitespace char, so we don't
        // try to complete it as a path.
        assert!(ctx(line, line.len(), true).is_none());
    }

    #[test]
    fn at_with_whitespace_in_query_does_not_trigger() {
        let line = "@src/foo bar";
        // The user has clearly typed past the path; we shouldn't
        // hijack their input.
        assert!(ctx(line, line.len(), true).is_none());
    }

    #[test]
    fn slash_candidates_filter_by_prefix_case_insensitive() {
        let slashes: Vec<String> = ["clear", "cost", "compact", "tools", "model"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut got = slash_candidates(&slashes, "co");
        got.sort();
        assert_eq!(got, vec!["compact".to_string(), "cost".to_string()]);
    }

    #[test]
    fn slash_candidates_match_subsequence() {
        let slashes: Vec<String> = ["help", "sessions", "model", "compact"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let got = slash_candidates(&slashes, "ssn");
        // `ssn` is a subsequence of `sessions`.
        assert_eq!(got.first().map(String::as_str), Some("sessions"));
    }

    #[test]
    fn slash_candidates_prefer_exact_prefix() {
        let slashes: Vec<String> = ["help", "help-debug"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let got = slash_candidates(&slashes, "help");
        assert_eq!(got.first().map(String::as_str), Some("help"));
    }

    #[test]
    fn slash_candidates_empty_query_returns_all_sorted() {
        let slashes: Vec<String> = ["help", "clear", "cost"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let got = slash_candidates(&slashes, "");
        assert_eq!(
            got,
            vec!["clear".to_string(), "cost".to_string(), "help".to_string()]
        );
    }

    #[test]
    fn path_candidates_fuzzy_ranks_prefix_match_first() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("alpha.rs"), "").unwrap();
        std::fs::write(dir.path().join("beta.rs"), "").unwrap();
        std::fs::create_dir(dir.path().join("nested")).unwrap();
        let got = path_candidates(dir.path(), "a");
        // Fuzzy: `alpha.rs` matches strongly (prefix), `beta.rs` matches
        // weakly (contains `a`), `nested` doesn't match.
        assert!(
            got.first().map(|s| s.ends_with("alpha.rs")).unwrap_or(false),
            "expected alpha.rs first, got {:?}",
            got
        );
        assert!(!got.iter().any(|s| s.ends_with("nested/")));
    }

    #[test]
    fn path_candidates_match_subsequence() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.rs"), "").unwrap();
        std::fs::write(dir.path().join("server.rs"), "").unwrap();
        let got = path_candidates(dir.path(), "mn");
        // `mn` is a subsequence of `main.rs`.
        assert!(
            got.first().map(|s| s.ends_with("main.rs")).unwrap_or(false),
            "expected main.rs first, got {:?}",
            got
        );
    }

    #[test]
    fn path_candidates_marks_directories_with_trailing_slash() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("README.md"), "").unwrap();
        let got = path_candidates(dir.path(), "");
        assert!(
            got.iter().any(|s| s.ends_with("src/")),
            "expected src/ in {:?}",
            got
        );
        assert!(got.iter().any(|s| s.ends_with("README.md")));
    }

    #[test]
    fn path_candidates_skip_hidden_unless_query_starts_with_dot() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".hidden"), "").unwrap();
        std::fs::write(dir.path().join("visible"), "").unwrap();
        let plain = path_candidates(dir.path(), "");
        assert!(!plain.iter().any(|s| s.ends_with(".hidden")));
        let dot = path_candidates(dir.path(), ".");
        assert!(dot.iter().any(|s| s.ends_with(".hidden")));
    }
}
