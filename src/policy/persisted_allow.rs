//! Cross-session approval allow-list — `~/.config/oli/policy-allow.json`.
//!
//! Pressing `[A]` (capital) on the approval modal calls
//! [`PersistedAllowList::insert`], which both adds the fingerprint
//! to the in-memory set and writes the updated set to disk. On
//! startup, the binary loads the file via [`PersistedAllowList::open`]
//! and hands the list to the approver, which short-circuits any
//! matching future request.
//!
//! The fingerprint is `tool::<canonical-json>` — the same shape
//! `TuiApprover` and `ReadlineApprover` use for their session-scoped
//! sets — so a user who types `cargo test` once and presses `A`
//! gets that exact command auto-approved on every subsequent
//! launch, but a different command (or different file path) still
//! prompts.
//!
//! Storage is JSON: `{"version": 1, "fingerprints": [...]}`. The
//! versioned envelope lets us evolve the schema without silently
//! mis-parsing older files.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

const FILE_NAME: &str = "policy-allow.json";

/// On-disk envelope. Version is bumped when we break compat.
#[derive(Debug, Default, Serialize, Deserialize)]
struct OnDisk {
    version: u32,
    fingerprints: Vec<String>,
}

pub struct PersistedAllowList {
    path: PathBuf,
    inner: Mutex<HashSet<String>>,
}

impl PersistedAllowList {
    /// Open the default-path allow-list. Returns an empty list
    /// if the file is missing or malformed (we never want a
    /// corrupt cache to deny tools the user has already
    /// approved).
    pub fn open() -> Self {
        match default_path() {
            Some(p) => Self::open_at(p),
            None => Self::empty(),
        }
    }

    /// Open a list backed by an explicit path. Tests use this so
    /// they don't touch the user's real config dir.
    pub fn open_at(path: PathBuf) -> Self {
        let inner = match std::fs::read_to_string(&path) {
            Ok(body) => match serde_json::from_str::<OnDisk>(&body) {
                Ok(d) if d.version == 1 => d.fingerprints.into_iter().collect(),
                _ => HashSet::new(),
            },
            Err(_) => HashSet::new(),
        };
        Self {
            path,
            inner: Mutex::new(inner),
        }
    }

    fn empty() -> Self {
        Self {
            path: PathBuf::from("/dev/null"),
            inner: Mutex::new(HashSet::new()),
        }
    }

    pub fn contains(&self, fingerprint: &str) -> bool {
        self.inner.lock().unwrap().contains(fingerprint)
    }

    /// Append a fingerprint and flush the file. Best-effort: a
    /// write failure is logged but the in-memory set still
    /// reflects the user's choice for the rest of this session.
    pub fn insert(&self, fingerprint: String) {
        let mut set = self.inner.lock().unwrap();
        if !set.insert(fingerprint) {
            return; // already present, no need to flush
        }
        let snapshot = set.clone();
        drop(set);
        if let Err(e) = flush(&self.path, &snapshot) {
            crate::log_warn!(
                "[policy] failed to persist allow-list to {}: {}",
                self.path.display(),
                e
            );
        }
    }

    /// Wipe both memory and disk. Useful for tests; not exposed
    /// via a slash command yet.
    #[allow(dead_code)]
    pub fn clear(&self) {
        self.inner.lock().unwrap().clear();
        let _ = flush(&self.path, &HashSet::new());
    }

    /// Snapshot for diagnostics / debugging. Tests verify the
    /// round-trip through this.
    #[cfg(test)]
    pub fn snapshot(&self) -> HashSet<String> {
        self.inner.lock().unwrap().clone()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn flush(path: &Path, set: &HashSet<String>) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut entries: Vec<String> = set.iter().cloned().collect();
    // Stable order so diffs of the file are useful.
    entries.sort();
    let body = OnDisk {
        version: 1,
        fingerprints: entries,
    };
    let json = serde_json::to_string_pretty(&body)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, json)
}

fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("oli").join(FILE_NAME))
}

/// Same fingerprint format the session-scoped approver sets
/// use: `tool::<canonical-json>`. Centralized here so callers
/// can't accidentally diverge.
pub fn fingerprint(tool: &str, args: &serde_json::Value) -> String {
    format!("{}::{}", tool, args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn missing_file_yields_empty_list() {
        let list = PersistedAllowList::open_at(PathBuf::from(
            "/tmp/__nope_oli_allow_does_not_exist__",
        ));
        assert!(list.snapshot().is_empty());
    }

    #[test]
    fn malformed_file_yields_empty_list_not_panic() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("allow.json");
        std::fs::write(&path, "not json").unwrap();
        let list = PersistedAllowList::open_at(path);
        assert!(list.snapshot().is_empty());
    }

    #[test]
    fn insert_round_trips_through_disk() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("allow.json");
        let list = PersistedAllowList::open_at(path.clone());
        list.insert(fingerprint("Bash", &json!({"command":"cargo test"})));
        // Re-open and verify the entry is still there.
        let list2 = PersistedAllowList::open_at(path);
        assert!(list2.contains(&fingerprint("Bash", &json!({"command":"cargo test"}))));
        assert!(!list2.contains(&fingerprint("Bash", &json!({"command":"rm -rf /"}))));
    }

    #[test]
    fn insert_creates_parent_dir() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("allow.json");
        let list = PersistedAllowList::open_at(path.clone());
        list.insert(fingerprint("Edit", &json!({"file_path":"x"})));
        assert!(path.exists(), "expected parent dir to be created");
    }

    #[test]
    fn duplicate_inserts_are_a_noop() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("allow.json");
        let list = PersistedAllowList::open_at(path);
        let fp = fingerprint("Edit", &json!({"file_path":"x"}));
        list.insert(fp.clone());
        list.insert(fp.clone());
        assert_eq!(list.snapshot().len(), 1);
    }

    #[test]
    fn clear_wipes_memory_and_disk() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("allow.json");
        let list = PersistedAllowList::open_at(path.clone());
        list.insert(fingerprint("Edit", &json!({"file_path":"x"})));
        list.clear();
        assert!(list.snapshot().is_empty());
        let list2 = PersistedAllowList::open_at(path);
        assert!(list2.snapshot().is_empty());
    }

    #[test]
    fn fingerprint_includes_tool_name_and_args_json() {
        let fp = fingerprint("Bash", &json!({"command":"ls"}));
        assert!(fp.starts_with("Bash::"));
        assert!(fp.contains("\"command\":\"ls\""));
    }

    #[test]
    fn unknown_version_is_ignored() {
        // A future schema version should be treated as "unknown
        // — start fresh" rather than panic-decoded.
        let dir = tempdir().unwrap();
        let path = dir.path().join("allow.json");
        std::fs::write(
            &path,
            r#"{"version":99,"fingerprints":["Edit::stale"]}"#,
        )
        .unwrap();
        let list = PersistedAllowList::open_at(path);
        assert!(list.snapshot().is_empty());
    }
}
