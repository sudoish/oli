//! Stdio transport for MCP servers. The MCP standard for stdio is
//! newline-delimited JSON-RPC 2.0: one JSON message per line on stdin
//! and stdout, no embedded newlines, no Content-Length framing.
//!
//! Spawn a child process; a single background reader task pulls lines
//! off its stdout, parses each as a JSON-RPC message, and routes
//! responses to oneshot waiters keyed by request id. Server-initiated
//! notifications are logged-and-dropped — v1 doesn't subscribe to
//! resource updates or implement sampling.
//!
//! Stderr is piped into a per-server buffer so `/mcp logs <server>` can
//! surface it without flooding the model's context.

use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, oneshot};

use crate::error::{AgentError, Result};
use crate::mcp::transport::McpTransport;

/// Cap on the size of the captured stderr ring. Older bytes are dropped
/// once we exceed this — a flaky server should not be able to OOM us via
/// debug spam.
const STDERR_BUFFER_BYTES: usize = 64 * 1024;

type Waiter = oneshot::Sender<std::result::Result<Value, RpcError>>;

#[derive(Debug, Clone)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "rpc error {}: {}", self.code, self.message)
    }
}

pub struct StdioTransport {
    /// Child stdin, behind a mutex so concurrent `request` callers can
    /// serialize their writes. Holding the lock for the duration of the
    /// write is fine — the lines are tiny.
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    /// Pending request id → response waiter. Background reader resolves
    /// these as responses arrive.
    pending: Arc<Mutex<HashMap<i64, Waiter>>>,
    next_id: AtomicI64,
    /// Captured stderr (ring-trimmed). Surfaced via `/mcp logs`.
    stderr: Arc<Mutex<Vec<u8>>>,
    /// Best-effort handle on the spawned child. Dropped on `close`.
    child: Arc<Mutex<Option<Child>>>,
}

impl StdioTransport {
    /// Spawn the configured command and wire up reader/stderr tasks.
    /// Returns once the child is alive and the background tasks are
    /// running; protocol-level handshake lives in `McpServer::connect`.
    pub async fn spawn(
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<Self> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_clear();
        // Inherit a minimal set of env vars then layer the explicit ones
        // on top. Servers typically need PATH (for npx, uvx) and HOME
        // (for cached package directories) — we forward both unless the
        // user has overridden them via the config's `env` table.
        for var in ["PATH", "HOME", "USER", "LANG", "TMPDIR", "TERM"] {
            if let Ok(v) = std::env::var(var) {
                cmd.env(var, v);
            }
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().map_err(|e| {
            AgentError::Provider(format!("mcp stdio spawn `{}` failed: {}", command, e))
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| AgentError::Provider("mcp: child has no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AgentError::Provider("mcp: child has no stdout".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| AgentError::Provider("mcp: child has no stderr".into()))?;

        let pending: Arc<Mutex<HashMap<i64, Waiter>>> = Arc::new(Mutex::new(HashMap::new()));
        let stderr_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));

        // Reader task: parse each line as JSON-RPC, route responses by
        // id. Notifications without an id are dropped (v1 ignores
        // sampling / resources/subscribe / progress).
        {
            let pending = pending.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let msg: Value = match serde_json::from_str(trimmed) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    let id = msg.get("id").and_then(|v| v.as_i64());
                    if let Some(id) = id {
                        let waiter = pending.lock().await.remove(&id);
                        if let Some(tx) = waiter {
                            let result = if let Some(err) = msg.get("error") {
                                let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
                                let message = err
                                    .get("message")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("(no message)")
                                    .to_string();
                                Err(RpcError { code, message })
                            } else {
                                Ok(msg.get("result").cloned().unwrap_or(Value::Null))
                            };
                            let _ = tx.send(result);
                        }
                    }
                    // Server-initiated requests/notifications: ignored.
                    // v1 doesn't surface roots/sampling.
                }
                // Reader exited (EOF). Drain any remaining waiters with
                // a transport-error so callers don't hang forever.
                let mut p = pending.lock().await;
                for (_, tx) in p.drain() {
                    let _ = tx.send(Err(RpcError {
                        code: -32000,
                        message: "transport closed".into(),
                    }));
                }
            });
        }

        // Stderr task: append to a ring-trimmed buffer so a chatty
        // server doesn't blow our memory.
        {
            let buf = stderr_buf.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stderr);
                let mut chunk = [0u8; 4096];
                use tokio::io::AsyncReadExt;
                loop {
                    match reader.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let mut b = buf.lock().await;
                            b.extend_from_slice(&chunk[..n]);
                            if b.len() > STDERR_BUFFER_BYTES {
                                let drop_n = b.len() - STDERR_BUFFER_BYTES;
                                b.drain(..drop_n);
                            }
                        }
                    }
                }
            });
        }

        Ok(Self {
            stdin: Arc::new(Mutex::new(Some(stdin))),
            pending,
            next_id: AtomicI64::new(1),
            stderr: stderr_buf,
            child: Arc::new(Mutex::new(Some(child))),
        })
    }

    /// Snapshot of captured stderr as a UTF-8 string (lossy — server
    /// stderr can in theory contain garbage; we don't want a `/mcp logs`
    /// invocation to fail because of it).
    pub async fn stderr_snapshot(&self) -> String {
        let buf = self.stderr.lock().await;
        String::from_utf8_lossy(&buf).into_owned()
    }
}

#[async_trait]
impl McpTransport for StdioTransport {
    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let envelope = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let mut line = serde_json::to_vec(&envelope)?;
        line.push(b'\n');
        {
            let mut guard = self.stdin.lock().await;
            let stdin = guard
                .as_mut()
                .ok_or_else(|| AgentError::Provider("mcp: stdin closed".into()))?;
            stdin
                .write_all(&line)
                .await
                .map_err(|e| AgentError::Provider(format!("mcp stdin write: {}", e)))?;
            stdin
                .flush()
                .await
                .map_err(|e| AgentError::Provider(format!("mcp stdin flush: {}", e)))?;
        }

        match rx.await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(AgentError::Provider(e.to_string())),
            Err(_) => {
                // Waiter dropped — reader task exited before the
                // response landed. Clean up any leftover entry.
                self.pending.lock().await.remove(&id);
                Err(AgentError::Provider(
                    "mcp transport closed before response".into(),
                ))
            }
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let envelope = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let mut line = serde_json::to_vec(&envelope)?;
        line.push(b'\n');
        let mut guard = self.stdin.lock().await;
        let stdin = guard
            .as_mut()
            .ok_or_else(|| AgentError::Provider("mcp: stdin closed".into()))?;
        stdin
            .write_all(&line)
            .await
            .map_err(|e| AgentError::Provider(format!("mcp stdin write: {}", e)))?;
        stdin
            .flush()
            .await
            .map_err(|e| AgentError::Provider(format!("mcp stdin flush: {}", e)))?;
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        // Drop stdin first so the child sees EOF on its read loop.
        {
            let mut guard = self.stdin.lock().await;
            *guard = None;
        }
        // Best-effort kill if it doesn't exit on its own. We don't wait
        // forever — caller's responsibility to enforce a grace period.
        if let Some(mut child) = self.child.lock().await.take() {
            let _ = child.start_kill();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drives the transport against a tiny shell-script "server" that
    /// echoes any line it receives back, with the same id, as a
    /// JSON-RPC response. Verifies: line framing, id correlation,
    /// response demux.
    #[tokio::test]
    async fn request_response_roundtrip_via_echo_server() {
        // jq is widely available and lets us write a one-line "server"
        // that responds with `{jsonrpc:"2.0", id: <input id>, result: {ok:true, method:<input method>}}`.
        // If jq isn't on PATH the test silently skips.
        if which("jq").is_none() {
            eprintln!("skipping: jq not on PATH");
            return;
        }
        let env = HashMap::new();
        let t = StdioTransport::spawn(
            "jq",
            &[
                "--unbuffered".into(),
                "-c".into(),
                r#"{jsonrpc:"2.0", id: .id, result: {ok: true, method: .method}}"#.into(),
            ],
            &env,
        )
        .await
        .expect("spawn jq");
        let v = t
            .request("ping", json!({"hello": "world"}))
            .await
            .expect("request");
        assert_eq!(v["ok"], true);
        assert_eq!(v["method"], "ping");
        let _ = t.close().await;
    }

    /// Allocate a few ids in parallel and verify each waiter gets the
    /// matching response. With pipelined requests the demux is the
    /// thing that has to be right; if it isn't, this test deadlocks.
    #[tokio::test]
    async fn pipelined_requests_route_back_to_correct_waiter() {
        if which("jq").is_none() {
            eprintln!("skipping: jq not on PATH");
            return;
        }
        let env = HashMap::new();
        let t = Arc::new(
            StdioTransport::spawn(
                "jq",
                &[
                    "--unbuffered".into(),
                    "-c".into(),
                    r#"{jsonrpc:"2.0", id: .id, result: {echoed: .params}}"#.into(),
                ],
                &env,
            )
            .await
            .expect("spawn jq"),
        );

        let mut tasks = Vec::new();
        for i in 0..5 {
            let t = t.clone();
            tasks.push(tokio::spawn(async move {
                let v = t.request("m", json!({"i": i})).await.expect("request");
                assert_eq!(v["echoed"]["i"], i);
            }));
        }
        for fut in tasks {
            fut.await.expect("task");
        }
        let _ = t.close().await;
    }

    #[tokio::test]
    async fn spawn_failure_returns_provider_error() {
        let env = HashMap::new();
        let err = match StdioTransport::spawn("/no/such/binary", &[], &env).await {
            Err(e) => e,
            Ok(_) => panic!("expected spawn failure for /no/such/binary"),
        };
        assert!(err.to_string().contains("/no/such/binary"));
    }

    fn which(bin: &str) -> Option<std::path::PathBuf> {
        let path = std::env::var_os("PATH")?;
        for p in std::env::split_paths(&path) {
            let candidate = p.join(bin);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    }
}
