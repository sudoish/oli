//! Model Context Protocol management commands.

use crate::config::Config;
use crate::error::{AgentError, Result};
use crate::mcp;

/// Add an HTTP MCP server, authorize it, and persist its config.
pub async fn add(name: &str, url: &str, paste: bool, read_only: bool) -> Result<()> {
    let credential = mcp::oauth::login(name, url, paste, &scopes(read_only)).await?;
    let path = crate::config::default_config_path().ok_or_else(|| {
        AgentError::Config("cannot locate config.toml; set XDG_CONFIG_HOME or HOME".into())
    })?;
    mcp::provision::apply_to_file(&path, name, url)?;
    println!(
        "Connected `{name}` with scopes {}.\nUpdated {}.\n\
         Its tools will be available the next time Oli starts.",
        credential.scope.as_deref().unwrap_or("(server default)"),
        path.display()
    );
    Ok(())
}

/// Reauthorize a configured OAuth MCP server.
pub async fn login(name: &str, paste: bool, read_only: bool) -> Result<()> {
    let config = Config::load_or_default()?;
    let server = config.mcp.servers.get(name).ok_or_else(|| {
        AgentError::Config(format!(
            "unknown MCP server `{name}`; add it with `oli mcp add {name} <url>`"
        ))
    })?;
    if server.auth != Some(crate::mcp::config::McpAuthKind::OAuth) {
        return Err(AgentError::Config(format!(
            "MCP server `{name}` does not use OAuth; set `auth = \"oauth\"` or \
             reconnect it with `oli mcp add {name} <url>`"
        )));
    }
    let url = server
        .url
        .as_deref()
        .ok_or_else(|| AgentError::Config(format!("MCP server `{name}` has no HTTP `url`")))?;
    let credential = mcp::oauth::login(name, url, paste, &scopes(read_only)).await?;
    println!(
        "Connected `{name}` with scopes {}.",
        credential.scope.as_deref().unwrap_or("(server default)")
    );
    Ok(())
}

/// Remove one MCP server's OAuth credential.
pub fn logout(name: &str) -> Result<()> {
    if mcp::oauth::logout(name)? {
        println!("Disconnected MCP server `{name}`.");
    } else {
        println!("MCP server `{name}` was not logged in.");
    }
    Ok(())
}

/// Print OAuth and connection status for one server or all OAuth servers.
pub async fn status(name: Option<String>) -> Result<()> {
    let config = Config::load_or_default()?;
    let names: Vec<String> = match name {
        Some(name) => vec![name],
        None => config
            .mcp
            .servers
            .iter()
            .filter(|(_, server)| server.auth.is_some())
            .map(|(name, _)| name.clone())
            .collect(),
    };
    if names.is_empty() {
        println!("No OAuth MCP servers are configured.");
        return Ok(());
    }
    for name in names {
        match mcp::oauth::status(&name).await? {
            Some(credential) => {
                let Some(server_config) = config.mcp.servers.get(&name) else {
                    println!(
                        "{name}: credentials stored for {}, but the server is not configured",
                        credential.resource
                    );
                    continue;
                };
                let server = mcp::McpServer::connect(&name, server_config).await;
                match server.health {
                    mcp::HealthState::Healthy => println!(
                        "{name}: connected to {} with {} tool(s) (scopes: {})",
                        credential.resource,
                        server.tools.len(),
                        credential.scope.as_deref().unwrap_or("(server default)")
                    ),
                    mcp::HealthState::Down(reason) => {
                        println!("{name}: credentials stored, but connection failed: {reason}")
                    }
                }
            }
            None => println!("{name}: not logged in; run `oli mcp login {name}`"),
        }
    }
    Ok(())
}

fn scopes(read_only: bool) -> Vec<String> {
    if read_only {
        vec!["read".into()]
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_requests_only_the_read_scope() {
        assert_eq!(scopes(true), ["read"]);
        assert!(scopes(false).is_empty());
    }
}
