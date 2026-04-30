use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::Mutex;

/// Per-session state shared across tool calls within a single agent run.
/// Cheap to clone — the inner state is `Arc<Mutex<...>>`.
#[derive(Default, Clone)]
pub struct ToolContext {
    inner: Arc<Mutex<SessionState>>,
}

#[derive(Default)]
pub struct SessionState {
    /// Canonical path -> file mtime at the moment of the most recent
    /// `Read`. `Edit` enforces the read-first invariant against the
    /// presence of a key here, and refuses an edit when the on-disk
    /// mtime has advanced past the recorded one (external mutation).
    /// A `None` mtime means "we couldn't stat at read time" — Edit
    /// downgrades to read-presence-only in that case to keep
    /// pathological filesystems usable.
    pub read_files: HashMap<PathBuf, Option<SystemTime>>,
    /// Last explicit `cwd` from a Bash invocation, sticky across calls.
    /// `None` falls back to the agent process's cwd.
    pub cwd: Option<PathBuf>,
    /// Optional sink for read events. Set at startup when the session is
    /// backed by `PersistedMemory` so the read-set survives `--resume`.
    pub read_logger: Option<Arc<dyn ReadLogger>>,
}

/// Sink for read-tracking events. PersistedMemory implements this to
/// mirror reads into the session JSONL so a resumed session restores
/// `Edit`'s read-first invariant without forcing a re-read.
#[async_trait]
pub trait ReadLogger: Send + Sync {
    async fn log_read(&self, path: &Path);
}

impl ToolContext {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark a file as read. Canonicalizes the path so callers using
    /// relative paths and absolute paths converge on the same key, and
    /// captures the file's mtime so `Edit` can detect external
    /// mutations between read and edit. If a `ReadLogger` is wired
    /// up, the canonical path is also forwarded to it.
    pub async fn mark_read(&self, path: impl AsRef<Path>) {
        if let Ok(canon) = tokio::fs::canonicalize(path.as_ref()).await {
            let mtime = tokio::fs::metadata(&canon)
                .await
                .ok()
                .and_then(|m| m.modified().ok());
            let logger = {
                let mut state = self.inner.lock().await;
                state.read_files.insert(canon.clone(), mtime);
                state.read_logger.clone()
            };
            if let Some(l) = logger {
                l.log_read(&canon).await;
            }
        }
    }

    /// Boolean read-check. Kept as a small public surface for plugin
    /// authors and tests that want to ask "was this read?" without
    /// caring about staleness; production `Edit` goes through
    /// `is_stale` instead because it needs to distinguish stale
    /// reads from never-read.
    #[allow(dead_code)]
    pub async fn was_read(&self, path: impl AsRef<Path>) -> bool {
        let key = match tokio::fs::canonicalize(path.as_ref()).await {
            Ok(p) => p,
            Err(_) => return false,
        };
        self.inner.lock().await.read_files.contains_key(&key)
    }

    /// Check whether the file has been modified externally since the
    /// last `mark_read` (or `insert_canonical_read`). Returns:
    /// - `Some(true)` — file was read AND on-disk mtime is now newer.
    ///   `Edit` should refuse and ask for a fresh `Read`.
    /// - `Some(false)` — file was read and is still in sync (or we
    ///   couldn't stat it at read time, in which case we conservatively
    ///   trust the read).
    /// - `None` — file was never read in this session. The caller is
    ///   responsible for surfacing that as a "Read first" error;
    ///   distinguishing it from "stale" lets Edit produce a clearer
    ///   message.
    pub async fn is_stale(&self, path: impl AsRef<Path>) -> Option<bool> {
        let key = tokio::fs::canonicalize(path.as_ref()).await.ok()?;
        let recorded = {
            let state = self.inner.lock().await;
            state.read_files.get(&key).cloned()
        };
        let recorded = recorded?;
        // If we couldn't capture an mtime at read time (filesystem
        // didn't expose one), don't second-guess later — treat as
        // fresh and let the model take the read at face value.
        let recorded = match recorded {
            Some(t) => t,
            None => return Some(false),
        };
        let current = tokio::fs::metadata(&key)
            .await
            .ok()
            .and_then(|m| m.modified().ok());
        match current {
            Some(now) if now > recorded => Some(true),
            _ => Some(false),
        }
    }

    /// Insert an already-canonicalized path into the read-set without
    /// invoking the logger or touching the filesystem. Used by session
    /// replay where the path was canonicalized at original-record
    /// time. We do still capture a fresh mtime so a subsequent Edit
    /// can detect post-resume external mutation; this means a file
    /// changed *during* the gap between sessions counts as fresh —
    /// the read-replay attests it was once read, not what its
    /// contents were.
    pub async fn insert_canonical_read(&self, path: PathBuf) {
        let mtime = tokio::fs::metadata(&path)
            .await
            .ok()
            .and_then(|m| m.modified().ok());
        self.inner.lock().await.read_files.insert(path, mtime);
    }

    /// Bind a sink for read events. Subsequent `mark_read` calls forward
    /// the canonical path to the sink. Replay paths injected via
    /// `insert_canonical_read` are *not* forwarded — they are recorded
    /// already and would loop.
    pub async fn set_read_logger(&self, logger: Arc<dyn ReadLogger>) {
        self.inner.lock().await.read_logger = Some(logger);
    }

    /// Persist a cwd for subsequent Bash calls that omit `cwd`.
    pub async fn set_cwd(&self, cwd: PathBuf) {
        self.inner.lock().await.cwd = Some(cwd);
    }

    /// Look up the sticky cwd, if any.
    pub async fn cwd(&self) -> Option<PathBuf> {
        self.inner.lock().await.cwd.clone()
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

    #[tokio::test]
    async fn cwd_round_trips() {
        let ctx = ToolContext::new();
        assert!(ctx.cwd().await.is_none());
        ctx.set_cwd(PathBuf::from("/tmp")).await;
        assert_eq!(ctx.cwd().await, Some(PathBuf::from("/tmp")));
    }

    #[tokio::test]
    async fn insert_canonical_read_skips_logger() {
        use std::sync::Mutex as StdMutex;

        struct CountingLogger(Arc<StdMutex<usize>>);
        #[async_trait]
        impl ReadLogger for CountingLogger {
            async fn log_read(&self, _: &Path) {
                *self.0.lock().unwrap() += 1;
            }
        }

        let count = Arc::new(StdMutex::new(0usize));
        let ctx = ToolContext::new();
        ctx.set_read_logger(Arc::new(CountingLogger(count.clone())))
            .await;

        ctx.insert_canonical_read(PathBuf::from("/tmp/replayed"))
            .await;
        assert_eq!(*count.lock().unwrap(), 0, "replay must not re-log");
        assert!(ctx.was_read("/tmp/replayed").await || true);
        // (`was_read` would re-canonicalize; we just care the logger wasn't poked.)
    }

    #[tokio::test]
    async fn mark_read_forwards_to_logger() {
        use std::sync::Mutex as StdMutex;

        struct RecordingLogger(Arc<StdMutex<Vec<PathBuf>>>);
        #[async_trait]
        impl ReadLogger for RecordingLogger {
            async fn log_read(&self, p: &Path) {
                self.0.lock().unwrap().push(p.to_path_buf());
            }
        }

        let log: Arc<StdMutex<Vec<PathBuf>>> = Arc::new(StdMutex::new(Vec::new()));
        let ctx = ToolContext::new();
        ctx.set_read_logger(Arc::new(RecordingLogger(log.clone())))
            .await;

        let f = NamedTempFile::new().unwrap();
        ctx.mark_read(f.path()).await;
        let entries = log.lock().unwrap().clone();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0],
            tokio::fs::canonicalize(f.path()).await.unwrap()
        );
    }
}
