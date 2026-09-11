use super::{SlashCommand, SlashOutcome};
use crate::agent::Agent;
use async_trait::async_trait;

/// `/mcp` — introspection over the Model Context Protocol clients
/// connected at startup. Subcommands:
///   - (no args)         list servers, health, tool count
///   - `tools <server>`  enumerate the tools that server exposes
///   - `logs <server>`   show captured stderr (stdio servers only)
///   - `restart <server>` re-run connect+initialize for a server
pub struct Mcp {
    handles: std::sync::Arc<Vec<crate::mcp::McpHandle>>,
}

impl Mcp {
    pub fn new(handles: std::sync::Arc<Vec<crate::mcp::McpHandle>>) -> Self {
        Self { handles }
    }
}

#[async_trait]
impl SlashCommand for Mcp {
    fn name(&self) -> &str {
        "mcp"
    }
    fn description(&self) -> &str {
        "list MCP servers; `tools <server>` or `logs <server>` for detail"
    }
    async fn run(&self, args: &str, _agent: &mut Agent) -> SlashOutcome {
        let args = args.trim();
        if self.handles.is_empty() {
            return SlashOutcome::Continue(Some(
                "(no MCP servers configured — add a [mcp.servers.*] table to your config)".into(),
            ));
        }

        // Subcommand split: first token is the verb, rest is the
        // server name. `<empty>` falls through to the listing path.
        let (sub, rest) = match args.find(char::is_whitespace) {
            Some(i) => (&args[..i], args[i..].trim()),
            None => (args, ""),
        };

        match sub {
            "" => {
                let mut out = format!("MCP servers ({}):\n", self.handles.len());
                for h in self.handles.iter() {
                    let s = h.server.lock().await;
                    let (status, detail) = match &s.health {
                        crate::mcp::HealthState::Healthy => ("healthy", String::new()),
                        crate::mcp::HealthState::Down(r) => ("down", format!(" ({})", r)),
                    };
                    out.push_str(&format!(
                        "  {:<14}  {:<8}  {} tool(s){}\n",
                        h.name,
                        status,
                        s.tools.len(),
                        detail
                    ));
                }
                SlashOutcome::Continue(Some(out.trim_end().to_string()))
            }
            "tools" => {
                let target = rest;
                if target.is_empty() {
                    return SlashOutcome::Continue(Some("usage: /mcp tools <server>".into()));
                }
                let h = match self.handles.iter().find(|h| h.name == target) {
                    Some(h) => h,
                    None => {
                        return SlashOutcome::Continue(Some(format!(
                            "unknown MCP server: {}",
                            target
                        )));
                    }
                };
                let s = h.server.lock().await;
                if s.tools.is_empty() {
                    return SlashOutcome::Continue(Some(format!(
                        "(server `{}` exposes no tools)",
                        target
                    )));
                }
                let mut out = format!("Tools from `{}` ({}):\n", target, s.tools.len());
                let pad = s.tools.iter().map(|t| t.name.len()).max().unwrap_or(0);
                for t in &s.tools {
                    out.push_str(&format!(
                        "  {:<width$}  {}\n",
                        t.name,
                        t.description,
                        width = pad
                    ));
                }
                SlashOutcome::Continue(Some(out.trim_end().to_string()))
            }
            "logs" => {
                let target = rest;
                if target.is_empty() {
                    return SlashOutcome::Continue(Some("usage: /mcp logs <server>".into()));
                }
                let h = match self.handles.iter().find(|h| h.name == target) {
                    Some(h) => h,
                    None => {
                        return SlashOutcome::Continue(Some(format!(
                            "unknown MCP server: {}",
                            target
                        )));
                    }
                };
                let s = h.server.lock().await;
                let logs = s.stderr_snapshot().await;
                let body = if logs.is_empty() {
                    "(no stderr captured)".to_string()
                } else {
                    logs
                };
                SlashOutcome::Continue(Some(format!("--- stderr from `{}` ---\n{}", target, body)))
            }
            "restart" => {
                let target = rest;
                if target.is_empty() {
                    return SlashOutcome::Continue(Some("usage: /mcp restart <server>".into()));
                }
                let h = match self.handles.iter().find(|h| h.name == target) {
                    Some(h) => h,
                    None => {
                        return SlashOutcome::Continue(Some(format!(
                            "unknown MCP server: {}",
                            target
                        )));
                    }
                };
                let mut s = h.server.lock().await;
                match s.restart().await {
                    Ok(()) => SlashOutcome::Continue(Some(format!(
                        "restarted `{}` — {} tool(s) available",
                        target,
                        s.tools.len()
                    ))),
                    Err(e) => SlashOutcome::Continue(Some(format!(
                        "restart of `{}` failed: {}",
                        target, e
                    ))),
                }
            }
            other => SlashOutcome::Continue(Some(format!(
                "unknown subcommand: {} (try `tools <server>`, `logs <server>`, \
                 or `restart <server>`)",
                other
            ))),
        }
    }
}
