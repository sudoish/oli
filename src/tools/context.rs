use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Per-session state shared across tool calls within a single agent run.
/// Cheap to clone — the inner state is `Arc<Mutex<...>>`.
#[derive(Default, Clone)]
pub struct ToolContext {
    inner: Arc<Mutex<SessionState>>,
}

#[derive(Default, Debug)]
pub struct SessionState {
    /// Canonicalized paths of files that have been successfully read this
    /// session. Used by `Edit` to enforce a read-first invariant.
    pub read_files: HashSet<PathBuf>,
}

impl ToolContext {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark a file as read. Canonicalizes the path so callers using relative
    /// paths and absolute paths converge on the same key.
    pub async fn mark_read(&self, path: impl AsRef<Path>) {
        if let Ok(canon) = tokio::fs::canonicalize(path.as_ref()).await {
            self.inner.lock().await.read_files.insert(canon);
        }
    }

    pub async fn was_read(&self, path: impl AsRef<Path>) -> bool {
        let key = match tokio::fs::canonicalize(path.as_ref()).await {
            Ok(p) => p,
            Err(_) => return false,
        };
        self.inner.lock().await.read_files.contains(&key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn mark_and_check_read_via_canonical_path() {
        let f = NamedTempFile::new().unwrap();
        let ctx = ToolContext::new();
        ctx.mark_read(f.path()).await;
        assert!(ctx.was_read(f.path()).await);
    }

    #[tokio::test]
    async fn missing_path_is_not_marked() {
        let ctx = ToolContext::new();
        ctx.mark_read("/tmp/__no_such_file_for_ctx_test__").await;
        assert!(!ctx.was_read("/tmp/__no_such_file_for_ctx_test__").await);
    }

    #[tokio::test]
    async fn relative_and_absolute_paths_converge_after_canonicalize() {
        let f = NamedTempFile::new().unwrap();
        let abs = f.path().to_path_buf();
        let ctx = ToolContext::new();
        ctx.mark_read(&abs).await;
        // Different syntactic spelling of the same file resolves to the same canonical path.
        let mut spelt = std::path::PathBuf::new();
        spelt.push(abs.parent().unwrap());
        spelt.push(".");
        spelt.push(abs.file_name().unwrap());
        assert!(ctx.was_read(&spelt).await);
    }
}
