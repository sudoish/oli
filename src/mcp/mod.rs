//! Model Context Protocol client. External MCP servers (Linear, Notion,
//! Sentry, GitHub, Playwright, ...) get dialed up at startup, their
//! tool list enumerated, and exposed as first-class entries in the
//! shared `tools::Registry`. To the agent loop and the policy gate, an
//! MCP-backed tool is indistinguishable from `Read` or a subprocess
//! tool.
//!
//! Two transports in v1: stdio (default) and streamable-http. Only stdio
//! lands in this commit — http is wired through the same `McpTransport`
//! trait but its impl is deferred (see specs/mcp.md phase 5b).

use std::sync::Arc;

use tokio::sync::Mutex;

use crate::error::Result;

pub mod config;
pub mod http;
pub mod server;
pub mod stdio;
pub mod tool;
pub mod transport;

pub use config::McpConfig;
pub use server::{HealthState, McpServer};
pub use tool::McpTool;

/// Runtime handle on a connected (or failed-to-connect) server. Shared
/// across the harness so `/mcp` slash commands can introspect health
/// and stderr logs without going through the registry.
pub struct McpHandle {
    pub name: String,
    pub server: Arc<Mutex<McpServer>>,
}

/// Connect every enabled server in `cfg` in parallel, returning a vector
/// of handles. Failed connects are kept (with `HealthState::Down`) so
/// `/mcp` can show *why* a server is missing.
pub async fn connect_all(cfg: &McpConfig) -> Vec<McpHandle> {
    let mut futures = Vec::new();
    for (name, server_cfg) in &cfg.servers {
        if !server_cfg.enabled {
            continue;
        }
        let name = name.clone();
        let server_cfg = server_cfg.clone();
        futures.push(tokio::spawn(async move {
            let server = McpServer::connect(&name, &server_cfg).await;
            McpHandle {
                name,
                server: Arc::new(Mutex::new(server)),
            }
        }));
    }
    let mut out = Vec::with_capacity(futures.len());
    for fut in futures {
        if let Ok(handle) = fut.await {
            out.push(handle);
        }
    }
    out
}

/// Build `McpTool` adapters for every healthy server's tools, filtered
/// through that server's `allow`/`deny` lists. The returned vector goes
/// straight into `Registry::register_box`.
pub async fn build_tools(handles: &[McpHandle]) -> Vec<Box<dyn crate::tools::Tool>> {
    let mut tools: Vec<Box<dyn crate::tools::Tool>> = Vec::new();
    for h in handles {
        let server = h.server.lock().await;
        if !matches!(server.health, HealthState::Healthy) {
            continue;
        }
        for meta in server.tools.iter() {
            if !server.cfg.tools.allows(&meta.name) {
                continue;
            }
            tools.push(Box::new(McpTool::new(
                h.server.clone(),
                h.name.clone(),
                meta.clone(),
            )));
        }
    }
    tools
}

/// Best-effort shutdown of every connected server. Called from the
/// binary's drop path; not on the hot loop.
#[allow(dead_code)]
pub async fn close_all(handles: Vec<McpHandle>) -> Result<()> {
    for h in handles {
        let mut server = h.server.lock().await;
        let _ = server.close().await;
    }
    Ok(())
}
