//! Cross-session "long-term" memory — `NotesStore` trait + filesystem
//! default. Exposes a notes layer that persists across runs, distinct
//! from the active-context `Memory` trait. Retrieval failures here are
//! recoverable (the model just misses a hint), so a graph- or
//! embedding-backed alternative is a much smaller risk than swapping
//! out active-context memory.
//!
//! See `specs/memory.md` for the design rationale (why NotesStore is a
//! separate trait from `Memory`).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::Result;

pub mod filesystem;

pub use filesystem::FilesystemNotesStore;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Note {
    pub id: String,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Milliseconds since the UNIX epoch.
    pub created_at: u64,
}

/// Parameters accepted by `write`. Separate from `Note` so callers can
/// omit fields the store generates (id, created_at).
#[derive(Clone, Debug)]
pub struct NewNote {
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
}

#[async_trait]
pub trait NotesStore: Send + Sync {
    /// Persist a new note. Returns the generated id.
    async fn write(&self, note: NewNote) -> Result<String>;

    /// Substring + tag-filter search across all notes. `tag` (when
    /// present) restricts results to notes carrying it. `query` is a
    /// case-insensitive substring match against title and body.
    /// Returns most-recent first, capped at `limit`.
    async fn search(&self, query: &str, tag: Option<&str>, limit: usize) -> Result<Vec<Note>>;

    /// List all notes, optionally filtered by tag. Most-recent first.
    async fn list(&self, tag: Option<&str>) -> Result<Vec<Note>>;

    /// Delete a note by id. Returns true if a note was removed.
    /// Trait method kept for the contract: future REPL surface
    /// (`/notes rm`) will call it. Tests exercise concrete impls.
    #[allow(dead_code)]
    async fn delete(&self, id: &str) -> Result<bool>;
}
