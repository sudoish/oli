//! Model Context Protocol client. External MCP servers (Linear, Notion,
//! Sentry, GitHub, Playwright, ...) get dialed up at startup, their
//! tool list enumerated, and exposed as first-class entries in the
//! shared `tools::Registry`. To the agent loop and the policy gate, an
//! MCP-backed tool is indistinguishable from `Read` or a subprocess
//! tool.
//!
//! Two transports, both behind the same `McpTransport` trait: stdio
//! (the default) and streamable-http.

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::config::{McpServerConfig, McpTransportKind};
    use crate::mcp::server::ToolMeta;
    use serde_json::json;

    /// Build a McpHandle wrapping a healthy McpServer with a manual
    /// tool list. Used for the no-real-process integration tests
    /// below; we don't need a live transport because the delta
    /// computation only reads `server.tools` + the (manually flipped)
    /// `tools_changed` flag.
    fn handle_with_tools(
        name: &str,
        tools: Vec<&'static str>,
    ) -> (McpHandle, std::sync::Arc<tokio::sync::Mutex<McpServer>>) {
        let cfg = McpServerConfig {
            kind: McpTransportKind::Stdio,
            command: Some("/bin/true".into()),
            args: vec![],
            env: Default::default(),
            url: None,
            headers: Default::default(),
            init_timeout_ms: 1000,
            call_timeout_ms: 1000,
            tools: Default::default(),
            enabled: true,
        };
        let server = McpServer {
            name: name.to_string(),
            cfg,
            transport: None,
            capabilities: Default::default(),
            tools: tools
                .into_iter()
                .map(|t| ToolMeta {
                    name: t.to_string(),
                    description: String::new(),
                    input_schema: json!({"type":"object"}),
                })
                .collect(),
            health: HealthState::Healthy,
            stderr_source: None,
        };
        let arc = std::sync::Arc::new(tokio::sync::Mutex::new(server));
        let handle = McpHandle {
            name: name.to_string(),
            server: arc.clone(),
        };
        (handle, arc)
    }

    /// `refresh_changed_tools` skips servers whose flag is clear and
    /// produces no deltas. The agent loop's per-turn cost on a quiet
    /// session is just the cheap atomic load.
    #[tokio::test]
    async fn refresh_returns_no_deltas_when_flag_is_clear() {
        // No transport means take_tools_changed is always false.
        let (h, _arc) = handle_with_tools("s", vec!["a", "b"]);
        let deltas = refresh_changed_tools(&[h]).await;
        // We skip clear servers entirely; deltas vec is empty.
        assert!(deltas.is_empty());
    }
}

/// Per-server delta surfaced by `refresh_changed_tools` so the agent
/// can update the harness `Registry` in place.
#[derive(Default)]
pub struct ToolListDelta {
    /// Server id (matches `McpHandle::name`). Surfaced for
    /// logging / `/diagnostics`; the agent's apply loop only
    /// touches `removed` + `added`, so it's not read today.
    #[allow(dead_code)]
    pub server: String,
    /// Tool names (post-`<server>__` namespacing) that should be
    /// removed from the harness registry.
    pub removed: Vec<String>,
    /// Freshly-built `McpTool` adapters for tools the server now
    /// exposes that we didn't have before.
    pub added: Vec<Box<dyn crate::tools::Tool>>,
}

/// Drain `notifications/tools/list_changed` flags across all healthy
/// servers, refetch their tool lists, and return per-server deltas so
/// the caller (the agent loop) can swap registry entries atomically.
/// Servers that aren't dirty are skipped — the cost on a quiet turn
/// is one atomic load per server.
pub async fn refresh_changed_tools(handles: &[McpHandle]) -> Vec<ToolListDelta> {
    let mut out = Vec::new();
    for h in handles {
        let mut server = h.server.lock().await;
        if !matches!(server.health, HealthState::Healthy) {
            continue;
        }
        if !server.take_tools_changed() {
            continue;
        }
        let prior_names: std::collections::HashSet<String> = server
            .tools
            .iter()
            .filter(|m| server.cfg.tools.allows(&m.name))
            .map(|m| format!("{}__{}", h.name, m.name))
            .collect();

        // Refetch — if the server is now flaking, log and move on
        // without disturbing the existing registry entries.
        let fresh = match server.refetch_tools().await {
            Ok(f) => f,
            Err(e) => {
                crate::log_warn!("mcp `{}` tools/list refresh failed: {}", h.name, e);
                continue;
            }
        };
        let fresh_names: std::collections::HashSet<String> = fresh
            .iter()
            .filter(|m| server.cfg.tools.allows(&m.name))
            .map(|m| format!("{}__{}", h.name, m.name))
            .collect();

        let removed: Vec<String> = prior_names.difference(&fresh_names).cloned().collect();
        let added: Vec<Box<dyn crate::tools::Tool>> = fresh
            .iter()
            .filter(|m| server.cfg.tools.allows(&m.name))
            .filter(|m| {
                let namespaced = format!("{}__{}", h.name, m.name);
                !prior_names.contains(&namespaced)
            })
            .map(|m| {
                Box::new(McpTool::new(h.server.clone(), h.name.clone(), m.clone()))
                    as Box<dyn crate::tools::Tool>
            })
            .collect();

        if !removed.is_empty() || !added.is_empty() {
            crate::log_info!(
                "[mcp] server `{}` tool list changed: -{} +{}",
                h.name,
                removed.len(),
                added.len()
            );
        }
        out.push(ToolListDelta {
            server: h.name.clone(),
            removed,
            added,
        });
    }
    out
}
