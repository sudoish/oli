//! `McpServer` — one per configured server. Owns the transport,
//! negotiated capabilities, and the latest `tools/list` snapshot.
//! Health is tracked here so a flaky server doesn't take down the REPL:
//! a failed call flips the server to `Down`, future tool dispatches
//! return a "currently unavailable" string instead.

use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;

use crate::error::{AgentError, Result};
use crate::mcp::config::{McpServerConfig, McpTransportKind, env_snapshot, expand_env_vars};
use crate::mcp::http::HttpTransport;
use crate::mcp::stdio::StdioTransport;
use crate::mcp::transport::McpTransport;

/// Protocol version we advertise to servers. The MCP spec runs a
/// version-negotiation step in `initialize`; we send our latest and
/// accept whatever the server returns. v1 doesn't gate on this — we
/// only support a single major version.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// One MCP tool's metadata as reported by `tools/list`. Fields mirror
/// the spec: `name` is the bare tool name (we namespace it on the way
/// into the registry); `inputSchema` is JSON Schema for arguments.
#[derive(Clone, Debug)]
pub struct ToolMeta {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Negotiated capability flags from `initialize`. v1 only consumes
/// `tools` — `resources` and `prompts` are reserved for phase 5c.
#[derive(Clone, Debug, Default)]
pub struct ServerCapabilities {
    pub tools: bool,
    pub resources: bool,
    pub prompts: bool,
}

#[derive(Clone, Debug)]
pub enum HealthState {
    Healthy,
    /// Server failed to come up at startup. The reason is what we
    /// surface in `/mcp` listings.
    Down(String),
}

pub struct McpServer {
    pub name: String,
    pub cfg: McpServerConfig,
    /// `None` while the server is `Down` — saves us from carrying a
    /// dead transport handle around. `connect` populates this on
    /// success.
    pub transport: Option<Arc<dyn McpTransport>>,
    pub capabilities: ServerCapabilities,
    pub tools: Vec<ToolMeta>,
    pub health: HealthState,
    /// Backing buffer for `/mcp logs <server>`. Populated by the stdio
    /// transport's stderr capture; for HTTP transports we'll wire this
    /// to a different source in phase 5b.
    pub stderr_source: Option<Arc<StdioTransport>>,
}

impl McpServer {
    /// Connect to the configured server: spawn transport, negotiate
    /// `initialize`, send `notifications/initialized`, fetch
    /// `tools/list`. Failures are absorbed into `HealthState::Down`
    /// so the REPL keeps working.
    pub async fn connect(name: &str, cfg: &McpServerConfig) -> Self {
        let mut server = Self {
            name: name.to_string(),
            cfg: cfg.clone(),
            transport: None,
            capabilities: ServerCapabilities::default(),
            tools: Vec::new(),
            health: HealthState::Down("not yet connected".into()),
            stderr_source: None,
        };

        match server.try_connect().await {
            Ok(()) => server.health = HealthState::Healthy,
            Err(e) => {
                eprintln!("mcp: server `{}` failed to start: {}", name, e);
                server.health = HealthState::Down(e.to_string());
            }
        }
        server
    }

    async fn try_connect(&mut self) -> Result<()> {
        match self.cfg.kind {
            McpTransportKind::Stdio => self.try_connect_stdio().await,
            McpTransportKind::StreamableHttp => self.try_connect_http().await,
        }
    }

    async fn try_connect_stdio(&mut self) -> Result<()> {
        let command = self
            .cfg
            .command
            .as_ref()
            .ok_or_else(|| AgentError::Config(format!("mcp `{}`: missing `command`", self.name)))?;
        let host_env = env_snapshot();
        // Expand `${VAR}` in env values now so a missing var fails the
        // server cleanly instead of silently passing an unsubstituted
        // template through to the child.
        let mut expanded_env = std::collections::HashMap::new();
        for (k, v) in &self.cfg.env {
            let expanded = expand_env_vars(v, &host_env).map_err(|e| {
                AgentError::Config(format!("mcp `{}` env {}: {}", self.name, k, e))
            })?;
            expanded_env.insert(k.clone(), expanded);
        }

        let transport = StdioTransport::spawn(command, &self.cfg.args, &expanded_env).await?;
        let transport_arc = Arc::new(transport);
        self.stderr_source = Some(transport_arc.clone());
        let dyn_transport: Arc<dyn McpTransport> = transport_arc.clone();
        self.transport = Some(dyn_transport.clone());

        self.run_handshake(dyn_transport).await
    }

    async fn try_connect_http(&mut self) -> Result<()> {
        let url = self
            .cfg
            .url
            .as_ref()
            .ok_or_else(|| AgentError::Config(format!("mcp `{}`: missing `url`", self.name)))?;
        let host_env = env_snapshot();
        let mut expanded_headers = std::collections::HashMap::new();
        for (k, v) in &self.cfg.headers {
            let expanded = expand_env_vars(v, &host_env).map_err(|e| {
                AgentError::Config(format!("mcp `{}` header {}: {}", self.name, k, e))
            })?;
            expanded_headers.insert(k.clone(), expanded);
        }

        let transport = HttpTransport::new(url.clone(), expanded_headers);
        // HTTP servers don't expose a stderr stream — `/mcp logs` is a
        // no-op for them. We leave `stderr_source` at None.
        let dyn_transport: Arc<dyn McpTransport> = Arc::new(transport);
        self.transport = Some(dyn_transport.clone());

        self.run_handshake(dyn_transport).await
    }

    /// Common handshake shared by both transports: initialize →
    /// notifications/initialized → tools/list. Owns the timeout
    /// budgets and populates `capabilities` + `tools`.
    async fn run_handshake(&mut self, transport: Arc<dyn McpTransport>) -> Result<()> {
        let init_dur = Duration::from_millis(self.cfg.init_timeout_ms);

        // 1. initialize
        let init_params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {
                // v1: client doesn't expose roots/sampling.
                "roots": { "listChanged": false },
            },
            "clientInfo": {
                "name": "oli",
                "version": env!("CARGO_PKG_VERSION"),
            }
        });
        let init_result = timeout(init_dur, transport.request("initialize", init_params))
            .await
            .map_err(|_| {
                AgentError::Provider(format!(
                    "mcp `{}` initialize timed out after {}ms",
                    self.name, self.cfg.init_timeout_ms
                ))
            })??;
        self.capabilities = parse_capabilities(&init_result);

        // 2. notifications/initialized — required after initialize.
        transport
            .notify("notifications/initialized", json!({}))
            .await?;

        // 3. tools/list — only meaningful if the server advertises tools.
        if self.capabilities.tools {
            let list_result = timeout(init_dur, transport.request("tools/list", json!({}))).await;
            match list_result {
                Ok(Ok(v)) => {
                    self.tools = parse_tool_list(&v);
                }
                Ok(Err(e)) => {
                    eprintln!("mcp `{}` tools/list failed: {}", self.name, e);
                }
                Err(_) => {
                    eprintln!(
                        "mcp `{}` tools/list timed out after {}ms",
                        self.name, self.cfg.init_timeout_ms
                    );
                }
            }
        }

        Ok(())
    }

    /// Invoke a tool on this server. Bounded by `call_timeout_ms`.
    /// Errors come back as `Err(AgentError)` so the caller decides
    /// whether to surface them to the model verbatim or wrap.
    pub async fn call_tool(&self, tool_name: &str, args: Value) -> Result<Value> {
        let transport = self.transport.as_ref().ok_or_else(|| {
            AgentError::Provider(format!(
                "mcp server `{}` is unavailable: {}",
                self.name,
                self.health_reason()
            ))
        })?;
        let dur = Duration::from_millis(self.cfg.call_timeout_ms);
        let params = json!({
            "name": tool_name,
            "arguments": args,
        });
        let res = timeout(dur, transport.request("tools/call", params))
            .await
            .map_err(|_| {
                AgentError::Provider(format!(
                    "mcp `{}__{}` call timed out after {}ms",
                    self.name, tool_name, self.cfg.call_timeout_ms
                ))
            })??;
        Ok(res)
    }

    fn health_reason(&self) -> String {
        match &self.health {
            HealthState::Healthy => "(unexpectedly healthy)".into(),
            HealthState::Down(r) => r.clone(),
        }
    }

    pub async fn close(&mut self) -> Result<()> {
        if let Some(t) = self.transport.take() {
            let _ = t.close().await;
        }
        Ok(())
    }

    /// Tear down the existing transport (if any) and re-run the
    /// handshake. Used by `/mcp restart` to recover a `Down` server
    /// or refresh a flaky connection mid-session.
    ///
    /// Tools registered into the parent `Registry` continue to point at
    /// this server via `Arc<Mutex<McpServer>>`, so they pick up the new
    /// state automatically. The set of *exposed* tools doesn't change
    /// without a full registry rebuild — if the server suddenly adds or
    /// drops tools, the user will see the deltas only on the next REPL
    /// session. That's acceptable for v1; spec defers the live-rebuild
    /// behavior.
    pub async fn restart(&mut self) -> Result<()> {
        if let Some(t) = self.transport.take() {
            let _ = t.close().await;
        }
        self.stderr_source = None;
        self.tools.clear();
        self.capabilities = ServerCapabilities::default();

        match self.try_connect().await {
            Ok(()) => {
                self.health = HealthState::Healthy;
                Ok(())
            }
            Err(e) => {
                let msg = e.to_string();
                self.health = HealthState::Down(msg);
                Err(e)
            }
        }
    }

    /// Pull captured stderr (stdio only). HTTP transport returns empty.
    pub async fn stderr_snapshot(&self) -> String {
        match &self.stderr_source {
            Some(s) => s.stderr_snapshot().await,
            None => String::new(),
        }
    }
}

fn parse_capabilities(init_result: &Value) -> ServerCapabilities {
    let caps = init_result.get("capabilities");
    ServerCapabilities {
        tools: caps.and_then(|c| c.get("tools")).is_some(),
        resources: caps.and_then(|c| c.get("resources")).is_some(),
        prompts: caps.and_then(|c| c.get("prompts")).is_some(),
    }
}

fn parse_tool_list(list_result: &Value) -> Vec<ToolMeta> {
    let arr = match list_result.get("tools").and_then(|v| v.as_array()) {
        Some(a) => a,
        None => return Vec::new(),
    };
    arr.iter()
        .filter_map(|t| {
            let name = t.get("name").and_then(|v| v.as_str())?.to_string();
            let description = t
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let input_schema = t
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
            Some(ToolMeta {
                name,
                description,
                input_schema,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_capabilities_picks_up_tools_block() {
        let v = json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {
                "tools": {},
                "resources": {},
            }
        });
        let caps = parse_capabilities(&v);
        assert!(caps.tools);
        assert!(caps.resources);
        assert!(!caps.prompts);
    }

    #[test]
    fn parse_capabilities_handles_missing_block() {
        let v = json!({"protocolVersion": "2025-06-18"});
        let caps = parse_capabilities(&v);
        assert!(!caps.tools);
        assert!(!caps.resources);
        assert!(!caps.prompts);
    }

    #[test]
    fn parse_tool_list_extracts_metadata_per_tool() {
        let v = json!({
            "tools": [
                {
                    "name": "get_issue",
                    "description": "fetch an issue",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "id": {"type": "string"}
                        },
                        "required": ["id"]
                    }
                },
                {
                    "name": "list_issues"
                    // description omitted; inputSchema omitted
                }
            ]
        });
        let tools = parse_tool_list(&v);
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "get_issue");
        assert_eq!(tools[0].description, "fetch an issue");
        assert_eq!(tools[0].input_schema["properties"]["id"]["type"], "string");
        assert_eq!(tools[1].name, "list_issues");
        assert_eq!(tools[1].description, "");
        assert_eq!(tools[1].input_schema["type"], "object");
    }

    #[test]
    fn parse_tool_list_returns_empty_when_no_tools_field() {
        let v = json!({});
        assert_eq!(parse_tool_list(&v).len(), 0);
    }

    /// End-to-end lifecycle smoke test against a fake MCP server
    /// written in Python: initialize → tools/list → tools/call.
    /// Skipped if Python isn't on PATH, so this test is portable but
    /// only meaningful in environments where it can run.
    #[tokio::test]
    async fn lifecycle_initialize_list_call_against_python_fake() {
        if which("python3").is_none() {
            eprintln!("skipping: python3 not on PATH");
            return;
        }
        let cfg = McpServerConfig {
            kind: McpTransportKind::Stdio,
            command: Some("python3".into()),
            args: vec!["-u".into(), "-c".into(), FAKE_FULL_SERVER_PY.into()],
            env: Default::default(),
            url: None,
            headers: Default::default(),
            init_timeout_ms: 5000,
            call_timeout_ms: 60_000,
            tools: Default::default(),
            enabled: true,
        };
        let server = McpServer::connect("fake", &cfg).await;
        assert!(matches!(server.health, HealthState::Healthy));
        assert!(server.capabilities.tools);
        assert_eq!(server.tools.len(), 1);
        assert_eq!(server.tools[0].name, "echo");
        assert_eq!(server.tools[0].description, "echo the input");

        // tools/call roundtrip.
        let result = server
            .call_tool("echo", json!({"text": "hello"}))
            .await
            .expect("call_tool");
        let content = result["content"][0]["text"].as_str().unwrap();
        assert_eq!(content, "got: hello");
    }

    /// A minimal MCP server in Python, embedded as a `python3 -c` arg.
    /// Inline so the test doesn't fight with the filesystem when many
    /// MCP tests run in parallel — passing it via `-c` keeps the
    /// startup fast and deterministic.
    const FAKE_FULL_SERVER_PY: &str = r#"
import json, sys
def respond(req, result=None, error=None):
    body = {"jsonrpc": "2.0", "id": req.get("id")}
    if error is not None:
        body["error"] = error
    else:
        body["result"] = result
    sys.stdout.write(json.dumps(body) + "\n")
    sys.stdout.flush()

# iter(readline, '') avoids the for-line-in-stdin buffer fill that
# can cause flakes when the parent writes before Python is ready.
for line in iter(sys.stdin.readline, ''):
    line = line.strip()
    if not line: continue
    msg = json.loads(line)
    method = msg.get("method")
    if method == "initialize":
        respond(msg, result={"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"fake","version":"0.0.0"}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        respond(msg, result={"tools":[{"name":"echo","description":"echo the input","inputSchema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}}]})
    elif method == "tools/call":
        text = msg.get("params", {}).get("arguments", {}).get("text", "")
        respond(msg, result={"content":[{"type":"text","text":"got: " + text}]})
    else:
        respond(msg, error={"code": -32601, "message": "method not found"})
"#;

    /// A trivial ping/pong fake for the restart test — same shape as
    /// the full server but with one canned tool.
    const FAKE_PING_SERVER_PY: &str = r#"
import json, sys
def respond(req, result=None):
    sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":req.get("id"),"result":result}) + "\n")
    sys.stdout.flush()
for line in iter(sys.stdin.readline, ''):
    line = line.strip()
    if not line: continue
    msg = json.loads(line)
    m = msg.get("method")
    if m == "initialize":
        respond(msg, {"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"fake","version":"0.0.0"}})
    elif m == "notifications/initialized":
        pass
    elif m == "tools/list":
        respond(msg, {"tools":[{"name":"ping","description":"p","inputSchema":{"type":"object"}}]})
    elif m == "tools/call":
        respond(msg, {"content":[{"type":"text","text":"pong"}]})
"#;

    /// Servers that fail to come up don't take down the harness — they
    /// land in `HealthState::Down(reason)` so `/mcp` can show why.
    #[tokio::test]
    async fn connect_failure_lands_in_health_down() {
        let cfg = McpServerConfig {
            kind: McpTransportKind::Stdio,
            command: Some("/no/such/binary".into()),
            args: vec![],
            env: Default::default(),
            url: None,
            headers: Default::default(),
            init_timeout_ms: 1000,
            call_timeout_ms: 1000,
            tools: Default::default(),
            enabled: true,
        };
        let server = McpServer::connect("missing", &cfg).await;
        match server.health {
            HealthState::Down(_) => {}
            HealthState::Healthy => panic!("expected Down for nonexistent binary"),
        }
        assert!(server.tools.is_empty());
    }

    /// Streamable-http config without a reachable URL should fail
    /// fast through the transport (DNS / connection refused) rather
    /// than returning a generic "not supported" error.
    #[tokio::test]
    async fn streamable_http_with_unreachable_url_lands_in_down() {
        let cfg = McpServerConfig {
            kind: McpTransportKind::StreamableHttp,
            command: None,
            args: vec![],
            env: Default::default(),
            // 127.0.0.1 with a port nothing's listening on — connection
            // refused fires immediately on most systems.
            url: Some("http://127.0.0.1:1/".into()),
            headers: Default::default(),
            init_timeout_ms: 1000,
            call_timeout_ms: 1000,
            tools: Default::default(),
            enabled: true,
        };
        let server = McpServer::connect("hosted", &cfg).await;
        match server.health {
            HealthState::Down(_) => {}
            HealthState::Healthy => panic!("expected Down for unreachable URL"),
        }
    }

    /// `restart` should re-spawn the transport and re-run the handshake.
    /// We verify by connecting against the Python fake, calling once,
    /// then restarting and calling again — the second call has to go
    /// through a fresh transport because the first child has already
    /// been torn down.
    #[tokio::test]
    async fn restart_re_runs_initialize_and_tools_list() {
        if which("python3").is_none() {
            eprintln!("skipping: python3 not on PATH");
            return;
        }
        let cfg = McpServerConfig {
            kind: McpTransportKind::Stdio,
            command: Some("python3".into()),
            args: vec!["-u".into(), "-c".into(), FAKE_PING_SERVER_PY.into()],
            env: Default::default(),
            url: None,
            headers: Default::default(),
            init_timeout_ms: 5000,
            call_timeout_ms: 60_000,
            tools: Default::default(),
            enabled: true,
        };
        let mut server = McpServer::connect("fake", &cfg).await;
        assert!(matches!(server.health, HealthState::Healthy));
        let r1 = server
            .call_tool("ping", json!({}))
            .await
            .expect("first call");
        assert_eq!(r1["content"][0]["text"], "pong");

        // Restart and call again — fresh transport, fresh handshake.
        server.restart().await.expect("restart");
        assert!(matches!(server.health, HealthState::Healthy));
        assert_eq!(server.tools.len(), 1);
        let r2 = server
            .call_tool("ping", json!({}))
            .await
            .expect("second call after restart");
        assert_eq!(r2["content"][0]["text"], "pong");
    }

    /// Restart of a misconfigured server should leave it `Down` with
    /// the new failure reason, not silently succeed or panic.
    #[tokio::test]
    async fn restart_failure_lands_in_down() {
        let cfg = McpServerConfig {
            kind: McpTransportKind::Stdio,
            command: Some("/no/such/binary".into()),
            args: vec![],
            env: Default::default(),
            url: None,
            headers: Default::default(),
            init_timeout_ms: 1000,
            call_timeout_ms: 1000,
            tools: Default::default(),
            enabled: true,
        };
        let mut server = McpServer::connect("missing", &cfg).await;
        assert!(matches!(server.health, HealthState::Down(_)));
        let res = server.restart().await;
        assert!(res.is_err());
        assert!(matches!(server.health, HealthState::Down(_)));
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
