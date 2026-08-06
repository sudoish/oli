//! The persisted token bundle and JWT claim decoding.
//!
//! Pure: parsing takes `&str`, expiry checks take an explicit `now`.
//! Nothing here touches the filesystem, the network, or the clock
//! except via [`now_unix`], which callers can bypass in tests.
//!
//! # On not verifying signatures
//!
//! The JWT payloads are decoded but **not** verified. That is
//! deliberate and matches every client in this space: the tokens
//! arrive over TLS from the issuer's own token endpoint, and we read
//! them only for routing metadata (`chatgpt_account_id`) and expiry
//! scheduling. The authority on whether a token is valid is the API
//! that rejects it, not us. We never make a trust decision on a claim.

use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Refresh an access token this long before it actually expires, so a
/// request never races the boundary.
pub const REFRESH_WINDOW: Duration = Duration::from_secs(5 * 60);

/// Refresh anyway if the bundle hasn't been refreshed in this long,
/// even when the access token still looks valid. Keeps the refresh
/// token itself from ageing out on an idle install.
pub const MAX_REFRESH_AGE: Duration = Duration::from_secs(8 * 24 * 60 * 60);

/// Namespaced claim under which OpenAI puts subscription metadata.
/// The `#[serde(rename)]` attributes below need string literals, so
/// these exist to document the wire format (and to keep the test
/// fixtures honest) rather than to be substituted in.
pub const AUTH_CLAIM_NS: &str = "https://api.openai.com/auth";

/// Namespaced claim carrying the user profile.
pub const PROFILE_CLAIM_NS: &str = "https://api.openai.com/profile";

#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    #[error("malformed JWT: expected three dot-separated segments")]
    Malformed,

    #[error("JWT payload is not valid base64url: {0}")]
    Base64(#[from] base64::DecodeError),

    #[error("JWT payload is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// The full credential bundle, as persisted to `auth.json`.
///
/// `id_token` is kept in its raw encoded form rather than as parsed
/// claims: it is the input to the token-exchange grant, and re-encoding
/// parsed claims would not round-trip.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    /// Short-lived bearer sent as `Authorization: Bearer …`.
    pub access_token: String,

    /// Long-lived credential used to mint new access tokens. Empty
    /// when the issuer declined to return one (which means the user
    /// must re-run `oli login` when the access token expires).
    #[serde(default)]
    pub refresh_token: String,

    /// Raw id_token JWT. Carries `chatgpt_plan_type` and
    /// `chatgpt_account_id`.
    #[serde(default)]
    pub id_token: String,

    /// Workspace/account id sent as the `ChatGPT-Account-ID` header.
    /// Cached here so the happy path doesn't re-parse the id_token on
    /// every request; [`Tokens::account_id`] falls back to the claim.
    #[serde(default)]
    pub account_id: Option<String>,

    /// Unix seconds at which this bundle was last refreshed.
    #[serde(default)]
    pub last_refresh: Option<u64>,
}

/// Subscription metadata read out of the id_token.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IdClaims {
    pub email: Option<String>,
    /// e.g. `"plus"`, `"pro"`, `"business"`. Kept as a free string:
    /// the set of values is not documented and an unrecognised plan
    /// must not break login.
    pub chatgpt_plan_type: Option<String>,
    pub chatgpt_account_id: Option<String>,
    pub chatgpt_user_id: Option<String>,
}

impl Tokens {
    /// The account id to send as `ChatGPT-Account-ID`, preferring the
    /// cached field and falling back to the id_token claim.
    pub fn account_id(&self) -> Option<String> {
        if let Some(id) = &self.account_id {
            return Some(id.clone());
        }
        parse_id_claims(&self.id_token)
            .ok()
            .and_then(|c| c.chatgpt_account_id)
    }

    /// Unix-seconds expiry of the access token, or `None` when the
    /// token carries no readable `exp`.
    pub fn expires_at(&self) -> Option<u64> {
        jwt_expiry(&self.access_token).ok().flatten()
    }

    /// Whether the access token should be refreshed before use at
    /// `now` (unix seconds).
    ///
    /// True when the token expires within [`REFRESH_WINDOW`], when its
    /// `exp` is unreadable (fail toward refreshing rather than toward
    /// a mid-request 401), or when the bundle is older than
    /// [`MAX_REFRESH_AGE`].
    pub fn needs_refresh(&self, now: u64) -> bool {
        match self.expires_at() {
            Some(exp) => {
                if exp.saturating_sub(now) <= REFRESH_WINDOW.as_secs() {
                    return true;
                }
            }
            None => return true,
        }
        match self.last_refresh {
            Some(last) => now.saturating_sub(last) >= MAX_REFRESH_AGE.as_secs(),
            None => false,
        }
    }

    /// Whether a refresh is even possible. A bundle with no refresh
    /// token can only be replaced by a fresh `oli login`.
    pub fn can_refresh(&self) -> bool {
        !self.refresh_token.trim().is_empty()
    }

    /// Parsed id_token claims, or the default when absent/unreadable.
    pub fn claims(&self) -> IdClaims {
        parse_id_claims(&self.id_token).unwrap_or_default()
    }
}

/// Decode a JWT's payload segment as `T`. Signature is ignored — see
/// the module docs.
fn decode_payload<T: serde::de::DeserializeOwned>(jwt: &str) -> Result<T, TokenError> {
    let mut parts = jwt.split('.');
    let payload = match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(h), Some(p), Some(s), None) if !h.is_empty() && !p.is_empty() && !s.is_empty() => p,
        _ => return Err(TokenError::Malformed),
    };
    let bytes = URL_SAFE_NO_PAD.decode(payload)?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Read the `exp` claim as unix seconds. `Ok(None)` means the JWT
/// parsed but carried no `exp`.
pub fn jwt_expiry(jwt: &str) -> Result<Option<u64>, TokenError> {
    #[derive(Deserialize)]
    struct Exp {
        #[serde(default)]
        exp: Option<i64>,
    }
    let claims: Exp = decode_payload(jwt)?;
    Ok(claims.exp.filter(|e| *e >= 0).map(|e| e as u64))
}

/// Read the subscription claims out of an id_token.
pub fn parse_id_claims(jwt: &str) -> Result<IdClaims, TokenError> {
    #[derive(Deserialize)]
    struct Raw {
        #[serde(default)]
        email: Option<String>,
        #[serde(rename = "https://api.openai.com/profile", default)]
        profile: Option<Profile>,
        #[serde(rename = "https://api.openai.com/auth", default)]
        auth: Option<Auth>,
    }
    #[derive(Deserialize)]
    struct Profile {
        #[serde(default)]
        email: Option<String>,
    }
    #[derive(Deserialize)]
    struct Auth {
        #[serde(default)]
        chatgpt_plan_type: Option<String>,
        #[serde(default)]
        chatgpt_account_id: Option<String>,
        #[serde(default)]
        chatgpt_user_id: Option<String>,
        #[serde(default)]
        user_id: Option<String>,
    }

    let raw: Raw = decode_payload(jwt)?;
    let email = raw.email.or_else(|| raw.profile.and_then(|p| p.email));
    Ok(match raw.auth {
        Some(a) => IdClaims {
            email,
            chatgpt_plan_type: a.chatgpt_plan_type,
            chatgpt_account_id: a.chatgpt_account_id,
            chatgpt_user_id: a.chatgpt_user_id.or(a.user_id),
        },
        None => IdClaims {
            email,
            ..Default::default()
        },
    })
}

/// Wall clock in unix seconds. The one impure function in this module;
/// every decision function takes `now` as a parameter so tests never
/// call it.
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Build a JWT-shaped string with `payload` as its middle segment.
    /// Header and signature are placeholders — nothing reads them.
    fn jwt(payload: serde_json::Value) -> String {
        format!(
            "eyJhbGciOiJub25lIn0.{}.sig",
            URL_SAFE_NO_PAD.encode(payload.to_string())
        )
    }

    fn id_token_with(plan: &str, account: &str) -> String {
        jwt(json!({
            "email": "user@example.com",
            AUTH_CLAIM_NS: {
                "chatgpt_plan_type": plan,
                "chatgpt_account_id": account,
                "chatgpt_user_id": "user-1",
            }
        }))
    }

    #[test]
    fn parses_plan_type_and_account_id() {
        let claims = parse_id_claims(&id_token_with("pro", "acct-42")).unwrap();
        assert_eq!(claims.chatgpt_plan_type.as_deref(), Some("pro"));
        assert_eq!(claims.chatgpt_account_id.as_deref(), Some("acct-42"));
        assert_eq!(claims.chatgpt_user_id.as_deref(), Some("user-1"));
        assert_eq!(claims.email.as_deref(), Some("user@example.com"));
    }

    #[test]
    fn unknown_plan_type_is_preserved_not_rejected() {
        // The set of plan values is undocumented; a new one must not
        // break login.
        let claims = parse_id_claims(&id_token_with("enterprise-plus-ultra", "a")).unwrap();
        assert_eq!(
            claims.chatgpt_plan_type.as_deref(),
            Some("enterprise-plus-ultra")
        );
    }

    #[test]
    fn falls_back_to_profile_email() {
        let token = jwt(json!({ PROFILE_CLAIM_NS: { "email": "p@example.com" } }));
        let claims = parse_id_claims(&token).unwrap();
        assert_eq!(claims.email.as_deref(), Some("p@example.com"));
    }

    #[test]
    fn falls_back_to_user_id_when_chatgpt_user_id_absent() {
        let token = jwt(json!({ AUTH_CLAIM_NS: { "user_id": "legacy-1" } }));
        let claims = parse_id_claims(&token).unwrap();
        assert_eq!(claims.chatgpt_user_id.as_deref(), Some("legacy-1"));
    }

    #[test]
    fn token_without_auth_claim_parses_to_empty_claims() {
        let claims = parse_id_claims(&jwt(json!({}))).unwrap();
        assert_eq!(claims, IdClaims::default());
    }

    #[test]
    fn malformed_jwt_is_rejected() {
        for bad in ["", "onlyone", "two.parts", "a.b.c.d", ".b.c", "a..c"] {
            assert!(
                matches!(parse_id_claims(bad), Err(TokenError::Malformed)),
                "expected {bad:?} to be rejected as malformed"
            );
        }
    }

    #[test]
    fn non_base64_payload_is_rejected() {
        assert!(matches!(
            parse_id_claims("h.!!!not-base64!!!.s"),
            Err(TokenError::Base64(_))
        ));
    }

    #[test]
    fn non_json_payload_is_rejected() {
        let token = format!("h.{}.s", URL_SAFE_NO_PAD.encode("not json"));
        assert!(matches!(parse_id_claims(&token), Err(TokenError::Json(_))));
    }

    #[test]
    fn reads_expiry_claim() {
        assert_eq!(
            jwt_expiry(&jwt(json!({"exp": 1_700_000_000}))).unwrap(),
            Some(1_700_000_000)
        );
    }

    #[test]
    fn missing_or_negative_expiry_reads_as_none() {
        assert_eq!(jwt_expiry(&jwt(json!({}))).unwrap(), None);
        assert_eq!(jwt_expiry(&jwt(json!({"exp": -5}))).unwrap(), None);
    }

    fn tokens_expiring_at(exp: u64) -> Tokens {
        Tokens {
            access_token: jwt(json!({ "exp": exp })),
            refresh_token: "refresh-1".into(),
            id_token: id_token_with("plus", "acct-1"),
            account_id: None,
            last_refresh: Some(1_000),
        }
    }

    #[test]
    fn fresh_token_does_not_need_refresh() {
        let t = tokens_expiring_at(10_000);
        assert!(!t.needs_refresh(1_000));
    }

    #[test]
    fn token_inside_refresh_window_needs_refresh() {
        let t = tokens_expiring_at(10_000);
        // 5 minutes = 300s before expiry is the boundary, inclusive.
        assert!(t.needs_refresh(9_700));
        assert!(t.needs_refresh(9_701));
        assert!(!t.needs_refresh(9_699));
    }

    #[test]
    fn already_expired_token_needs_refresh() {
        let t = tokens_expiring_at(10_000);
        assert!(t.needs_refresh(10_001));
        // saturating_sub keeps this from underflowing into "fresh".
        assert!(t.needs_refresh(u64::MAX));
    }

    #[test]
    fn unreadable_expiry_needs_refresh() {
        // Fail toward refreshing: a token we can't reason about is
        // better re-minted than sent into a 401.
        let t = Tokens {
            access_token: "not-a-jwt".into(),
            refresh_token: "r".into(),
            ..Default::default()
        };
        assert!(t.needs_refresh(0));
    }

    #[test]
    fn stale_bundle_needs_refresh_even_when_token_is_valid() {
        let t = tokens_expiring_at(u64::MAX / 2);
        let nine_days = 9 * 24 * 60 * 60;
        assert!(t.needs_refresh(1_000 + nine_days));
        assert!(!t.needs_refresh(1_000 + 60));
    }

    #[test]
    fn account_id_prefers_cached_field_over_claim() {
        let mut t = tokens_expiring_at(10_000);
        t.account_id = Some("cached".into());
        assert_eq!(t.account_id().as_deref(), Some("cached"));
    }

    #[test]
    fn account_id_falls_back_to_id_token_claim() {
        let t = tokens_expiring_at(10_000);
        assert_eq!(t.account_id().as_deref(), Some("acct-1"));
    }

    #[test]
    fn account_id_is_none_when_neither_source_has_one() {
        let t = Tokens::default();
        assert_eq!(t.account_id(), None);
    }

    #[test]
    fn can_refresh_requires_a_non_blank_refresh_token() {
        let mut t = tokens_expiring_at(10_000);
        assert!(t.can_refresh());
        t.refresh_token = "   ".into();
        assert!(!t.can_refresh());
        t.refresh_token = String::new();
        assert!(!t.can_refresh());
    }

    #[test]
    fn tokens_round_trip_through_json() {
        let t = tokens_expiring_at(10_000);
        let back: Tokens = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn tokens_deserialize_from_minimal_json() {
        // Only `access_token` is required; a bundle written by an
        // older build must still load.
        let t: Tokens = serde_json::from_str(r#"{"access_token":"a"}"#).unwrap();
        assert_eq!(t.access_token, "a");
        assert!(!t.can_refresh());
    }
}
