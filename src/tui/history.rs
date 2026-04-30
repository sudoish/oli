//! Persisted prompt history. Read on TUI startup, appended on
//! every submission. Stored as JSONL so multi-line prompts (with
//! embedded newlines) round-trip safely; one JSON-encoded string
//! per line.
//!
//! Capped at `MAX_ENTRIES` to keep the file small. When over the
//! cap, the file is rewritten with the trailing slice — old
//! entries fall off the front.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

const MAX_ENTRIES: usize = 500;
const HISTORY_FILE: &str = "tui-history.jsonl";

/// Where the history file lives. `~/.config/oli/tui-history.jsonl`
/// on Linux, the equivalent under `XDG_CONFIG_HOME` if set.
/// `None` if no $HOME / no $XDG_CONFIG_HOME — silently disables
/// persistence in that case.
pub fn history_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("oli").join(HISTORY_FILE))
}

pub fn load() -> Vec<String> {
    let Some(path) = history_path() else {
        return Vec::new();
    };
    load_from(&path)
}

pub fn load_from(path: &std::path::Path) -> Vec<String> {
    let body = match std::fs::read_to_string(path) {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };
    body.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            serde_json::from_str::<String>(line).ok()
        })
        .collect()
}

pub fn append(entry: &str) {
    if let Some(path) = history_path() {
        append_at(&path, entry);
    }
}

pub fn append_at(path: &std::path::Path, entry: &str) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let line = match serde_json::to_string(entry) {
        Ok(s) => format!("{}\n", s),
        Err(_) => return,
    };
    let mut f = match OpenOptions::new().create(true).append(true).open(path) {
        Ok(f) => f,
        Err(_) => return,
    };
    let _ = f.write_all(line.as_bytes());

    // Cheap rotation: if the file grew past MAX_ENTRIES * average,
    // rewrite with just the tail. We don't measure exactly, just
    // count lines on a best-effort basis. Failures here don't
    // surface — the user's session keeps working with whatever we
    // managed to write.
    if let Ok(content) = std::fs::read_to_string(path) {
        let lines: Vec<&str> = content.lines().collect();
        if lines.len() > MAX_ENTRIES {
            let tail: String = lines
                .iter()
                .rev()
                .take(MAX_ENTRIES)
                .rev()
                .copied()
                .collect::<Vec<_>>()
                .join("\n");
            let _ = std::fs::write(path, format!("{}\n", tail));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn round_trip_preserves_multi_line_entries() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hist.jsonl");
        append_at(&path, "single line");
        append_at(&path, "first\nsecond\nthird");
        append_at(&path, "with \"quotes\"");
        let loaded = load_from(&path);
        assert_eq!(
            loaded,
            vec![
                "single line".to_string(),
                "first\nsecond\nthird".to_string(),
                "with \"quotes\"".to_string(),
            ]
        );
    }

    #[test]
    fn missing_file_yields_empty_history() {
        let loaded = load_from(std::path::Path::new("/tmp/__nope_history__"));
        assert!(loaded.is_empty());
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hist.jsonl");
        std::fs::write(&path, "\"good\"\nNOT JSON\n\"also good\"\n").unwrap();
        let loaded = load_from(&path);
        assert_eq!(loaded, vec!["good".to_string(), "also good".to_string()]);
    }

    #[test]
    fn append_creates_parent_dir_if_missing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("hist.jsonl");
        append_at(&path, "hi");
        assert!(path.exists());
        let loaded = load_from(&path);
        assert_eq!(loaded, vec!["hi".to_string()]);
    }

    #[test]
    fn rotation_caps_file_at_max_entries() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hist.jsonl");
        // Pre-fill past the cap, then append one more — file
        // should be rewritten with just the tail.
        for i in 0..(MAX_ENTRIES + 50) {
            append_at(&path, &format!("entry {}", i));
        }
        let loaded = load_from(&path);
        assert_eq!(loaded.len(), MAX_ENTRIES);
        // Newest entry survived.
        assert!(
            loaded
                .last()
                .map(|s| s.contains(&format!("{}", MAX_ENTRIES + 49)))
                .unwrap_or(false)
        );
    }
}
