//! `McpTransport` — the JSON-RPC 2.0 wire used by every MCP server.
//! Two impls in v1: `StdioTransport` (newline-delimited JSON over a
//! child process's stdio) and a future `HttpTransport` (streamable-http,
//! deferred to phase 5b).
//!
//! Implementations own their own request-id allocation and response
//! demuxing — the `McpServer` runtime only ever calls `request` and
//! `notify` and shouldn't have to think about correlation.

use async_trait::async_trait;
use serde_json::Value;

use crate::error::Result;

#[async_trait]
pub trait McpTransport: Send + Sync {
    /// Send a JSON-RPC request and await its matched response. The
    /// returned `Value` is the `result` field of the response (or an
    /// error if the server returned a JSON-RPC error envelope).
    async fn request(&self, method: &str, params: Value) -> Result<Value>;

    /// Send a JSON-RPC notification (no `id`, no response expected).
    async fn notify(&self, method: &str, params: Value) -> Result<()>;

    /// Best-effort shutdown. Stdio impls send `notifications/cancelled`
    /// for in-flight calls then close stdin; HTTP impls drop the
    /// connection.
    async fn close(&self) -> Result<()>;

    /// Read-and-clear the `notifications/tools/list_changed` flag.
    /// Returns `true` once per notification arrival; subsequent calls
    /// without a fresh notification return `false`. Default `false`
    /// for transports that don't implement notification capture
    /// (currently HTTP).
    fn take_tools_changed(&self) -> bool {
        false
    }
}
