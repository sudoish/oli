//! OAuth 2.1 authorization for streamable-HTTP MCP servers.
//!
//! Discovery follows the MCP authorization specification: an unauthenticated
//! request identifies protected-resource metadata, which identifies the
//! authorization server. Public clients are registered dynamically and use
//! authorization-code + PKCE. Credentials are stored per MCP server outside
//! `config.toml` and refreshed before use.

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::auth::listener::{CallbackServer, parse_pasted_redirect};
use crate::auth::{CALLBACK_PATH, form_encode, pkce};
use crate::error::{AgentError, Result};

const PASTE_CALLBACK_PORT: u16 = 1458;
const EXPIRY_SKEW_SECONDS: u64 = 60;

#[derive(Clone, Debug, Deserialize)]
struct ProtectedResourceMetadata {
    resource: String,
    authorization_servers: Vec<String>,
    #[serde(default)]
    scopes_supported: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct AuthorizationServerMetadata {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    registration_endpoint: Option<String>,
    #[serde(default)]
    scopes_supported: Vec<String>,
    #[serde(default)]
    code_challenge_methods_supported: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct RegistrationResponse {
    client_id: String,
    #[serde(default)]
    client_secret: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpOAuthCredential {
    pub server_name: String,
    pub resource: String,
    pub issuer: String,
    pub token_endpoint: String,
    pub client_id: String,
    #[serde(default)]
    pub client_secret: Option<String>,
    pub redirect_uri: String,
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_at: Option<u64>,
    #[serde(default)]
    pub scope: Option<String>,
}

impl McpOAuthCredential {
    fn access_token_is_fresh(&self) -> bool {
        self.expires_at
            .is_none_or(|expiry| expiry > now_unix().saturating_add(EXPIRY_SKEW_SECONDS))
    }
}

#[derive(Clone, Debug)]
pub struct McpOAuthStore {
    dir: PathBuf,
}

impl McpOAuthStore {
    pub fn default_location() -> Result<Self> {
        let dir = default_store_dir().ok_or_else(|| {
            AgentError::Auth("cannot locate a config directory; set XDG_CONFIG_HOME or HOME".into())
        })?;
        Ok(Self::at(dir))
    }

    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn load(&self, server_name: &str) -> Result<Option<McpOAuthCredential>> {
        let path = self.path_for(server_name);
        let body = match std::fs::read_to_string(&path) {
            Ok(body) => body,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(AgentError::Auth(format!(
                    "cannot read MCP credentials from {}: {error}",
                    path.display()
                )));
            }
        };
        serde_json::from_str(&body).map(Some).map_err(|error| {
            AgentError::Auth(format!(
                "{} is corrupt ({error}); run `oli mcp login {server_name}` again",
                path.display()
            ))
        })
    }

    pub fn save(&self, credential: &McpOAuthCredential) -> Result<()> {
        std::fs::create_dir_all(&self.dir).map_err(|error| {
            AgentError::Auth(format!("cannot create {}: {error}", self.dir.display()))
        })?;
        let path = self.path_for(&credential.server_name);
        let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
        let body = serde_json::to_vec_pretty(credential)?;
        let mut file = open_private(&tmp)?;
        if let Err(error) = file
            .write_all(&body)
            .and_then(|()| file.write_all(b"\n"))
            .and_then(|()| file.sync_all())
        {
            let _ = std::fs::remove_file(&tmp);
            return Err(AgentError::Auth(format!(
                "cannot write {}: {error}",
                tmp.display()
            )));
        }
        drop(file);
        std::fs::rename(&tmp, &path).map_err(|error| {
            let _ = std::fs::remove_file(&tmp);
            AgentError::Auth(format!("cannot replace {}: {error}", path.display()))
        })
    }

    pub fn clear(&self, server_name: &str) -> Result<bool> {
        let path = self.path_for(server_name);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(AgentError::Auth(format!(
                "cannot remove {}: {error}",
                path.display()
            ))),
        }
    }

    fn path_for(&self, server_name: &str) -> PathBuf {
        let digest = Sha256::digest(server_name.as_bytes());
        self.dir.join(format!("{digest:x}.json"))
    }
}

pub fn default_store_dir() -> Option<PathBuf> {
    Some(crate::config::config_dir()?.join("mcp-auth"))
}

#[derive(Clone)]
pub struct McpOAuthSession {
    server_name: String,
    store: McpOAuthStore,
    http: reqwest::Client,
}

impl McpOAuthSession {
    pub fn new(server_name: impl Into<String>) -> Result<Self> {
        Ok(Self {
            server_name: server_name.into(),
            store: McpOAuthStore::default_location()?,
            http: reqwest::Client::new(),
        })
    }

    #[cfg(test)]
    pub fn with_store(server_name: impl Into<String>, store: McpOAuthStore) -> Self {
        Self {
            server_name: server_name.into(),
            store,
            http: reqwest::Client::new(),
        }
    }

    pub async fn access_token(&self) -> Result<String> {
        let mut credential = self.store.load(&self.server_name)?.ok_or_else(|| {
            AgentError::Auth(format!(
                "MCP server `{}` is not authorized; run `oli mcp login {}`",
                self.server_name, self.server_name
            ))
        })?;
        if credential.access_token_is_fresh() {
            return Ok(credential.access_token);
        }
        refresh(&self.http, &mut credential).await?;
        self.store.save(&credential)?;
        Ok(credential.access_token)
    }
}

pub async fn login(
    server_name: &str,
    resource_url: &str,
    paste: bool,
    requested_scopes: &[String],
) -> Result<McpOAuthCredential> {
    ensure_https(resource_url, "MCP resource")?;
    let http = reqwest::Client::new();
    let (resource, authorization) = discover(&http, resource_url).await?;
    ensure_https(&resource.resource, "protected resource")?;
    if resource.resource.trim_end_matches('/') != resource_url.trim_end_matches('/') {
        return Err(AgentError::Auth(format!(
            "MCP resource mismatch: connected to {resource_url}, metadata identifies {}",
            resource.resource
        )));
    }
    ensure_https(&authorization.issuer, "authorization server")?;
    ensure_https(
        &authorization.authorization_endpoint,
        "authorization endpoint",
    )?;
    ensure_https(&authorization.token_endpoint, "token endpoint")?;
    if !authorization
        .code_challenge_methods_supported
        .iter()
        .any(|method| method == "S256")
    {
        return Err(AgentError::Auth(
            "the MCP authorization server does not advertise PKCE S256".into(),
        ));
    }

    let scopes = select_scopes(requested_scopes, &resource, &authorization)?;
    let callback = if paste {
        None
    } else {
        Some(CallbackServer::bind_ports(&[0]).await?)
    };
    let port = callback
        .as_ref()
        .map(CallbackServer::port)
        .unwrap_or(PASTE_CALLBACK_PORT);
    let redirect_uri = format!("http://localhost:{port}{CALLBACK_PATH}");
    let registration = register_client(&http, &authorization, &redirect_uri).await?;
    let pkce = pkce::generate();
    let state = pkce::generate_state();
    let authorize_url = build_authorize_url(
        &authorization.authorization_endpoint,
        &registration.client_id,
        &redirect_uri,
        &resource.resource,
        &scopes,
        &pkce,
        &state,
    );

    println!("Authorize Oli to use MCP server `{server_name}`:\n\n  {authorize_url}\n");
    let code = if let Some(callback) = callback {
        if crate::auth::login::open_browser(&authorize_url) {
            println!("Opened your browser. Waiting for authorization…");
        } else {
            println!(
                "Open the URL above in a browser on this machine.\n\
                 Browser on another machine? Ctrl-C and rerun with `--paste`."
            );
        }
        callback.accept(&state).await?.code
    } else {
        println!(
            "After authorization, the browser will try to open {redirect_uri} and may show \
             a connection error. Copy the full URL from the address bar and paste it here."
        );
        let pasted = crate::auth::login::read_line("Pasted URL: ").await?;
        parse_pasted_redirect(&pasted, &state)?.code
    };

    let token = exchange_code(
        &http,
        &authorization.token_endpoint,
        &registration,
        &redirect_uri,
        &resource.resource,
        &pkce.verifier,
        &code,
    )
    .await?;
    let credential = credential_from_token(
        server_name,
        &resource,
        &authorization,
        &registration,
        &redirect_uri,
        &scopes,
        token,
    );
    McpOAuthStore::default_location()?.save(&credential)?;
    Ok(credential)
}

pub async fn status(server_name: &str) -> Result<Option<McpOAuthCredential>> {
    McpOAuthStore::default_location()?.load(server_name)
}

pub fn logout(server_name: &str) -> Result<bool> {
    McpOAuthStore::default_location()?.clear(server_name)
}

async fn discover(
    http: &reqwest::Client,
    resource_url: &str,
) -> Result<(ProtectedResourceMetadata, AuthorizationServerMetadata)> {
    let challenge = probe_resource_metadata(http, resource_url).await?;
    ensure_https(&challenge, "protected-resource metadata")?;
    let resource: ProtectedResourceMetadata =
        get_json(http, &challenge, "resource metadata").await?;
    let issuer = resource.authorization_servers.first().ok_or_else(|| {
        AgentError::Auth("MCP resource metadata names no authorization server".into())
    })?;
    ensure_https(issuer, "authorization server")?;
    let metadata_url = format!(
        "{}/.well-known/oauth-authorization-server",
        issuer.trim_end_matches('/')
    );
    let authorization: AuthorizationServerMetadata =
        match get_json(http, &metadata_url, "authorization-server metadata").await {
            Ok(metadata) => metadata,
            Err(oauth_error) => {
                let oidc_url = format!(
                    "{}/.well-known/openid-configuration",
                    issuer.trim_end_matches('/')
                );
                get_json(http, &oidc_url, "OpenID provider metadata")
                    .await
                    .map_err(|oidc_error| {
                        AgentError::Auth(format!(
                            "authorization metadata discovery failed: {oauth_error}; \
                             OpenID fallback failed: {oidc_error}"
                        ))
                    })?
            }
        };
    if authorization.issuer.trim_end_matches('/') != issuer.trim_end_matches('/') {
        return Err(AgentError::Auth(format!(
            "authorization issuer mismatch: resource named {issuer}, metadata named {}",
            authorization.issuer
        )));
    }
    Ok((resource, authorization))
}

async fn probe_resource_metadata(http: &reqwest::Client, resource_url: &str) -> Result<String> {
    let response = http
        .post(resource_url)
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "oli", "version": env!("CARGO_PKG_VERSION")}
            }
        }))
        .send()
        .await
        .map_err(|error| AgentError::Auth(format!("cannot reach MCP server: {error}")))?;
    let header = response
        .headers()
        .get(reqwest::header::WWW_AUTHENTICATE)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            AgentError::Auth(format!(
                "MCP server returned HTTP {} without an OAuth challenge",
                response.status()
            ))
        })?;
    challenge_parameter(header, "resource_metadata").ok_or_else(|| {
        AgentError::Auth("MCP OAuth challenge did not include `resource_metadata`".into())
    })
}

fn challenge_parameter(header: &str, name: &str) -> Option<String> {
    header.split(',').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key.trim() == name).then(|| value.trim().trim_matches('"').to_string())
    })
}

async fn get_json<T: for<'de> Deserialize<'de>>(
    http: &reqwest::Client,
    url: &str,
    label: &str,
) -> Result<T> {
    let response =
        http.get(url).send().await.map_err(|error| {
            AgentError::Auth(format!("cannot fetch {label} from {url}: {error}"))
        })?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(AgentError::Auth(format!(
            "{label} endpoint returned HTTP {}: {body}",
            status.as_u16()
        )));
    }
    serde_json::from_str(&body)
        .map_err(|error| AgentError::Auth(format!("cannot parse {label}: {error}")))
}

fn select_scopes(
    requested: &[String],
    resource: &ProtectedResourceMetadata,
    authorization: &AuthorizationServerMetadata,
) -> Result<Vec<String>> {
    let scopes = if requested.is_empty() {
        resource.scopes_supported.clone()
    } else {
        requested.to_vec()
    };
    for scope in &scopes {
        if !resource.scopes_supported.is_empty() && !resource.scopes_supported.contains(scope) {
            return Err(AgentError::Auth(format!(
                "MCP resource does not support requested scope `{scope}`"
            )));
        }
        if !authorization.scopes_supported.is_empty()
            && !authorization.scopes_supported.contains(scope)
        {
            return Err(AgentError::Auth(format!(
                "authorization server does not support requested scope `{scope}`"
            )));
        }
    }
    Ok(scopes)
}

async fn register_client(
    http: &reqwest::Client,
    metadata: &AuthorizationServerMetadata,
    redirect_uri: &str,
) -> Result<RegistrationResponse> {
    let endpoint = metadata.registration_endpoint.as_ref().ok_or_else(|| {
        AgentError::Auth(
            "the MCP authorization server does not support dynamic client registration".into(),
        )
    })?;
    ensure_https(endpoint, "client-registration endpoint")?;
    let response = http
        .post(endpoint)
        .json(&json!({
            "client_name": "oli",
            "redirect_uris": [redirect_uri],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none"
        }))
        .send()
        .await
        .map_err(|error| AgentError::Auth(format!("client registration failed: {error}")))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(AgentError::Auth(format!(
            "client registration returned HTTP {}: {body}",
            status.as_u16()
        )));
    }
    serde_json::from_str(&body)
        .map_err(|error| AgentError::Auth(format!("cannot parse client registration: {error}")))
}

fn build_authorize_url(
    endpoint: &str,
    client_id: &str,
    redirect_uri: &str,
    resource: &str,
    scopes: &[String],
    pkce: &pkce::Pkce,
    state: &str,
) -> String {
    let scope = scopes.join(" ");
    let mut params = vec![
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
        ("resource", resource),
        ("code_challenge", &pkce.challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
    ];
    if !scope.is_empty() {
        params.push(("scope", &scope));
    }
    let query = params
        .iter()
        .map(|(key, value)| format!("{key}={}", form_encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{}?{query}", endpoint.trim_end_matches('?'))
}

async fn exchange_code(
    http: &reqwest::Client,
    endpoint: &str,
    registration: &RegistrationResponse,
    redirect_uri: &str,
    resource: &str,
    verifier: &str,
    code: &str,
) -> Result<TokenResponse> {
    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", &registration.client_id),
        ("code_verifier", verifier),
        ("resource", resource),
    ];
    if let Some(secret) = &registration.client_secret {
        form.push(("client_secret", secret));
    }
    post_token(http, endpoint, &form).await
}

async fn refresh(http: &reqwest::Client, credential: &mut McpOAuthCredential) -> Result<()> {
    let refresh_token = credential.refresh_token.as_deref().ok_or_else(|| {
        AgentError::Auth(format!(
            "MCP credential for `{}` expired without a refresh token; run `oli mcp login {}`",
            credential.server_name, credential.server_name
        ))
    })?;
    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", credential.client_id.as_str()),
        ("resource", credential.resource.as_str()),
    ];
    if let Some(secret) = &credential.client_secret {
        form.push(("client_secret", secret));
    }
    let token = post_token(http, &credential.token_endpoint, &form).await?;
    credential.access_token = token.access_token;
    if token.refresh_token.is_some() {
        credential.refresh_token = token.refresh_token;
    }
    credential.expires_at = token
        .expires_in
        .map(|seconds| now_unix().saturating_add(seconds));
    if token.scope.is_some() {
        credential.scope = token.scope;
    }
    Ok(())
}

async fn post_token(
    http: &reqwest::Client,
    endpoint: &str,
    form: &[(&str, &str)],
) -> Result<TokenResponse> {
    let response = http
        .post(endpoint)
        .form(form)
        .send()
        .await
        .map_err(|error| AgentError::Auth(format!("token request failed: {error}")))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(AgentError::Auth(format!(
            "token endpoint returned HTTP {}: {body}",
            status.as_u16()
        )));
    }
    serde_json::from_str(&body)
        .map_err(|error| AgentError::Auth(format!("cannot parse token response: {error}")))
}

fn credential_from_token(
    server_name: &str,
    resource: &ProtectedResourceMetadata,
    authorization: &AuthorizationServerMetadata,
    registration: &RegistrationResponse,
    redirect_uri: &str,
    requested_scopes: &[String],
    token: TokenResponse,
) -> McpOAuthCredential {
    McpOAuthCredential {
        server_name: server_name.to_string(),
        resource: resource.resource.clone(),
        issuer: authorization.issuer.clone(),
        token_endpoint: authorization.token_endpoint.clone(),
        client_id: registration.client_id.clone(),
        client_secret: registration.client_secret.clone(),
        redirect_uri: redirect_uri.to_string(),
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        expires_at: token
            .expires_in
            .map(|seconds| now_unix().saturating_add(seconds)),
        scope: token
            .scope
            .or_else(|| Some(requested_scopes.join(" ")))
            .filter(|scope| !scope.is_empty()),
    }
}

fn ensure_https(url: &str, label: &str) -> Result<()> {
    if !url.starts_with("https://") {
        return Err(AgentError::Auth(format!(
            "{label} must use HTTPS, got `{url}`"
        )));
    }
    Ok(())
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(unix)]
fn open_private(path: &Path) -> Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| AgentError::Auth(format!("cannot create {}: {error}", path.display())))
}

#[cfg(not(unix))]
fn open_private(path: &Path) -> Result<std::fs::File> {
    std::fs::File::create(path)
        .map_err(|error| AgentError::Auth(format!("cannot create {}: {error}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credential() -> McpOAuthCredential {
        McpOAuthCredential {
            server_name: "linear".into(),
            resource: "https://mcp.linear.app/mcp".into(),
            issuer: "https://mcp.linear.app".into(),
            token_endpoint: "https://mcp.linear.app/token".into(),
            client_id: "client-1".into(),
            client_secret: None,
            redirect_uri: "http://localhost:1458/auth/callback".into(),
            access_token: "access-1".into(),
            refresh_token: Some("refresh-1".into()),
            expires_at: Some(now_unix() + 3600),
            scope: Some("read write".into()),
        }
    }

    #[test]
    fn extracts_resource_metadata_from_bearer_challenge() {
        let header = r#"Bearer realm="OAuth", resource_metadata="https://mcp.linear.app/.well-known/oauth-protected-resource/mcp", error="invalid_token""#;
        assert_eq!(
            challenge_parameter(header, "resource_metadata").as_deref(),
            Some("https://mcp.linear.app/.well-known/oauth-protected-resource/mcp")
        );
    }

    #[test]
    fn authorize_url_binds_resource_scope_pkce_and_state() {
        let pkce = pkce::Pkce {
            verifier: "verifier".into(),
            challenge: "challenge".into(),
        };
        let url = build_authorize_url(
            "https://auth.example/authorize",
            "client",
            "http://localhost:1234/auth/callback",
            "https://mcp.example/mcp",
            &["read".into(), "write".into()],
            &pkce,
            "state",
        );
        assert!(url.contains("resource=https%3A%2F%2Fmcp.example%2Fmcp"));
        assert!(url.contains("scope=read%20write"));
        assert!(url.contains("code_challenge=challenge"));
        assert!(url.contains("state=state"));
    }

    #[test]
    fn omitted_scopes_use_the_resources_supported_scopes() {
        let resource = ProtectedResourceMetadata {
            resource: "https://mcp.example/mcp".into(),
            authorization_servers: vec!["https://auth.example".into()],
            scopes_supported: vec!["read".into(), "write".into()],
        };
        let authorization = AuthorizationServerMetadata {
            issuer: "https://auth.example".into(),
            authorization_endpoint: "https://auth.example/authorize".into(),
            token_endpoint: "https://auth.example/token".into(),
            registration_endpoint: Some("https://auth.example/register".into()),
            scopes_supported: vec!["read".into(), "write".into()],
            code_challenge_methods_supported: vec!["S256".into()],
        };
        assert_eq!(
            select_scopes(&[], &resource, &authorization).unwrap(),
            vec!["read", "write"]
        );
    }

    #[test]
    fn credential_store_round_trips_and_clears_one_server() {
        let dir = tempfile::tempdir().unwrap();
        let store = McpOAuthStore::at(dir.path());
        store.save(&credential()).unwrap();
        assert_eq!(store.load("linear").unwrap(), Some(credential()));
        assert!(store.clear("linear").unwrap());
        assert_eq!(store.load("linear").unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn credential_store_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let store = McpOAuthStore::at(dir.path());
        store.save(&credential()).unwrap();
        let mode = std::fs::metadata(store.path_for("linear"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn expired_access_token_is_not_fresh() {
        let mut value = credential();
        value.expires_at = Some(now_unix());
        assert!(!value.access_token_is_fresh());
    }
}
