//! `PersistedMemory` — durable JSONL transcript decorator.
//!
//! Wraps an inner `Memory` and mirrors every state-mutating call to a
//! line-delimited file under `~/.config/oli/sessions/<id>.jsonl`. On
//! open, an existing file is replayed into the inner memory so the
//! session resumes exactly where it left off (modulo compaction, which
//! is internal restructuring and does not get logged).
//!
//! Format is `{"op": ..., "msg"?: ..., "n"?: ...}` per line. The op
//! vocabulary mirrors the trait surface:
//!
//! - `pin` — `msg` field carries the pinned message
//! - `record` — `msg` field carries the recorded message
//! - `clear` — no payload
//! - `truncate` — `n` field carries the new logical length
//!
//! Compaction is *not* logged. The original records are still present
//! in the transcript; replay reapplies them to a fresh inner memory and
//! lets the new session re-derive its own summary if it ever crosses
//! the compaction threshold again.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::fs::{File, OpenOptions};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use crate::error::{AgentError, Result};
use crate::tools::context::ReadLogger;

use super::{CompactContext, Memory};

pub struct PersistedMemory {
    inner: Box<dyn Memory>,
    file: Arc<Mutex<File>>,
    /// Public read accessors below — kept on the struct as part of
    /// the contract so callers (REPL, future `/sessions` UX) can
    /// surface where the session lives. Currently the binary
    /// doesn't print them, hence the `#[allow(dead_code)]`.
    #[allow(dead_code)]
    path: PathBuf,
    #[allow(dead_code)]
    id: String,
    /// Canonicalized paths replayed from the session file's `read` ops.
    /// Drained by the binary at startup into the live `ToolContext` so
    /// `Edit`'s read-first invariant survives a resumed conversation.
    replayed_reads: Vec<PathBuf>,
}

impl PersistedMemory {
    /// Open (or create) the session file for `id` under the default
    /// sessions dir, replaying any prior content into `inner` before
    /// wrapping. Subsequent mutations append. When `allow_create` is false,
    /// an error is returned if the session file does not already exist.
    pub async fn open(id: &str, inner: Box<dyn Memory>, allow_create: bool) -> Result<Self> {
        let dir = sessions_dir().ok_or_else(|| {
            AgentError::Config(
                "could not resolve sessions dir (no $HOME or $XDG_CONFIG_HOME?)".into(),
            )
        })?;
        Self::open_at(&dir, id, inner, allow_create).await
    }

    /// Same as `open`, but accepts an explicit sessions directory.
    /// Tests use this to avoid touching the user's real config dir.
    pub async fn open_at(
        dir: &Path,
        id: &str,
        mut inner: Box<dyn Memory>,
        allow_create: bool,
    ) -> Result<Self> {
        tokio::fs::create_dir_all(dir).await?;
        let path = dir.join(format!("{}.jsonl", id));

        let mut replayed_reads = Vec::new();
        if path.exists() {
            replay_into(&path, inner.as_mut(), &mut replayed_reads).await?;
        } else if !allow_create {
            return Err(AgentError::Config(format!(
                "conversation {} does not exist (transcript missing)",
                id
            )));
        }

        let file = OpenOptions::new()
            .create(allow_create)
            .append(true)
            .open(&path)
            .await?;

        Ok(Self {
            inner,
            file: Arc::new(Mutex::new(file)),
            path,
            id: id.to_string(),
            replayed_reads,
        })
    }

    /// Session id this struct is bound to. Public so the REPL or
    /// future tooling can display it. Not currently called by the
    /// binary; tests may exercise it.
    #[allow(dead_code)]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// On-disk path the JSONL transcript appends to.
    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Drain the read paths replayed from the session file. Caller
    /// applies them to the live `ToolContext` via
    /// `insert_canonical_read`. A second call returns an empty vec.
    pub fn drain_replayed_reads(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.replayed_reads)
    }

    /// Hand out a `ReadLogger` that appends `read` ops to this session's
    /// file. Caller binds it onto the `ToolContext` so the `Read` tool's
    /// `mark_read` calls land in the transcript and round-trip across
    /// conversation resume.
    pub fn read_logger(&self) -> Arc<dyn ReadLogger> {
        Arc::new(PersistedReadLogger {
            file: self.file.clone(),
        })
    }

    async fn append(&self, op: Value) -> Result<()> {
        let mut line = op.to_string();
        line.push('\n');
        let mut f = self.file.lock().await;
        f.write_all(line.as_bytes()).await?;
        f.flush().await?;
        Ok(())
    }
}

/// Adapter that lets `ToolContext::mark_read` mirror reads into the
/// session JSONL. Holds an `Arc` to the same `File` mutex
/// `PersistedMemory` writes through, so ordering between a `record`
/// op and a `read` op reflects real call order.
struct PersistedReadLogger {
    file: Arc<Mutex<File>>,
}

#[async_trait]
impl ReadLogger for PersistedReadLogger {
    async fn log_read(&self, path: &Path) {
        let line = json!({
            "op": "read",
            "path": path.to_string_lossy().to_string(),
        })
        .to_string()
            + "\n";
        let mut f = self.file.lock().await;
        let _ = f.write_all(line.as_bytes()).await;
        let _ = f.flush().await;
    }
}

#[async_trait]
impl Memory for PersistedMemory {
    async fn record(&mut self, message: Value) -> Result<()> {
        self.append(json!({"op": "record", "msg": &message}))
            .await?;
        self.inner.record(message).await
    }

    async fn snapshot(&self) -> Vec<Value> {
        self.inner.snapshot().await
    }

    async fn pin(&mut self, message: Value) -> Result<()> {
        self.append(json!({"op": "pin", "msg": &message})).await?;
        self.inner.pin(message).await
    }

    async fn pinned(&self) -> Vec<Value> {
        self.inner.pinned().await
    }

    fn len(&self) -> usize {
        self.inner.len()
    }

    async fn truncate(&mut self, n: usize) -> Result<()> {
        self.append(json!({"op": "truncate", "n": n})).await?;
        self.inner.truncate(n).await
    }

    async fn clear(&mut self) -> Result<()> {
        self.append(json!({"op": "clear"})).await?;
        self.inner.clear().await
    }

    async fn maybe_compact(&mut self, ctx: CompactContext<'_>) -> Result<()> {
        // Compaction is internal restructuring: it doesn't change the
        // logical record sequence the transcript captures. Replay will
        // see the originals and let a future session decide whether to
        // compact again under fresh budget pressure.
        self.inner.maybe_compact(ctx).await
    }
}

async fn replay_into(path: &Path, mem: &mut dyn Memory, reads: &mut Vec<PathBuf>) -> Result<()> {
    let body = tokio::fs::read_to_string(path).await?;
    for line in body.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue, // skip malformed lines rather than abort the whole replay
        };
        let op = v.get("op").and_then(|x| x.as_str()).unwrap_or("");
        match op {
            "pin" => {
                if let Some(m) = v.get("msg").cloned() {
                    let _ = mem.pin(m).await;
                }
            }
            "record" => {
                if let Some(m) = v.get("msg").cloned() {
                    let _ = mem.record(m).await;
                }
            }
            "clear" => {
                let _ = mem.clear().await;
                // A clear in a prior run wipes session-local state, so
                // the post-clear read-set should not carry pre-clear
                // entries.
                reads.clear();
            }
            "truncate" => {
                let n = v.get("n").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                let _ = mem.truncate(n).await;
            }
            "read" => {
                if let Some(p) = v.get("path").and_then(|x| x.as_str()) {
                    reads.push(PathBuf::from(p));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Resolve the directory where session transcripts live.
pub fn sessions_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("oli").join("sessions"))
}

/// Mint a fresh, lexically sortable session id. The timestamp keeps ids
/// readable while the process id and random suffix prevent concurrent
/// headless processes from sharing one transcript.
pub fn new_session_id() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!(
        "{:013}-{:08x}-{:016x}",
        millis,
        std::process::id(),
        rand::random::<u64>()
    )
}

/// List existing session ids (file stems), newest first by mtime.
/// Errors are swallowed because /sessions and `--continue` should
/// degrade gracefully if the dir is unreadable rather than crash.
pub fn list_sessions() -> Vec<SessionEntry> {
    match sessions_dir() {
        Some(d) => list_sessions_in(&d),
        None => Vec::new(),
    }
}

/// Path-aware variant of `list_sessions`. Test seam.
pub fn list_sessions_in(dir: &Path) -> Vec<SessionEntry> {
    let mut out = Vec::new();
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    for entry in read.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let id = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let mtime = entry.metadata().and_then(|m| m.modified()).ok();
        out.push(SessionEntry { id, path, mtime });
    }
    out.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    out
}

/// One session row in `list_sessions`. The picker UI currently
/// only reads `id` and `mtime`; `path` rides along so future
/// callers (e.g. an "open in editor" action) don't need a
/// second `sessions_dir().join(...)` round-trip.
pub struct SessionEntry {
    pub id: String,
    #[allow(dead_code)]
    pub path: PathBuf,
    pub mtime: Option<std::time::SystemTime>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::memory::LinearWithCompact;
    use serde_json::json;
    use tempfile::tempdir;

    fn fresh_inner() -> Box<dyn Memory> {
        Box::new(LinearWithCompact::new())
    }

    #[tokio::test]
    async fn first_open_creates_file_and_appends_record() {
        let dir = tempdir().unwrap();
        let mut m = PersistedMemory::open_at(dir.path(), "test1", fresh_inner(), true)
            .await
            .unwrap();
        m.record(json!({"role":"user","content":"hi"}))
            .await
            .unwrap();

        let body = tokio::fs::read_to_string(dir.path().join("test1.jsonl"))
            .await
            .unwrap();
        assert!(body.contains(r#""op":"record""#));
        assert!(body.contains(r#""content":"hi""#));
    }

    #[tokio::test]
    async fn replay_reconstructs_inner_state() {
        let dir = tempdir().unwrap();
        {
            let mut m = PersistedMemory::open_at(dir.path(), "rep", fresh_inner(), true)
                .await
                .unwrap();
            m.pin(json!({"role":"system","content":"sys"}))
                .await
                .unwrap();
            m.record(json!({"role":"user","content":"a"}))
                .await
                .unwrap();
            m.record(json!({"role":"assistant","content":"b"}))
                .await
                .unwrap();
        }
        let m = PersistedMemory::open_at(dir.path(), "rep", fresh_inner(), false)
            .await
            .unwrap();
        let snap = m.snapshot().await;
        assert_eq!(snap.len(), 3);
        assert_eq!(snap[0]["role"], "system");
        assert_eq!(snap[1]["content"], "a");
        assert_eq!(snap[2]["content"], "b");
        // pinned doesn't count toward len().
        assert_eq!(m.len(), 2);
    }

    #[tokio::test]
    async fn truncate_and_clear_round_trip_through_replay() {
        let dir = tempdir().unwrap();
        {
            let mut m = PersistedMemory::open_at(dir.path(), "tc", fresh_inner(), true)
                .await
                .unwrap();
            m.record(json!({"role":"user","content":"a"}))
                .await
                .unwrap();
            m.record(json!({"role":"user","content":"b"}))
                .await
                .unwrap();
            m.record(json!({"role":"user","content":"c"}))
                .await
                .unwrap();
            m.truncate(1).await.unwrap();
            m.record(json!({"role":"user","content":"d"}))
                .await
                .unwrap();
        }
        let m = PersistedMemory::open_at(dir.path(), "tc", fresh_inner(), false)
            .await
            .unwrap();
        // truncate(1) trimmed records 2 & 3; record("d") appended at
        // logical position 2.
        assert_eq!(m.len(), 2);
        let snap = m.snapshot().await;
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0]["content"], "a");
        assert_eq!(snap[1]["content"], "d");
    }

    #[tokio::test]
    async fn clear_drops_state_on_replay_too() {
        let dir = tempdir().unwrap();
        {
            let mut m = PersistedMemory::open_at(dir.path(), "cl", fresh_inner(), true)
                .await
                .unwrap();
            m.pin(json!({"role":"system","content":"sys"}))
                .await
                .unwrap();
            m.record(json!({"role":"user","content":"x"}))
                .await
                .unwrap();
            m.clear().await.unwrap();
            m.record(json!({"role":"user","content":"y"}))
                .await
                .unwrap();
        }
        let m = PersistedMemory::open_at(dir.path(), "cl", fresh_inner(), false)
            .await
            .unwrap();
        let snap = m.snapshot().await;
        // clear preserves pinned content.
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0]["role"], "system");
        assert_eq!(snap[1]["content"], "y");
    }

    #[tokio::test]
    async fn malformed_lines_are_skipped_not_fatal() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("bad.jsonl"),
            "{\"op\":\"record\",\"msg\":{\"role\":\"user\",\"content\":\"a\"}}\nNOT JSON\n",
        )
        .unwrap();
        let m = PersistedMemory::open_at(dir.path(), "bad", fresh_inner(), false)
            .await
            .unwrap();
        assert_eq!(m.len(), 1);
    }

    #[tokio::test]
    async fn list_sessions_in_returns_existing_ids() {
        let dir = tempdir().unwrap();
        {
            let mut m = PersistedMemory::open_at(dir.path(), "alpha", fresh_inner(), true)
                .await
                .unwrap();
            m.record(json!({"role":"user","content":"a"}))
                .await
                .unwrap();
        }
        {
            let mut m = PersistedMemory::open_at(dir.path(), "beta", fresh_inner(), true)
                .await
                .unwrap();
            m.record(json!({"role":"user","content":"b"}))
                .await
                .unwrap();
        }
        let entries = list_sessions_in(dir.path());
        let ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        assert!(ids.contains(&"alpha"));
        assert!(ids.contains(&"beta"));
    }

    #[test]
    fn new_session_id_yields_a_nonempty_sortable_string() {
        let a = new_session_id();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = new_session_id();
        assert!(!a.is_empty());
        assert!(b >= a);
    }

    #[test]
    fn new_session_ids_are_unique_within_the_same_millisecond_window() {
        let ids: std::collections::HashSet<_> = (0..1_000).map(|_| new_session_id()).collect();
        assert_eq!(ids.len(), 1_000);
    }

    #[tokio::test]
    async fn read_logger_writes_read_op_and_replay_drains_into_caller() {
        use crate::tools::context::ToolContext;
        use std::path::Path;
        use tempfile::NamedTempFile;

        let dir = tempdir().unwrap();
        let target_file = NamedTempFile::new().unwrap();
        let canon = tokio::fs::canonicalize(target_file.path()).await.unwrap();

        // First run: open, attach the logger to a context, mark a read,
        // and close.
        {
            let m = PersistedMemory::open_at(dir.path(), "rs", fresh_inner(), true)
                .await
                .unwrap();
            let ctx = ToolContext::new();
            ctx.set_read_logger(m.read_logger()).await;
            ctx.mark_read(target_file.path()).await;
        }

        // The transcript should now contain a `read` op for our file.
        let body = tokio::fs::read_to_string(dir.path().join("rs.jsonl"))
            .await
            .unwrap();
        assert!(
            body.contains(r#""op":"read""#),
            "transcript missing read op: {}",
            body
        );

        // Second run: replay drains the read into a fresh context.
        let mut m2 = PersistedMemory::open_at(dir.path(), "rs", fresh_inner(), false)
            .await
            .unwrap();
        let drained = m2.drain_replayed_reads();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0], canon);

        let ctx2 = ToolContext::new();
        for p in drained {
            ctx2.insert_canonical_read(p).await;
        }
        // Edit's read-first invariant should now be satisfied without
        // re-reading the file.
        assert!(ctx2.was_read(Path::new(target_file.path())).await);

        // A subsequent drain returns an empty vec — the field is
        // taken, not cloned.
        assert!(m2.drain_replayed_reads().is_empty());
    }

    #[tokio::test]
    async fn clear_in_transcript_drops_replayed_reads_too() {
        use crate::tools::context::ToolContext;
        use tempfile::NamedTempFile;

        let dir = tempdir().unwrap();
        let pre = NamedTempFile::new().unwrap();
        let post = NamedTempFile::new().unwrap();

        {
            let mut m = PersistedMemory::open_at(dir.path(), "cls", fresh_inner(), true)
                .await
                .unwrap();
            let ctx = ToolContext::new();
            ctx.set_read_logger(m.read_logger()).await;
            ctx.mark_read(pre.path()).await;
            m.clear().await.unwrap();
            ctx.mark_read(post.path()).await;
        }

        let mut m2 = PersistedMemory::open_at(dir.path(), "cls", fresh_inner(), false)
            .await
            .unwrap();
        let drained = m2.drain_replayed_reads();
        assert_eq!(drained.len(), 1);
        assert_eq!(
            drained[0],
            tokio::fs::canonicalize(post.path()).await.unwrap()
        );
    }
}
