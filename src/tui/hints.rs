//! Persisted onboarding hints. Each hint has a stable id; once
//! the user has seen and used the corresponding feature we
//! record the id here and stop showing the tip on future
//! renders.
//!
//! Stored at `~/.config/oli/tui-hints.json` as a JSON array of
//! string ids. Tiny file, sync IO is fine.

use std::collections::HashSet;
use std::path::PathBuf;

const FILE_NAME: &str = "tui-hints.json";

/// Hint ids the TUI knows about. Adding a new tip = define a
/// new constant + emit it in the relevant render path.
pub mod ids {
    /// Footnote on the approval modal: "Press [a] to allow this
    /// for the rest of the session." Faded after the user
    /// presses `a` once.
    pub const APPROVAL_ALLOW: &str = "approval-allow";
    /// Footnote on the input: "Shift+Enter for a newline."
    /// Faded after the first multi-line submit.
    pub const MULTI_LINE: &str = "multi-line";
}

pub fn hints_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("oli").join(FILE_NAME))
}

pub fn load() -> HashSet<String> {
    let Some(path) = hints_path() else {
        return HashSet::new();
    };
    load_from(&path)
}

pub fn load_from(path: &std::path::Path) -> HashSet<String> {
    let body = match std::fs::read_to_string(path) {
        Ok(b) => b,
        Err(_) => return HashSet::new(),
    };
    serde_json::from_str::<Vec<String>>(&body)
        .map(|v| v.into_iter().collect())
        .unwrap_or_default()
}

pub fn save(set: &HashSet<String>) {
    if let Some(path) = hints_path() {
        save_to(&path, set);
    }
}

pub fn save_to(path: &std::path::Path, set: &HashSet<String>) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut sorted: Vec<&String> = set.iter().collect();
    sorted.sort();
    let body = serde_json::to_string(&sorted).unwrap_or_default();
    let _ = std::fs::write(path, body);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn round_trip_persists_hint_ids() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hints.json");
        let mut set = HashSet::new();
        set.insert(ids::APPROVAL_ALLOW.to_string());
        set.insert(ids::MULTI_LINE.to_string());
        save_to(&path, &set);
        let loaded = load_from(&path);
        assert_eq!(loaded, set);
    }

    #[test]
    fn missing_file_yields_empty_set() {
        let loaded = load_from(std::path::Path::new("/tmp/__nope_hints__"));
        assert!(loaded.is_empty());
    }

    #[test]
    fn malformed_file_yields_empty_set_not_panic() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hints.json");
        std::fs::write(&path, "not json").unwrap();
        let loaded = load_from(&path);
        assert!(loaded.is_empty());
    }

    #[test]
    fn save_creates_parent_dir() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("hints.json");
        let mut set = HashSet::new();
        set.insert("foo".into());
        save_to(&path, &set);
        assert!(path.exists());
    }
}
