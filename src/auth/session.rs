//! Live credentials for the request path.
//!
//! [`ChatGptAuth::credentials`] is what a provider calls before every
//! request. It hands back a bearer token that is valid *now*,
//! refreshing first if the stored one is close to expiry. Callers see
//! none of that — no refresh method to remember, no expiry to check.
//!
//! Refresh cannot live behind `Config::resolve_api_key`, which is
//! synchronous and runs once at provider construction: access tokens
//! are short-lived, so a session that outlives one would 401 in the
//! middle of a turn. It has to be per-request and async, which is why
//! this type exists at all.
//!
//! # Failure policy
//!
//! - Not signed in → error naming `oli login` *and* API-key auth.
//! - Refresh fails permanently (expired/revoked) → error naming both,
//!   and the dead bundle is left on disk for inspection rather than
//!   silently deleted.
//! - Refresh fails transiently while the current token is still
//!   valid → warn and carry on. We refresh five minutes early
//!   precisely so a blip in that window is survivable.
//! - Refresh fails transiently and the token has already expired →
//!   error, because there is nothing left to send.

use std::sync::Arc;
use tokio::sync::Mutex;

use crate::auth::oauth::{RefreshError, refresh_tokens};
use crate::auth::store::AuthStore;
use crate::auth::token::{Tokens, now_unix};
use crate::auth::{ISSUER, client_id};
use crate::error::{AgentError, Result};

/// What a request needs in order to authenticate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Credentials {
    /// Value for `Authorization: Bearer …`.
    pub bearer: String,
    /// Value for `ChatGPT-Account-ID`, when the account has one.
    /// Workspace accounts are rejected without it.
    pub account_id: Option<String>,
}

/// Holds the token bundle for a session and keeps it fresh.
///
/// Cloneable and shareable: the inner state is behind an `Arc<Mutex>`
/// so concurrent requests (the agent loop runs tool calls in parallel)
/// serialise on refresh rather than each firing their own.
#[derive(Clone, Debug)]
pub struct ChatGptAuth {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    store: AuthStore,
    issuer: String,
    client_id: String,
    http: reqwest::Client,
    /// `None` until first use — construction stays sync and I/O-free
    /// so `providers::build()` doesn't need to be async.
    cached: Mutex<Option<Tokens>>,
}

impl ChatGptAuth {
    /// Auth against the real issuer, reading the default store path.
    pub fn new() -> Result<Self> {
        Ok(Self::with_store(AuthStore::default_location()?))
    }

    /// Auth against the real issuer with an explicit store.
    pub fn with_store(store: AuthStore) -> Self {
        Self::with_parts(store, ISSUER.to_string(), client_id())
    }

    /// Full control — the test seam.
    pub fn with_parts(store: AuthStore, issuer: String, client_id: String) -> Self {
        Self {
            inner: Arc::new(Inner {
                store,
                issuer,
                client_id,
                http: reqwest::Client::new(),
                cached: Mutex::new(None),
            }),
        }
    }

    /// Credentials valid at this moment, refreshing first if needed.
    pub async fn credentials(&self) -> Result<Credentials> {
        self.credentials_at(now_unix()).await
    }

    /// [`Self::credentials`] with an injected clock, so expiry
    /// behaviour is testable without waiting an hour.
    pub async fn credentials_at(&self, now: u64) -> Result<Credentials> {
        let mut guard = self.inner.cached.lock().await;

        if guard.is_none() {
            *guard = Some(self.load_or_explain()?);
        }
        // Unwrap is sound: just populated above.
        let tokens = guard.as_ref().expect("tokens populated");

        if tokens.needs_refresh(now) {
            let refreshed = self.refresh(tokens, now).await?;
            *guard = Some(refreshed);
        }

        let tokens = guard.as_ref().expect("tokens populated");
        Ok(Credentials {
            bearer: tokens.access_token.clone(),
            account_id: tokens.account_id(),
        })
    }

    /// Force a refresh regardless of expiry. For the 401 path: the
    /// server is the authority on validity, so a rejected request gets
    /// one retry with a freshly minted token.
    pub async fn force_refresh(&self) -> Result<Credentials> {
        let now = now_unix();
        let mut guard = self.inner.cached.lock().await;
        let current = match guard.take() {
            Some(t) => t,
            None => self.load_or_explain()?,
        };
        // Ignore the current token's remaining life: it was rejected.
        let refreshed = self
            .refresh_or_fail(&current)
            .await
            .map_err(|e| self.explain_refresh_failure(e, &current, now, true))?;
        *guard = Some(refreshed);
        let tokens = guard.as_ref().expect("tokens populated");
        Ok(Credentials {
            bearer: tokens.access_token.clone(),
            account_id: tokens.account_id(),
        })
    }

    /// The path to the credential file, for error messages.
    pub fn store_path(&self) -> &std::path::Path {
        self.inner.store.path()
    }

    fn load_or_explain(&self) -> Result<Tokens> {
        match self.inner.store.load()? {
            Some(t) => Ok(t),
            None => Err(AgentError::Auth(format!(
                "not signed in to ChatGPT (no credentials at {}). \
                 Run `oli login` (or `oli login --device-auth` over SSH), \
                 or switch this provider to API-key auth with \
                 `kind = \"openai-compat\"` and `api_key_env = \"OPENAI_API_KEY\"`.",
                self.inner.store.path().display()
            ))),
        }
    }

    /// Refresh and persist, applying the survivability policy in the
    /// module docs.
    async fn refresh(&self, current: &Tokens, now: u64) -> Result<Tokens> {
        match self.refresh_or_fail(current).await {
            Ok(refreshed) => Ok(refreshed),
            Err(e) => {
                // Survivable only if what we already hold still works.
                let still_valid = current.expires_at().is_some_and(|exp| exp > now);
                if !e.is_permanent() && still_valid {
                    crate::log_warn!(
                        "[auth] ChatGPT token refresh failed ({e}); \
                         continuing with the current token until it expires"
                    );
                    return Ok(current.clone());
                }
                Err(self.explain_refresh_failure(e, current, now, false))
            }
        }
    }

    /// The refresh call itself, plus persistence on success.
    async fn refresh_or_fail(&self, current: &Tokens) -> std::result::Result<Tokens, RefreshError> {
        if !current.can_refresh() {
            return Err(RefreshError::Permanent(
                "no refresh token was stored".to_string(),
            ));
        }
        let refreshed = refresh_tokens(
            &self.inner.http,
            &self.inner.issuer,
            &self.inner.client_id,
            current,
        )
        .await?;

        // A write failure here is not fatal to this session — the
        // in-memory token still works — but it means the next process
        // start will re-refresh, so it is worth surfacing.
        if let Err(e) = self.inner.store.save(&refreshed) {
            crate::log_warn!("[auth] refreshed token could not be saved: {e}");
        }
        Ok(refreshed)
    }

    /// Turn a refresh failure into a user-facing error. Always names
    /// both remedies, because this is the message that shows up on the
    /// day OpenAI stops honouring third-party refreshes.
    fn explain_refresh_failure(
        &self,
        e: RefreshError,
        current: &Tokens,
        now: u64,
        forced: bool,
    ) -> AgentError {
        let expired = current.expires_at().is_none_or(|exp| exp <= now);
        let situation = if forced {
            "OpenAI rejected the ChatGPT credentials and refreshing them failed"
        } else if expired {
            "the stored ChatGPT access token has expired and could not be refreshed"
        } else {
            "the stored ChatGPT credentials could not be refreshed"
        };
        let next = if e.is_permanent() {
            "Run `oli login` to sign in again"
        } else {
            "This may be temporary — retry shortly, or run `oli login` to sign in again"
        };
        AgentError::Auth(format!(
            "{situation}: {e}. {next}. Subscription auth is not a documented OpenAI \
             feature and can be withdrawn without notice, so if it keeps failing, switch \
             this provider to API-key auth (`kind = \"openai-compat\"`, \
             `base_url = \"https://api.openai.com/v1\"`, \
             `api_key_env = \"OPENAI_API_KEY\"`). Credentials: {}",
            self.inner.store.path().display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Access token whose `exp` claim is `exp`.
    fn access_token(exp: u64) -> String {
        format!(
            "h.{}.s",
            URL_SAFE_NO_PAD.encode(serde_json::json!({ "exp": exp }).to_string())
        )
    }

    fn tokens(exp: u64) -> Tokens {
        Tokens {
            access_token: access_token(exp),
            refresh_token: "rt-1".into(),
            id_token: format!(
                "h.{}.s",
                URL_SAFE_NO_PAD.encode(
                    serde_json::json!({
                        "https://api.openai.com/auth": { "chatgpt_account_id": "acct-5" }
                    })
                    .to_string()
                )
            ),
            account_id: Some("acct-5".into()),
            last_refresh: Some(0),
        }
    }

    /// Serve a fixed response to every connection until dropped.
    async fn stub(status: u16, body: String) -> String {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let body = body.clone();
                let mut buf = vec![0u8; 4096];
                let _ = stream.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.flush().await;
            }
        });
        format!("http://127.0.0.1:{port}")
    }

    fn auth_with(store: AuthStore, issuer: String) -> ChatGptAuth {
        ChatGptAuth::with_parts(store, issuer, "client-1".into())
    }

    fn store_with(dir: &tempfile::TempDir, tokens: &Tokens) -> AuthStore {
        let store = AuthStore::at(dir.path().join("auth.json"));
        store.save(tokens).unwrap();
        store
    }

    #[tokio::test]
    async fn fresh_token_is_returned_without_touching_the_network() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with(&dir, &tokens(10_000));
        // Unroutable issuer: any refresh attempt would fail loudly.
        let auth = auth_with(store, "http://127.0.0.1:1".into());

        let creds = auth.credentials_at(1_000).await.unwrap();
        assert_eq!(creds.bearer, access_token(10_000));
        assert_eq!(creds.account_id.as_deref(), Some("acct-5"));
    }

    #[tokio::test]
    async fn expired_token_is_refreshed_before_the_caller_sees_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with(&dir, &tokens(1_000));
        let new_access = access_token(99_000);
        let issuer = stub(
            200,
            serde_json::json!({ "access_token": new_access }).to_string(),
        )
        .await;
        let auth = auth_with(store.clone(), issuer);

        // now = 5_000, well past the stored token's 1_000 expiry.
        let creds = auth.credentials_at(5_000).await.unwrap();

        assert_eq!(creds.bearer, new_access);
        // And it was persisted, so the next process start is fast.
        assert_eq!(store.load().unwrap().unwrap().access_token, new_access);
    }

    #[tokio::test]
    async fn token_inside_the_refresh_window_is_refreshed_early() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with(&dir, &tokens(10_000));
        let new_access = access_token(99_000);
        let issuer = stub(
            200,
            serde_json::json!({ "access_token": new_access }).to_string(),
        )
        .await;
        let auth = auth_with(store, issuer);

        // 200s before expiry: inside the 5-minute window.
        let creds = auth.credentials_at(9_800).await.unwrap();
        assert_eq!(creds.bearer, new_access);
    }

    #[tokio::test]
    async fn transient_refresh_failure_keeps_a_still_valid_token() {
        // The reason we refresh five minutes early: a blip in that
        // window must not end the session.
        let dir = tempfile::tempdir().unwrap();
        let store = store_with(&dir, &tokens(10_000));
        let issuer = stub(503, "{}".into()).await;
        let auth = auth_with(store, issuer);

        let creds = auth.credentials_at(9_800).await.unwrap();
        assert_eq!(creds.bearer, access_token(10_000));
    }

    #[tokio::test]
    async fn transient_refresh_failure_on_an_expired_token_is_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with(&dir, &tokens(1_000));
        let issuer = stub(503, "{}".into()).await;
        let auth = auth_with(store, issuer);

        let err = auth.credentials_at(5_000).await.unwrap_err().to_string();
        assert!(err.contains("expired"), "{err}");
        assert!(err.contains("may be temporary"), "{err}");
        assert!(err.contains("OPENAI_API_KEY"), "{err}");
    }

    #[tokio::test]
    async fn permanent_refresh_failure_says_sign_in_again() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with(&dir, &tokens(10_000));
        let issuer = stub(
            400,
            r#"{"error":"refresh_token_expired","error_description":"too old"}"#.into(),
        )
        .await;
        let auth = auth_with(store.clone(), issuer);

        let err = auth.credentials_at(9_800).await.unwrap_err().to_string();
        assert!(err.contains("oli login"), "{err}");
        assert!(err.contains("API-key auth"), "{err}");
        // The dead bundle is left in place rather than deleted, so the
        // user can see what happened.
        assert!(store.exists());
    }

    #[tokio::test]
    async fn a_permanent_failure_beats_a_still_valid_token() {
        // Revoked means revoked — continuing on an unexpired token
        // would just 401 on the next request with a worse message.
        let dir = tempfile::tempdir().unwrap();
        let store = store_with(&dir, &tokens(10_000));
        let issuer = stub(401, "{}".into()).await;
        let auth = auth_with(store, issuer);

        assert!(auth.credentials_at(9_800).await.is_err());
    }

    #[tokio::test]
    async fn not_signed_in_names_both_login_and_the_api_key_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let auth = auth_with(
            AuthStore::at(dir.path().join("auth.json")),
            "http://127.0.0.1:1".into(),
        );

        let err = auth.credentials().await.unwrap_err().to_string();
        assert!(err.contains("not signed in"), "{err}");
        assert!(err.contains("oli login"), "{err}");
        assert!(err.contains("--device-auth"), "{err}");
        assert!(err.contains("OPENAI_API_KEY"), "{err}");
    }

    #[tokio::test]
    async fn a_bundle_without_a_refresh_token_fails_permanently() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = tokens(1_000);
        t.refresh_token = String::new();
        let store = store_with(&dir, &t);
        let auth = auth_with(store, "http://127.0.0.1:1".into());

        let err = auth.credentials_at(5_000).await.unwrap_err().to_string();
        assert!(err.contains("no refresh token"), "{err}");
        assert!(err.contains("oli login"), "{err}");
    }

    #[tokio::test]
    async fn concurrent_callers_share_one_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with(&dir, &tokens(1_000));
        let new_access = access_token(99_000);
        let issuer = stub(
            200,
            serde_json::json!({ "access_token": new_access }).to_string(),
        )
        .await;
        let auth = auth_with(store, issuer);

        let results = futures::future::join_all((0..8).map(|_| {
            let auth = auth.clone();
            async move { auth.credentials_at(5_000).await }
        }))
        .await;

        for r in results {
            assert_eq!(r.unwrap().bearer, new_access);
        }
    }

    #[tokio::test]
    async fn cached_tokens_survive_the_file_being_removed() {
        // Once loaded, a session keeps working even if auth.json is
        // deleted underneath it — no surprise mid-turn failure.
        let dir = tempfile::tempdir().unwrap();
        let store = store_with(&dir, &tokens(10_000));
        let auth = auth_with(store.clone(), "http://127.0.0.1:1".into());

        auth.credentials_at(1_000).await.unwrap();
        store.clear().unwrap();

        assert!(auth.credentials_at(1_000).await.is_ok());
    }

    #[tokio::test]
    async fn force_refresh_replaces_a_token_that_had_not_expired() {
        // The 401 path: the server rejected a token we thought was
        // fine, so its remaining lifetime is not evidence of anything.
        let dir = tempfile::tempdir().unwrap();
        let store = store_with(&dir, &tokens(10_000));
        let new_access = access_token(99_000);
        let issuer = stub(
            200,
            serde_json::json!({ "access_token": new_access }).to_string(),
        )
        .await;
        let auth = auth_with(store, issuer);

        auth.credentials_at(1_000).await.unwrap();
        let creds = auth.force_refresh().await.unwrap();
        assert_eq!(creds.bearer, new_access);
    }

    #[tokio::test]
    async fn force_refresh_failure_explains_the_rejection() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with(&dir, &tokens(10_000));
        let issuer = stub(401, "{}".into()).await;
        let auth = auth_with(store, issuer);

        let err = auth.force_refresh().await.unwrap_err().to_string();
        assert!(err.contains("rejected"), "{err}");
        assert!(err.contains("oli login"), "{err}");
        assert!(err.contains("API-key auth"), "{err}");
    }
}
