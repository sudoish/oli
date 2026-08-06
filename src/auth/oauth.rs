//! OAuth 2.0 authorization-code + PKCE against `auth.openai.com`.
//!
//! URL building is pure and unit-tested; the two network calls take an
//! explicit `issuer` so tests can point them at a local stub server
//! instead of the real one.
//!
//! A note on encodings, because they are not consistent: the
//! authorization-code exchange posts
//! `application/x-www-form-urlencoded`, while the refresh grant posts
//! JSON. That asymmetry is the server's, not ours.

use serde::Deserialize;

use crate::auth::pkce::Pkce;
use crate::auth::token::{Tokens, now_unix};
use crate::auth::{SCOPES, form_encode};
use crate::error::{AgentError, Result};

/// Build the authorize URL the user's browser is sent to.
///
/// `id_token_add_organizations` is what makes the returned id_token
/// carry `chatgpt_account_id`; without it a workspace account cannot
/// be routed and every request 401s.
pub fn authorize_url(
    issuer: &str,
    client_id: &str,
    redirect_uri: &str,
    pkce: &Pkce,
    state: &str,
) -> String {
    let params = [
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
        ("scope", SCOPES),
        ("code_challenge", &pkce.challenge),
        ("code_challenge_method", "S256"),
        ("id_token_add_organizations", "true"),
        ("state", state),
    ];
    let query = params
        .iter()
        .map(|(k, v)| format!("{k}={}", form_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{}/oauth/authorize?{query}", issuer.trim_end_matches('/'))
}

/// Token-endpoint response. `refresh_token` is optional because the
/// refresh grant may legitimately omit it (meaning "keep the one you
/// have"); the code grant always returns one when `offline_access` was
/// granted.
#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub id_token: Option<String>,
}

impl TokenResponse {
    /// Fold a token-endpoint response into a storable bundle,
    /// carrying forward any `previous` values the response omitted.
    pub fn into_tokens(self, previous: Option<&Tokens>) -> Tokens {
        let id_token = self
            .id_token
            .or_else(|| previous.map(|p| p.id_token.clone()))
            .unwrap_or_default();
        let refresh_token = self
            .refresh_token
            .or_else(|| previous.map(|p| p.refresh_token.clone()))
            .unwrap_or_default();
        let mut tokens = Tokens {
            access_token: self.access_token,
            refresh_token,
            id_token,
            account_id: None,
            last_refresh: Some(now_unix()),
        };
        // Cache the account id so the request path doesn't re-parse
        // the id_token on every call.
        tokens.account_id = tokens.account_id();
        tokens
    }
}

/// Exchange an authorization code for tokens.
pub async fn exchange_code(
    http: &reqwest::Client,
    issuer: &str,
    client_id: &str,
    redirect_uri: &str,
    pkce: &Pkce,
    code: &str,
) -> Result<Tokens> {
    let body = format!(
        "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&code_verifier={}",
        form_encode(code),
        form_encode(redirect_uri),
        form_encode(client_id),
        form_encode(&pkce.verifier),
    );
    let resp = post_token_endpoint(http, issuer, body).await?;
    Ok(resp.into_tokens(None))
}

/// POST to `{issuer}/oauth/token` with a form-encoded body and decode
/// the response, turning any non-2xx into a loud `AgentError::Auth`.
async fn post_token_endpoint(
    http: &reqwest::Client,
    issuer: &str,
    body: String,
) -> Result<TokenResponse> {
    let endpoint = format!("{}/oauth/token", issuer.trim_end_matches('/'));
    let resp = http
        .post(&endpoint)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .map_err(|e| {
            AgentError::Auth(format!(
                "could not reach the OpenAI token endpoint ({endpoint}): {e}. \
                 Check your network, or configure an API-key provider instead."
            ))
        })?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(AgentError::Auth(format!(
            "ChatGPT sign-in was rejected by {endpoint} (HTTP {}): {}. \
             Subscription auth is not a documented OpenAI feature and can be \
             withdrawn without notice — if this keeps happening, switch the \
             provider to API-key auth (`api_key_env = \"OPENAI_API_KEY\"`).",
            status.as_u16(),
            describe_token_error(&body),
        )));
    }
    serde_json::from_str(&body).map_err(|e| {
        AgentError::Auth(format!(
            "the OpenAI token endpoint returned a response Oli could not parse ({e}). \
             Use API-key auth if this persists."
        ))
    })
}

/// Extract a human-readable message from a token-endpoint error body.
///
/// The endpoint returns `{"error": "...", "error_description": "..."}`
/// on a good day and an HTML error page on a bad one, so this always
/// produces *something* rather than swallowing the body.
pub fn describe_token_error(body: &str) -> String {
    #[derive(Deserialize)]
    struct ErrorBody {
        #[serde(default)]
        error: Option<String>,
        #[serde(default)]
        error_description: Option<String>,
    }
    if let Ok(parsed) = serde_json::from_str::<ErrorBody>(body) {
        match (parsed.error, parsed.error_description) {
            (Some(code), Some(desc)) => return format!("{desc} [{code}]"),
            (Some(code), None) => return code,
            (None, Some(desc)) => return desc,
            (None, None) => {}
        }
    }
    let trimmed = body.trim();
    if trimmed.is_empty() {
        "no response body".to_string()
    } else {
        // Cap it — an HTML error page should not fill the terminal.
        trimmed.chars().take(300).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{CALLBACK_PORT, redirect_uri};

    fn pkce() -> Pkce {
        Pkce {
            verifier: "verifier-abc".into(),
            challenge: "challenge-xyz".into(),
        }
    }

    #[test]
    fn authorize_url_carries_the_pkce_challenge_and_method() {
        let url = authorize_url(
            "https://auth.openai.com",
            "app_1",
            &redirect_uri(CALLBACK_PORT),
            &pkce(),
            "state-1",
        );
        assert!(url.starts_with("https://auth.openai.com/oauth/authorize?"));
        assert!(url.contains("code_challenge=challenge-xyz"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("state=state-1"));
    }

    #[test]
    fn authorize_url_requests_organizations_in_the_id_token() {
        // Without this the id_token has no chatgpt_account_id and
        // workspace accounts cannot be routed.
        let url = authorize_url("https://i", "c", "http://r", &pkce(), "s");
        assert!(url.contains("id_token_add_organizations=true"));
    }

    #[test]
    fn authorize_url_requests_offline_access() {
        // No offline_access, no refresh token, browser login every hour.
        let url = authorize_url("https://i", "c", "http://r", &pkce(), "s");
        assert!(url.contains("offline_access"));
    }

    #[test]
    fn authorize_url_percent_encodes_the_redirect_uri() {
        let url = authorize_url("https://i", "c", &redirect_uri(CALLBACK_PORT), &pkce(), "s");
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback"));
    }

    #[test]
    fn authorize_url_tolerates_a_trailing_slash_on_the_issuer() {
        let url = authorize_url("https://auth.openai.com/", "c", "http://r", &pkce(), "s");
        assert!(url.starts_with("https://auth.openai.com/oauth/authorize?"));
    }

    #[test]
    fn token_error_prefers_description_with_code() {
        let msg =
            describe_token_error(r#"{"error":"invalid_grant","error_description":"code expired"}"#);
        assert_eq!(msg, "code expired [invalid_grant]");
    }

    #[test]
    fn token_error_falls_back_to_code_alone() {
        assert_eq!(
            describe_token_error(r#"{"error":"unauthorized_client"}"#),
            "unauthorized_client"
        );
    }

    #[test]
    fn token_error_passes_through_non_json_bodies() {
        let msg = describe_token_error("<html>502 Bad Gateway</html>");
        assert!(msg.contains("502 Bad Gateway"));
    }

    #[test]
    fn token_error_caps_very_long_bodies() {
        let msg = describe_token_error(&"x".repeat(10_000));
        assert_eq!(msg.chars().count(), 300);
    }

    #[test]
    fn token_error_describes_an_empty_body() {
        assert_eq!(describe_token_error("   "), "no response body");
    }

    #[test]
    fn response_folds_into_a_bundle_with_a_refresh_stamp() {
        let resp = TokenResponse {
            access_token: "a".into(),
            refresh_token: Some("r".into()),
            id_token: Some("h.e30.s".into()),
        };
        let tokens = resp.into_tokens(None);
        assert_eq!(tokens.access_token, "a");
        assert_eq!(tokens.refresh_token, "r");
        assert!(tokens.last_refresh.is_some());
    }

    #[test]
    fn response_carries_forward_omitted_fields() {
        // A refresh grant may return only a new access token; dropping
        // the old refresh token there would silently log the user out
        // at the next expiry.
        let previous = Tokens {
            access_token: "old".into(),
            refresh_token: "keep-me".into(),
            id_token: "h.e30.s".into(),
            account_id: Some("acct".into()),
            last_refresh: Some(1),
        };
        let resp = TokenResponse {
            access_token: "new".into(),
            refresh_token: None,
            id_token: None,
        };
        let tokens = resp.into_tokens(Some(&previous));
        assert_eq!(tokens.access_token, "new");
        assert_eq!(tokens.refresh_token, "keep-me");
        assert_eq!(tokens.id_token, "h.e30.s");
    }

    #[test]
    fn response_caches_the_account_id_from_the_id_token() {
        use base64::Engine;
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let payload = URL_SAFE_NO_PAD.encode(
            serde_json::json!({
                "https://api.openai.com/auth": { "chatgpt_account_id": "acct-99" }
            })
            .to_string(),
        );
        let resp = TokenResponse {
            access_token: "a".into(),
            refresh_token: Some("r".into()),
            id_token: Some(format!("h.{payload}.s")),
        };
        assert_eq!(
            resp.into_tokens(None).account_id.as_deref(),
            Some("acct-99")
        );
    }

    // ---- Token-endpoint round trips against a local stub ----------
    //
    // reqwest talks plain HTTP to 127.0.0.1 here, so these exercise
    // the real request-building and error-handling paths without
    // touching the network.

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Serve exactly one HTTP response, and hand back what the client
    /// sent so the request body can be asserted on.
    async fn stub_once(
        status: u16,
        body: &'static str,
    ) -> (String, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = stream.read(&mut buf).await.unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).into_owned();
            let response = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            stream.flush().await.unwrap();
            request
        });
        (format!("http://127.0.0.1:{port}"), handle)
    }

    #[tokio::test]
    async fn exchange_posts_the_verifier_and_returns_tokens() {
        let (issuer, served) = stub_once(
            200,
            r#"{"access_token":"at","refresh_token":"rt","id_token":"h.e30.s"}"#,
        )
        .await;

        let tokens = exchange_code(
            &reqwest::Client::new(),
            &issuer,
            "client-1",
            "http://localhost:1455/auth/callback",
            &pkce(),
            "the-code",
        )
        .await
        .unwrap();

        assert_eq!(tokens.access_token, "at");
        assert_eq!(tokens.refresh_token, "rt");

        let request = served.await.unwrap();
        assert!(request.starts_with("POST /oauth/token "), "{request}");
        assert!(
            request.contains("grant_type=authorization_code"),
            "{request}"
        );
        assert!(request.contains("code_verifier=verifier-abc"), "{request}");
        assert!(request.contains("code=the-code"), "{request}");
        assert!(
            request.contains("application/x-www-form-urlencoded"),
            "{request}"
        );
    }

    #[tokio::test]
    async fn rejection_names_the_reason_and_the_api_key_fallback() {
        let (issuer, _served) = stub_once(
            403,
            r#"{"error":"access_denied","error_description":"client not permitted"}"#,
        )
        .await;

        let err = exchange_code(
            &reqwest::Client::new(),
            &issuer,
            "client-1",
            "http://localhost:1455/auth/callback",
            &pkce(),
            "the-code",
        )
        .await
        .unwrap_err()
        .to_string();

        // This is the message a user sees on the day OpenAI stops
        // accepting third-party clients. It has to say what happened
        // and what to do instead.
        assert!(err.contains("403"), "{err}");
        assert!(err.contains("client not permitted"), "{err}");
        assert!(err.contains("access_denied"), "{err}");
        assert!(err.contains("API-key auth"), "{err}");
        assert!(err.contains("OPENAI_API_KEY"), "{err}");
    }

    #[tokio::test]
    async fn unparseable_success_body_is_an_error_not_a_panic() {
        let (issuer, _served) = stub_once(200, "not json at all").await;

        let err = exchange_code(
            &reqwest::Client::new(),
            &issuer,
            "c",
            "http://r",
            &pkce(),
            "code",
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(err.contains("could not parse"), "{err}");
        assert!(err.contains("API-key auth"), "{err}");
    }

    #[tokio::test]
    async fn unreachable_issuer_names_the_endpoint_and_the_fallback() {
        // Bind and drop, so the port is almost certainly closed.
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let err = exchange_code(
            &reqwest::Client::new(),
            &format!("http://127.0.0.1:{port}"),
            "c",
            "http://r",
            &pkce(),
            "code",
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(err.contains("could not reach"), "{err}");
        assert!(err.contains("API-key"), "{err}");
    }
}
