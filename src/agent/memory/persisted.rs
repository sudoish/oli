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

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::fs::{File, OpenOptions};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use crate::error::{AgentError, Result};

use super::{CompactContext, Memory};

pub struct PersistedMemory {
    inner: Box<dyn Memory>,
    file: Mutex<File>,
    path: PathBuf,
    id: String,
}

impl PersistedMemory {
    /// Open (or create) the session file for `id` under the default
    /// sessions dir, replaying any prior content into `inner` before
    /// wrapping. Subsequent mutations append.
    pub async fn open(id: &str, inner: Box<dyn Memory>) -> Result<Self> {
        let dir = sessions_dir().ok_or_else(|| {
            AgentError::Config(
                "could not resolve sessions dir (no $HOME or $XDG_CONFIG_HOME?)".into(),
            )
        })?;
        Self::open_at(&dir, id, inner).await
    }

    /// Same as `open`, but accepts an explicit sessions directory.
    /// Tests use this to avoid touching the user's real config dir.
    pub async fn open_at(dir: &Path, id: &str, mut inner: Box<dyn Memory>) -> Result<Self> {
        tokio::fs::create_dir_all(dir).await?;
        let path = dir.join(format!("{}.jsonl", id));

        if path.exists() {
            replay_into(&path, inner.as_mut()).await?;
        }

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;

        Ok(Self {
            inner,
            file: Mutex::new(file),
            path,
            id: id.to_string(),
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    async fn append(&self, op: Value) {
        let mut line = op.to_string();
        line.push('\n');
        let mut f = self.file.lock().await;
        // Best-effort: a write failure is logged but should not crash
        // the session — the in-memory state is still authoritative for
        // the live conversation. Future hook for telemetry/error
        // reporting; for now we silently swallow to keep the agent loop
        // resilient against full disks / closed handles.
        let _ = f.write_all(line.as_bytes()).await;
        let _ = f.flush().await;
    }
}

#[async_trait]
impl Memory for PersistedMemory {
    async fn record(&mut self, message: Value) {
        self.append(json!({"op": "record", "msg": &message})).await;
        self.inner.record(message).await;
    }

    async fn snapshot(&self) -> Vec<Value> {
        self.inner.snapshot().await
    }

    async fn pin(&mut self, message: Value) {
        self.append(json!({"op": "pin", "msg": &message})).await;
        self.inner.pin(message).await;
    }

    async fn pinned(&self) -> Vec<Value> {
        self.inner.pinned().await
    }

    fn len(&self) -> usize {
        self.inner.len()
    }

    async fn truncate(&mut self, n: usize) {
        self.append(json!({"op": "truncate", "n": n})).await;
        self.inner.truncate(n).await;
    }

    async fn clear(&mut self) {
        self.append(json!({"op": "clear"})).await;
        self.inner.clear().await;
    }

    async fn maybe_compact(&mut self, ctx: CompactContext<'_>) -> Result<()> {
        // Compaction is internal restructuring: it doesn't change the
        // logical record sequence the transcript captures. Replay will
        // see the originals and let a future session decide whether to
        // compact again under fresh budget pressure.
        self.inner.maybe_compact(ctx).await
    }
}

async fn replay_into(path: &Path, mem: &mut dyn Memory) -> Result<()> {
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
                    mem.pin(m).await;
                }
            }
            "record" => {
                if let Some(m) = v.get("msg").cloned() {
                    mem.record(m).await;
                }
            }
            "clear" => mem.clear().await,
            "truncate" => {
                let n = v.get("n").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                mem.truncate(n).await;
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

/// Mint a fresh session id. UNIX-millis-based so it's sortable and
/// unique enough for the local-only single-user case. Tests can pass
/// their own id to avoid collisions.
pub fn new_session_id() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{:013}", millis)
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

/// One session row in `list_sessions`.
pub struct SessionEntry {
    pub id: String,
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
        let mut m = PersistedMemory::open_at(dir.path(), "test1", fresh_inner())
            .await
            .unwrap();
        m.record(json!({"role":"user","content":"hi"})).await;

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
            let mut m = PersistedMemory::open_at(dir.path(), "rep", fresh_inner())
                .await
                .unwrap();
            m.pin(json!({"role":"system","content":"sys"})).await;
            m.record(json!({"role":"user","content":"a"})).await;
            m.record(json!({"role":"assistant","content":"b"})).await;
        }
        let m = PersistedMemory::open_at(dir.path(), "rep", fresh_inner())
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
            let mut m = PersistedMemory::open_at(dir.path(), "tc", fresh_inner())
                .await
                .unwrap();
            m.record(json!({"role":"user","content":"a"})).await;
            m.record(json!({"role":"user","content":"b"})).await;
            m.record(json!({"role":"user","content":"c"})).await;
            m.truncate(1).await;
            m.record(json!({"role":"user","content":"d"})).await;
        }
        let m = PersistedMemory::open_at(dir.path(), "tc", fresh_inner())
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
            let mut m = PersistedMemory::open_at(dir.path(), "cl", fresh_inner())
                .await
                .unwrap();
            m.pin(json!({"role":"system","content":"sys"})).await;
            m.record(json!({"role":"user","content":"x"})).await;
            m.clear().await;
            m.record(json!({"role":"user","content":"y"})).await;
        }
        let m = PersistedMemory::open_at(dir.path(), "cl", fresh_inner())
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
        let m = PersistedMemory::open_at(dir.path(), "bad", fresh_inner())
            .await
            .unwrap();
        assert_eq!(m.len(), 1);
    }

    #[tokio::test]
    async fn list_sessions_in_returns_existing_ids() {
        let dir = tempdir().unwrap();
        {
            let mut m = PersistedMemory::open_at(dir.path(), "alpha", fresh_inner())
                .await
                .unwrap();
            m.record(json!({"role":"user","content":"a"})).await;
        }
        {
            let mut m = PersistedMemory::open_at(dir.path(), "beta", fresh_inner())
                .await
                .unwrap();
            m.record(json!({"role":"user","content":"b"})).await;
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
}
