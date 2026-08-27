//! Format-preserving configuration edits for `oli mcp add`.

use std::path::Path;

use toml_edit::{DocumentMut, Item, Table, value};

use crate::error::{AgentError, Result};

pub fn apply(config_toml: &str, name: &str, url: &str) -> Result<String> {
    if name.trim().is_empty() {
        return Err(AgentError::Config("MCP server name cannot be empty".into()));
    }
    let mut doc: DocumentMut = config_toml
        .parse()
        .map_err(|error| AgentError::Config(format!("config.toml is not valid TOML: {error}")))?;
    ensure_table(&mut doc, "mcp");
    if !doc["mcp"]
        .get("servers")
        .is_some_and(|item| item.is_table_like())
    {
        let mut servers = Table::new();
        servers.set_implicit(true);
        doc["mcp"]["servers"] = Item::Table(servers);
    }
    if !doc["mcp"]["servers"]
        .get(name)
        .is_some_and(|item| item.is_table_like())
    {
        doc["mcp"]["servers"][name] = Item::Table(Table::new());
    }
    let server = &mut doc["mcp"]["servers"][name];
    server["kind"] = value("streamable-http");
    server["url"] = value(url);
    server["auth"] = value("oauth");
    Ok(doc.to_string())
}

pub fn apply_to_file(path: &Path, name: &str, url: &str) -> Result<()> {
    let original = match std::fs::read_to_string(path) {
        Ok(body) => body,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(AgentError::Config(format!(
                "cannot read {}: {error}",
                path.display()
            )));
        }
    };
    let updated = apply(&original, name, url)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            AgentError::Config(format!("cannot create {}: {error}", parent.display()))
        })?;
    }
    if !original.is_empty() {
        let backup = path.with_extension("toml.bak");
        std::fs::copy(path, &backup).map_err(|error| {
            AgentError::Config(format!("cannot write {}: {error}", backup.display()))
        })?;
    }
    std::fs::write(path, updated)
        .map_err(|error| AgentError::Config(format!("cannot write {}: {error}", path.display())))
}

fn ensure_table(doc: &mut DocumentMut, name: &str) {
    if !doc.get(name).is_some_and(|item| item.is_table_like()) {
        let mut table = Table::new();
        table.set_implicit(true);
        doc[name] = Item::Table(table);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_writes_the_runtime_mcp_schema_and_preserves_other_config() {
        let input = r#"# keep this
default_provider = "ollama"

[providers.ollama]
kind = "openai-compat"
"#;
        let output = apply(input, "linear", "https://mcp.linear.app/mcp").unwrap();
        assert!(output.contains("# keep this"));
        assert!(output.contains("[mcp.servers.linear]"));
        assert!(output.contains(r#"kind = "streamable-http""#));
        assert!(output.contains(r#"url = "https://mcp.linear.app/mcp""#));
        assert!(output.contains(r#"auth = "oauth""#));
        crate::config::Config::from_str(&output).unwrap();
    }

    #[test]
    fn add_is_idempotent_and_keeps_server_tool_filters() {
        let input = r#"[mcp.servers.linear]
kind = "streamable-http"
url = "https://old.example/mcp"

[mcp.servers.linear.tools]
deny = ["delete_*"]
"#;
        let once = apply(input, "linear", "https://mcp.linear.app/mcp").unwrap();
        let twice = apply(&once, "linear", "https://mcp.linear.app/mcp").unwrap();
        assert_eq!(once, twice);
        assert!(twice.contains(r#"deny = ["delete_*"]"#));
    }

    #[cfg(unix)]
    #[test]
    fn backup_preserves_private_config_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "default_provider = \"openrouter\"\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        apply_to_file(&path, "linear", "https://mcp.linear.app/mcp").unwrap();

        let backup = path.with_extension("toml.bak");
        assert_eq!(
            std::fs::metadata(backup).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
