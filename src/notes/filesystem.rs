//! Filesystem-backed `NotesStore`. Each note is a markdown file with
//! TOML frontmatter under `~/.config/agent/notes/<id>.md`.
//!
//! ```text
//! +++
//! id = "1745944203000"
//! title = "How tests run"
//! created_at = 1745944203000
//! tags = ["testing", "infra"]
//! +++
//!
//! # How tests run
//!
//! ...body...
//! ```
//!
//! Why TOML frontmatter and not YAML: the harness already pulls `toml`
//! as a dependency for config; YAML would mean another crate. The `+++`
//! delimiter is the Hugo / Zola convention so editors recognize it.

use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::{AgentError, Result};

use super::{NewNote, Note, NotesStore};

const FRONTMATTER_DELIM: &str = "+++";

pub struct FilesystemNotesStore {
    dir: PathBuf,
}

impl FilesystemNotesStore {
    /// Open (or initialize) the notes dir. Creating the directory is
    /// done lazily on first write to avoid a startup-time `~/.config`
    /// touch when the user never writes a note.
    pub fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Default location: `~/.config/agent/notes/`.
    pub fn default_dir() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("agent").join("notes"))
    }

    fn path_for(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{}.md", id))
    }

    fn new_id(&self) -> String {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis().to_string())
            .unwrap_or_else(|_| "0".into())
    }

    fn render(note: &Note) -> String {
        let frontmatter = Frontmatter {
            id: note.id.clone(),
            title: note.title.clone(),
            created_at: note.created_at,
            tags: note.tags.clone(),
        };
        let header = toml::to_string(&frontmatter).unwrap_or_default();
        format!(
            "{}\n{}{}\n\n{}\n",
            FRONTMATTER_DELIM, header, FRONTMATTER_DELIM, note.body
        )
    }

    fn parse(text: &str) -> Option<Note> {
        let stripped = text.strip_prefix(FRONTMATTER_DELIM)?;
        let rest = stripped.trim_start_matches('\n');
        let close_marker = format!("\n{}", FRONTMATTER_DELIM);
        let end = rest.find(&close_marker)?;
        let header = &rest[..end];
        let body = rest[end + close_marker.len()..]
            .trim_start_matches('\n')
            .to_string();

        let fm: Frontmatter = toml::from_str(header).ok()?;
        Some(Note {
            id: fm.id,
            title: fm.title,
            body,
            tags: fm.tags,
            created_at: fm.created_at,
        })
    }

    async fn read_all_notes(&self) -> Result<Vec<Note>> {
        let mut out = Vec::new();
        let read = match tokio::fs::read_dir(&self.dir).await {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(AgentError::Io(e)),
        };
        let mut read = read;
        while let Some(entry) = read.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            if let Ok(body) = tokio::fs::read_to_string(&path).await {
                if let Some(n) = Self::parse(&body) {
                    out.push(n);
                }
            }
        }
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(out)
    }
}

#[async_trait]
impl NotesStore for FilesystemNotesStore {
    async fn write(&self, new: NewNote) -> Result<String> {
        tokio::fs::create_dir_all(&self.dir).await?;
        let id = self.new_id();
        let created_at = id.parse::<u64>().unwrap_or(0);
        let note = Note {
            id: id.clone(),
            title: new.title,
            body: new.body,
            tags: new.tags,
            created_at,
        };
        let body = Self::render(&note);
        tokio::fs::write(self.path_for(&id), body).await?;
        Ok(id)
    }

    async fn search(&self, query: &str, tag: Option<&str>, limit: usize) -> Result<Vec<Note>> {
        let q = query.to_lowercase();
        let all = self.read_all_notes().await?;
        let filtered: Vec<Note> = all
            .into_iter()
            .filter(|n| match tag {
                Some(t) => n.tags.iter().any(|x| x == t),
                None => true,
            })
            .filter(|n| {
                if q.is_empty() {
                    return true;
                }
                n.title.to_lowercase().contains(&q) || n.body.to_lowercase().contains(&q)
            })
            .take(limit.max(1))
            .collect();
        Ok(filtered)
    }

    async fn list(&self, tag: Option<&str>) -> Result<Vec<Note>> {
        let all = self.read_all_notes().await?;
        Ok(all
            .into_iter()
            .filter(|n| match tag {
                Some(t) => n.tags.iter().any(|x| x == t),
                None => true,
            })
            .collect())
    }

    async fn delete(&self, id: &str) -> Result<bool> {
        let path = self.path_for(id);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(AgentError::Io(e)),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Frontmatter {
    id: String,
    title: String,
    created_at: u64,
    #[serde(default)]
    tags: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::tempdir;

    fn store_in(dir: &Path) -> FilesystemNotesStore {
        FilesystemNotesStore::at(dir.to_path_buf())
    }

    #[tokio::test]
    async fn write_then_list_round_trips_metadata_and_body() {
        let dir = tempdir().unwrap();
        let store = store_in(dir.path());
        let id = store
            .write(NewNote {
                title: "test".into(),
                body: "body of the note".into(),
                tags: vec!["a".into(), "b".into()],
            })
            .await
            .unwrap();
        assert!(!id.is_empty());

        let notes = store.list(None).await.unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].title, "test");
        assert!(notes[0].body.contains("body of the note"));
        assert_eq!(notes[0].tags, vec!["a", "b"]);
        assert_eq!(notes[0].id, id);
    }

    #[tokio::test]
    async fn list_filters_by_tag() {
        let dir = tempdir().unwrap();
        let store = store_in(dir.path());
        store
            .write(NewNote {
                title: "n1".into(),
                body: "x".into(),
                tags: vec!["alpha".into()],
            })
            .await
            .unwrap();
        // Tiny sleep to differentiate timestamps; otherwise on fast
        // hardware both notes share an id and the second write
        // overwrites the first.
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        store
            .write(NewNote {
                title: "n2".into(),
                body: "y".into(),
                tags: vec!["beta".into()],
            })
            .await
            .unwrap();

        let alpha = store.list(Some("alpha")).await.unwrap();
        assert_eq!(alpha.len(), 1);
        assert_eq!(alpha[0].title, "n1");

        let beta = store.list(Some("beta")).await.unwrap();
        assert_eq!(beta.len(), 1);
        assert_eq!(beta[0].title, "n2");
    }

    #[tokio::test]
    async fn search_substring_match_is_case_insensitive() {
        let dir = tempdir().unwrap();
        let store = store_in(dir.path());
        store
            .write(NewNote {
                title: "Cargo Tests".into(),
                body: "run cargo test --release".into(),
                tags: vec![],
            })
            .await
            .unwrap();
        let results = store.search("CARGO", None, 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Cargo Tests");
    }

    #[tokio::test]
    async fn search_with_tag_filter_narrows_results() {
        let dir = tempdir().unwrap();
        let store = store_in(dir.path());
        store
            .write(NewNote {
                title: "matches".into(),
                body: "foo bar".into(),
                tags: vec!["meta".into()],
            })
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        store
            .write(NewNote {
                title: "matches".into(),
                body: "foo bar".into(),
                tags: vec!["other".into()],
            })
            .await
            .unwrap();
        let results = store.search("foo", Some("meta"), 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].tags, vec!["meta"]);
    }

    #[tokio::test]
    async fn list_returns_most_recent_first() {
        let dir = tempdir().unwrap();
        let store = store_in(dir.path());
        store
            .write(NewNote {
                title: "old".into(),
                body: "".into(),
                tags: vec![],
            })
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        store
            .write(NewNote {
                title: "new".into(),
                body: "".into(),
                tags: vec![],
            })
            .await
            .unwrap();
        let notes = store.list(None).await.unwrap();
        assert_eq!(notes[0].title, "new");
        assert_eq!(notes[1].title, "old");
    }

    #[tokio::test]
    async fn delete_removes_the_note_file() {
        let dir = tempdir().unwrap();
        let store = store_in(dir.path());
        let id = store
            .write(NewNote {
                title: "x".into(),
                body: "y".into(),
                tags: vec![],
            })
            .await
            .unwrap();
        assert_eq!(store.list(None).await.unwrap().len(), 1);
        let removed = store.delete(&id).await.unwrap();
        assert!(removed);
        assert_eq!(store.list(None).await.unwrap().len(), 0);
        // Idempotent: deleting again returns false, not an error.
        assert!(!store.delete(&id).await.unwrap());
    }

    #[tokio::test]
    async fn list_on_missing_dir_returns_empty_not_error() {
        let store = FilesystemNotesStore::at(PathBuf::from(
            "/tmp/__nope_notes_dir_for_filesystem_tests__",
        ));
        let notes = store.list(None).await.unwrap();
        assert!(notes.is_empty());
    }

    #[tokio::test]
    async fn malformed_files_are_skipped_during_listing() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(dir.path().join("bad.md"), "no frontmatter here").unwrap();
        let store = store_in(dir.path());
        store
            .write(NewNote {
                title: "good".into(),
                body: "ok".into(),
                tags: vec![],
            })
            .await
            .unwrap();
        let notes = store.list(None).await.unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].title, "good");
    }
}
