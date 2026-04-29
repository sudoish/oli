//! MCP config schema. Each `[mcp.servers.<id>]` table becomes one
//! `McpServerConfig`. Env-var substitution is `${VAR}` style; missing
//! vars fail the server at spawn time with a clear error so the user
//! knows which var to set.

use serde::Deserialize;
use std::collections::HashMap;

#[derive(Clone, Debug, Default, Deserialize)]
pub struct McpConfig {
    /// Per-server entries. Key is the user-facing identifier (used as
    /// the prefix in the `<id>__<tool>` namespaced tool name).
    #[serde(default)]
    pub servers: HashMap<String, McpServerConfig>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct McpServerConfig {
    pub kind: McpTransportKind,

    /// stdio: executable to run (e.g. `npx`, `uvx`, `docker`).
    #[serde(default)]
    pub command: Option<String>,

    /// stdio: arguments after `command`.
    #[serde(default)]
    pub args: Vec<String>,

    /// stdio: env vars to set on the child. Values support `${VAR}` to
    /// substitute from the harness's environment.
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// streamable-http: endpoint URL. Reserved for phase 5b.
    #[serde(default)]
    pub url: Option<String>,

    /// streamable-http: extra headers (typically `Authorization`).
    /// Reserved for phase 5b.
    #[serde(default)]
    pub headers: HashMap<String, String>,

    /// Initialize timeout in milliseconds. Default 5000.
    #[serde(default = "default_init_timeout")]
    pub init_timeout_ms: u64,

    /// Per-call timeout in milliseconds. Default 60000.
    #[serde(default = "default_call_timeout")]
    pub call_timeout_ms: u64,

    /// Per-server tool filter. Empty allow = expose all (subject to
    /// deny). Project overlay can flip this to `false` via `enabled`
    /// without removing the global definition.
    #[serde(default)]
    pub tools: McpToolFilter,

    /// Project overlay can flip this to `false` to disable a globally-
    /// configured server in a specific repo without redefining it.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum McpTransportKind {
    Stdio,
    StreamableHttp,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct McpToolFilter {
    /// Glob patterns; if non-empty, only tools whose names match at
    /// least one pattern are exposed. Empty = expose all.
    #[serde(default)]
    pub allow: Vec<String>,

    /// Glob patterns; tools whose names match are dropped. Applied
    /// after `allow`.
    #[serde(default)]
    pub deny: Vec<String>,
}

impl McpToolFilter {
    /// True if `tool` survives both the allow and deny filters.
    pub fn allows(&self, tool: &str) -> bool {
        let allowed = if self.allow.is_empty() {
            true
        } else {
            self.allow.iter().any(|p| glob_match(p, tool))
        };
        let denied = self.deny.iter().any(|p| glob_match(p, tool));
        allowed && !denied
    }
}

fn default_init_timeout() -> u64 {
    5_000
}

fn default_call_timeout() -> u64 {
    60_000
}

fn default_enabled() -> bool {
    true
}

/// Tiny shell-style glob: `*` matches any run of chars, `?` matches one.
/// Anything else matches literally. We don't need full glob() semantics
/// — MCP tool names are flat identifiers without slashes.
fn glob_match(pattern: &str, s: &str) -> bool {
    fn helper(p: &[u8], s: &[u8]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some(b'*'), _) => {
                // Match zero or more chars.
                if helper(&p[1..], s) {
                    return true;
                }
                if !s.is_empty() {
                    return helper(p, &s[1..]);
                }
                false
            }
            (Some(b'?'), Some(_)) => helper(&p[1..], &s[1..]),
            (Some(pc), Some(sc)) if pc == sc => helper(&p[1..], &s[1..]),
            _ => false,
        }
    }
    helper(pattern.as_bytes(), s.as_bytes())
}

/// Substitute `${VAR}` references against `vars`. Missing vars produce
/// an error tagged with the var name so the user knows what to set.
pub fn expand_env_vars(
    s: &str,
    vars: &HashMap<String, String>,
) -> std::result::Result<String, String> {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            if let Some(end) = bytes[i + 2..].iter().position(|&b| b == b'}') {
                let name = std::str::from_utf8(&bytes[i + 2..i + 2 + end])
                    .map_err(|_| "non-utf8 var name".to_string())?;
                let value = vars
                    .get(name)
                    .cloned()
                    .ok_or_else(|| format!("env var {} is not set", name))?;
                out.push_str(&value);
                i += 2 + end + 1;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    Ok(out)
}

/// Snapshot of the harness's environment as a HashMap. Convenient for
/// `expand_env_vars` so callers don't have to thread the full env.
pub fn env_snapshot() -> HashMap<String, String> {
    std::env::vars().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_star_matches_prefix() {
        assert!(glob_match("get_*", "get_issue"));
        assert!(glob_match("get_*", "get_"));
        assert!(!glob_match("get_*", "save_issue"));
    }

    #[test]
    fn glob_question_mark_matches_one_char() {
        assert!(glob_match("ge?", "get"));
        assert!(!glob_match("ge?", "ge"));
        assert!(!glob_match("ge?", "geet"));
    }

    #[test]
    fn glob_literal_match() {
        assert!(glob_match("save_issue", "save_issue"));
        assert!(!glob_match("save_issue", "save_comment"));
    }

    #[test]
    fn allow_empty_means_expose_all() {
        let f = McpToolFilter::default();
        assert!(f.allows("get_issue"));
        assert!(f.allows("delete_user"));
    }

    #[test]
    fn allow_restricts_when_set() {
        let f = McpToolFilter {
            allow: vec!["get_*".into(), "list_*".into()],
            deny: vec![],
        };
        assert!(f.allows("get_issue"));
        assert!(f.allows("list_users"));
        assert!(!f.allows("delete_issue"));
    }

    #[test]
    fn deny_overrides_allow() {
        let f = McpToolFilter {
            allow: vec!["*".into()],
            deny: vec!["delete_*".into()],
        };
        assert!(f.allows("get_issue"));
        assert!(!f.allows("delete_issue"));
    }

    #[test]
    fn env_var_expansion_substitutes() {
        let mut vars = HashMap::new();
        vars.insert("LINEAR_API_KEY".into(), "secret".into());
        let out = expand_env_vars("Bearer ${LINEAR_API_KEY}", &vars).unwrap();
        assert_eq!(out, "Bearer secret");
    }

    #[test]
    fn env_var_expansion_errors_on_missing() {
        let vars = HashMap::new();
        let err = expand_env_vars("Bearer ${MISSING}", &vars).unwrap_err();
        assert!(err.contains("MISSING"));
    }

    #[test]
    fn env_var_expansion_handles_no_substitutions() {
        let vars = HashMap::new();
        let out = expand_env_vars("plain string", &vars).unwrap();
        assert_eq!(out, "plain string");
    }

    #[test]
    fn env_var_expansion_handles_multiple_substitutions() {
        let mut vars = HashMap::new();
        vars.insert("A".into(), "1".into());
        vars.insert("B".into(), "2".into());
        let out = expand_env_vars("${A}-${B}", &vars).unwrap();
        assert_eq!(out, "1-2");
    }

    #[test]
    fn parses_full_server_config() {
        let toml = r#"
            [mcp.servers.linear]
            kind = "stdio"
            command = "npx"
            args = ["-y", "@linear/mcp-server"]
            env = { LINEAR_API_KEY = "${LINEAR_API_KEY}" }

            [mcp.servers.linear.tools]
            allow = ["get_*", "list_*"]
            deny = ["delete_*"]
        "#;
        #[derive(Deserialize)]
        struct Wrapper {
            mcp: McpConfig,
        }
        let w: Wrapper = toml::from_str(toml).unwrap();
        let s = &w.mcp.servers["linear"];
        assert_eq!(s.kind, McpTransportKind::Stdio);
        assert_eq!(s.command.as_deref(), Some("npx"));
        assert_eq!(s.args, vec!["-y", "@linear/mcp-server"]);
        assert_eq!(s.tools.allow, vec!["get_*", "list_*"]);
        assert_eq!(s.tools.deny, vec!["delete_*"]);
        assert!(s.enabled);
        assert_eq!(s.init_timeout_ms, 5_000);
        assert_eq!(s.call_timeout_ms, 60_000);
    }

    #[test]
    fn parses_streamable_http_server_config() {
        let toml = r#"
            [mcp.servers.sentry]
            kind = "streamable-http"
            url = "https://mcp.sentry.dev"
            headers = { Authorization = "Bearer ${SENTRY_TOKEN}" }
        "#;
        #[derive(Deserialize)]
        struct Wrapper {
            mcp: McpConfig,
        }
        let w: Wrapper = toml::from_str(toml).unwrap();
        let s = &w.mcp.servers["sentry"];
        assert_eq!(s.kind, McpTransportKind::StreamableHttp);
        assert_eq!(s.url.as_deref(), Some("https://mcp.sentry.dev"));
        assert_eq!(s.headers["Authorization"], "Bearer ${SENTRY_TOKEN}");
    }

    #[test]
    fn enabled_defaults_to_true() {
        let toml = r#"
            [mcp.servers.x]
            kind = "stdio"
            command = "true"
        "#;
        #[derive(Deserialize)]
        struct Wrapper {
            mcp: McpConfig,
        }
        let w: Wrapper = toml::from_str(toml).unwrap();
        assert!(w.mcp.servers["x"].enabled);
    }

    #[test]
    fn enabled_can_be_disabled_via_overlay() {
        let toml = r#"
            [mcp.servers.x]
            kind = "stdio"
            command = "true"
            enabled = false
        "#;
        #[derive(Deserialize)]
        struct Wrapper {
            mcp: McpConfig,
        }
        let w: Wrapper = toml::from_str(toml).unwrap();
        assert!(!w.mcp.servers["x"].enabled);
    }
}
