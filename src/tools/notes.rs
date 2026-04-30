//! `WriteNote` / `SearchNotes` / `ListNotes` — the model-facing surface
//! over a `NotesStore`. Distinct from the active-context `Memory`
//! trait: notes persist across sessions, the model uses them like a
//! second-brain.
//!
//! The tools are thin: parse args → call NotesStore method → render
//! the result. Each holds an `Arc<dyn NotesStore>` so the binary can
//! swap a different backing store (graph-backed, embedding-RAG) later
//! without touching this file.

use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

use crate::error::{Result, ToolError};
use crate::notes::{NewNote, NotesStore};
use crate::tools::{Tool, ToolContext, util};

const DEFAULT_SEARCH_LIMIT: usize = 10;

pub struct WriteNote {
    store: Arc<dyn NotesStore>,
}

impl WriteNote {
    pub fn new(store: Arc<dyn NotesStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for WriteNote {
    fn name(&self) -> &str {
        "WriteNote"
    }
    fn description(&self) -> &str {
        "Save a note to the agent's long-term memory. Use for cross-session \
         knowledge: project conventions, how-to-run-tests, user preferences, \
         decisions and their rationale."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "title": { "type": "string", "description": "Short title used for browsing/search." },
                "body": { "type": "string", "description": "Markdown content." },
                "tags": { "type": "array", "items": { "type": "string" }, "description": "Optional categorization tags." }
            },
            "required": ["title", "body"]
        })
    }
    async fn run(&self, args: Value, _ctx: &ToolContext) -> Result<String> {
        let title = args
            .get("title")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments {
                tool: "WriteNote".into(),
                detail: "missing `title`".into(),
            })?
            .to_string();
        let body = args
            .get("body")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments {
                tool: "WriteNote".into(),
                detail: "missing `body`".into(),
            })?
            .to_string();
        let tags = args
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let id = self.store.write(NewNote { title, body, tags }).await?;
        Ok(format!("note saved (id={})", id))
    }
}

pub struct SearchNotes {
    store: Arc<dyn NotesStore>,
}

impl SearchNotes {
    pub fn new(store: Arc<dyn NotesStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for SearchNotes {
    fn name(&self) -> &str {
        "SearchNotes"
    }
    fn description(&self) -> &str {
        "Find notes whose title or body contains a substring (case-insensitive). \
         Returns most-recent first."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Substring to look for." },
                "tag": { "type": "string", "description": "Optional tag filter." },
                "limit": { "type": "integer", "description": "Max results (default 10)." }
            },
            "required": ["query"]
        })
    }
    async fn run(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let query = args.get("query").and_then(|v| v.as_str()).ok_or_else(|| {
            ToolError::InvalidArguments {
                tool: "SearchNotes".into(),
                detail: "missing `query`".into(),
            }
        })?;
        let tag = args.get("tag").and_then(|v| v.as_str());
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_SEARCH_LIMIT);
        let notes = self.store.search(query, tag, limit).await?;
        Ok(util::truncate_with_cache(
            ctx,
            &render_listing(&notes),
            util::DEFAULT_MAX_OUTPUT_BYTES,
        ))
    }
}

pub struct ListNotes {
    store: Arc<dyn NotesStore>,
}

impl ListNotes {
    pub fn new(store: Arc<dyn NotesStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for ListNotes {
    fn name(&self) -> &str {
        "ListNotes"
    }
    fn description(&self) -> &str {
        "List all notes in the agent's long-term memory. Optionally filter by tag."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "tag": { "type": "string", "description": "Optional tag filter." }
            }
        })
    }
    async fn run(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let tag = args.get("tag").and_then(|v| v.as_str());
        let notes = self.store.list(tag).await?;
        Ok(util::truncate_with_cache(
            ctx,
            &render_listing(&notes),
            util::DEFAULT_MAX_OUTPUT_BYTES,
        ))
    }
}

fn render_listing(notes: &[crate::notes::Note]) -> String {
    if notes.is_empty() {
        return "(no notes)".into();
    }
    let mut out = format!("{} note(s):\n", notes.len());
    for n in notes {
        let tags = if n.tags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", n.tags.join(", "))
        };
        out.push_str(&format!("- {} ({}){}\n", n.title, n.id, tags));
        let preview: String = n.body.lines().take(3).collect::<Vec<_>>().join("\n");
        if !preview.is_empty() {
            for line in preview.lines() {
                out.push_str(&format!("    {}\n", line));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notes::FilesystemNotesStore;
    use tempfile::tempdir;

    fn store_in(path: std::path::PathBuf) -> Arc<dyn NotesStore> {
        Arc::new(FilesystemNotesStore::at(path))
    }

    #[tokio::test]
    async fn write_note_persists_and_returns_id() {
        let dir = tempdir().unwrap();
        let tool = WriteNote::new(store_in(dir.path().to_path_buf()));
        let ctx = ToolContext::new();
        let out = tool
            .run(json!({"title":"hi","body":"hello world"}), &ctx)
            .await
            .unwrap();
        assert!(out.contains("note saved"));
        // Confirm the file actually exists.
        let entries: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(entries.len(), 1);
    }

    #[tokio::test]
    async fn search_notes_renders_matching_results() {
        let dir = tempdir().unwrap();
        let store = store_in(dir.path().to_path_buf());
        store
            .write(NewNote {
                title: "Cargo workflow".into(),
                body: "use cargo test".into(),
                tags: vec!["dev".into()],
            })
            .await
            .unwrap();
        let tool = SearchNotes::new(store.clone());
        let ctx = ToolContext::new();
        let out = tool.run(json!({"query":"cargo"}), &ctx).await.unwrap();
        assert!(out.contains("Cargo workflow"));
        assert!(out.contains("[dev]"));
    }

    #[tokio::test]
    async fn search_notes_with_no_matches_says_so() {
        let dir = tempdir().unwrap();
        let tool = SearchNotes::new(store_in(dir.path().to_path_buf()));
        let ctx = ToolContext::new();
        let out = tool.run(json!({"query":"missing"}), &ctx).await.unwrap();
        assert!(out.contains("no notes"));
    }

    #[tokio::test]
    async fn list_notes_renders_all_with_tag_filter() {
        let dir = tempdir().unwrap();
        let store = store_in(dir.path().to_path_buf());
        store
            .write(NewNote {
                title: "a".into(),
                body: "".into(),
                tags: vec!["x".into()],
            })
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        store
            .write(NewNote {
                title: "b".into(),
                body: "".into(),
                tags: vec!["y".into()],
            })
            .await
            .unwrap();
        let tool = ListNotes::new(store);
        let ctx = ToolContext::new();
        let all = tool.run(json!({}), &ctx).await.unwrap();
        assert!(all.contains("2 note(s)"));
        let only_x = tool.run(json!({"tag":"x"}), &ctx).await.unwrap();
        assert!(only_x.contains("1 note(s)"));
        assert!(only_x.contains("- a"));
    }

    #[tokio::test]
    async fn write_note_missing_title_is_invalid_arguments() {
        let dir = tempdir().unwrap();
        let tool = WriteNote::new(store_in(dir.path().to_path_buf()));
        let ctx = ToolContext::new();
        let err = tool.run(json!({"body":"x"}), &ctx).await.unwrap_err();
        assert!(err.to_string().contains("missing `title`"));
    }
}
