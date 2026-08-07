//! ChatGPT-subscription OAuth — "Sign in with ChatGPT".
//!
//! Lets Oli authenticate against a ChatGPT Plus/Pro/Business
//! subscription instead of an OpenAI API key. **API-key auth stays
//! first-class**: nothing in this module runs unless the user has
//! explicitly configured a `kind = "openai-chatgpt"` provider, and
//! every failure path here names the API-key fallback.
//!
//! Submodules:
//! - [`pkce`] — RFC 7636 code verifier / S256 challenge, plus the
//!   `state` nonce. Pure, no I/O.
//! - [`token`] — the persisted token bundle and JWT claim decoding
//!   (`chatgpt_plan_type`, `chatgpt_account_id`, `exp`). Pure.
//! - [`store`] — `auth.json` at mode 0600, next to `config.toml`.
//! - [`oauth`] — authorize URL, code exchange, token-endpoint errors.
//! - [`listener`] — loopback HTTP listener for the browser redirect.
//! - [`login`] — orchestration behind `oli login`.
//! - [`device`] — the headless path, `oli login --device-auth`.
//! - [`session`] — live credentials for the request path, refreshing
//!   before expiry so callers never see a token lifecycle.
//!
//! # Protocol
//!
//! Standard OAuth 2.0 authorization-code + PKCE against
//! `https://auth.openai.com`, with two OpenAI-specific wrinkles:
//!
//! 1. Requests authenticated this way do **not** go to
//!    `api.openai.com`. They go to [`CHATGPT_BASE_URL`] and speak the
//!    Responses API, with the account id echoed back in a
//!    `ChatGPT-Account-ID` header.
//! 2. The headless flow is not RFC 8628. It is a private endpoint
//!    pair under `/api/accounts/deviceauth/` where the *server*
//!    generates the PKCE pair.
//!
//! # Provenance
//!
//! Reimplemented from the wire protocol as observed in
//! [openai/codex](https://github.com/openai/codex) (Apache-2.0). No
//! code is copied from that project; the constants below are protocol
//! facts, not source.
//!
//! # Stability
//!
//! OpenAI does not document this flow or offer client registration for
//! third parties, so [`CLIENT_ID`] is Codex's own public client id.
//! Tolerance for third-party clients is informal and could be
//! withdrawn without notice — which is why the error paths here are
//! verbose rather than terse.

pub mod device;
pub mod listener;
pub mod login;
pub mod oauth;
pub mod pkce;
pub mod provision;
pub mod session;
pub mod store;
pub mod token;

/// OAuth issuer. Authorize, token exchange, refresh and revoke all
/// hang off this host.
pub const ISSUER: &str = "https://auth.openai.com";

/// Public OAuth client id. This is Codex's client — OpenAI has no
/// public client registration for third-party CLIs, and the redirect
/// URIs below are allow-listed against this id specifically, so there
/// is no alternative that reaches the same auth service.
///
/// Overridable via [`CLIENT_ID_ENV`] for anyone who *does* have their
/// own registered client.
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// Env var that overrides [`CLIENT_ID`].
pub const CLIENT_ID_ENV: &str = "OLI_CHATGPT_CLIENT_ID";

/// Scopes requested at authorize time. `offline_access` is what earns
/// the refresh token; without it every session would need a browser.
pub const SCOPES: &str =
    "openid profile email offline_access api.connectors.read api.connectors.invoke";

/// Loopback callback port. Allow-listed against [`CLIENT_ID`], so it
/// cannot be changed to an arbitrary free port.
pub const CALLBACK_PORT: u16 = 1455;

/// Second (and last) allow-listed callback port, used when
/// [`CALLBACK_PORT`] is occupied.
pub const CALLBACK_PORT_FALLBACK: u16 = 1457;

/// Path component of the loopback redirect URI.
pub const CALLBACK_PATH: &str = "/auth/callback";

/// Where subscription-authenticated model requests go. Note this is
/// *not* `api.openai.com` and *not* Chat Completions — see
/// [`crate::providers`] for the Responses-API transport.
pub const CHATGPT_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";

/// Resolve the OAuth client id, honouring [`CLIENT_ID_ENV`].
pub fn client_id() -> String {
    std::env::var(CLIENT_ID_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| CLIENT_ID.to_string())
}

/// Redirect URI for a given loopback port.
pub fn redirect_uri(port: u16) -> String {
    format!("http://localhost:{port}{CALLBACK_PATH}")
}

/// Percent-encode `s` for use in an `application/x-www-form-urlencoded`
/// body or a query-string value.
///
/// Hand-rolled to avoid a `url`/`urlencoding` dependency for what is
/// ultimately a dozen lines. Encodes everything outside the RFC 3986
/// unreserved set, which is stricter than necessary but never wrong.
pub fn form_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Decode a percent-encoded query-string value. `+` becomes a space,
/// per the `application/x-www-form-urlencoded` convention browsers use
/// when building query strings.
///
/// Invalid escapes are passed through verbatim rather than rejected —
/// this parses a redirect from a browser, and a mangled value is
/// better surfaced downstream (as "state mismatch" or "no code") than
/// as a parse error.
pub fn form_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok());
                match hex {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_uri_uses_localhost_and_callback_path() {
        assert_eq!(
            redirect_uri(CALLBACK_PORT),
            "http://localhost:1455/auth/callback"
        );
        assert_eq!(
            redirect_uri(CALLBACK_PORT_FALLBACK),
            "http://localhost:1457/auth/callback"
        );
    }

    #[test]
    fn client_id_defaults_to_the_codex_public_client() {
        // SAFETY: single-purpose env var, removed immediately after.
        unsafe { std::env::remove_var(CLIENT_ID_ENV) };
        assert_eq!(client_id(), CLIENT_ID);
    }

    #[test]
    fn client_id_env_override_wins() {
        // SAFETY: as above. Kept in one test so the set/remove pair is
        // not interleaved with the default-value assertion above.
        unsafe { std::env::set_var(CLIENT_ID_ENV, "app_custom") };
        assert_eq!(client_id(), "app_custom");
        unsafe { std::env::remove_var(CLIENT_ID_ENV) };
    }

    #[test]
    fn client_id_env_override_ignores_blank() {
        // SAFETY: as above.
        unsafe { std::env::set_var(CLIENT_ID_ENV, "   ") };
        assert_eq!(client_id(), CLIENT_ID);
        unsafe { std::env::remove_var(CLIENT_ID_ENV) };
    }

    #[test]
    fn form_encode_passes_unreserved_through() {
        assert_eq!(form_encode("aZ09-_.~"), "aZ09-_.~");
    }

    #[test]
    fn form_encode_escapes_url_punctuation() {
        assert_eq!(
            form_encode("http://localhost:1455/auth/callback"),
            "http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback"
        );
    }

    #[test]
    fn form_encode_escapes_space_and_plus() {
        // Scope strings are space-separated; a naive encoder that maps
        // space to `+` would break the `+`-in-a-token case.
        assert_eq!(form_encode("a b+c"), "a%20b%2Bc");
    }

    #[test]
    fn form_encode_handles_multibyte() {
        assert_eq!(form_encode("é"), "%C3%A9");
    }

    #[test]
    fn form_decode_reverses_form_encode() {
        for s in [
            "plain",
            "a b+c",
            "http://localhost:1455/auth/callback",
            "é",
            "sk-abc_DEF-123.~",
        ] {
            assert_eq!(
                form_decode(&form_encode(s)),
                s,
                "round trip failed for {s:?}"
            );
        }
    }

    #[test]
    fn form_decode_treats_plus_as_space() {
        // Browsers emit `+` for spaces in query strings even though
        // our encoder emits %20.
        assert_eq!(form_decode("a+b"), "a b");
    }

    #[test]
    fn form_decode_passes_through_invalid_escapes() {
        assert_eq!(form_decode("100%"), "100%");
        assert_eq!(form_decode("%zz"), "%zz");
        assert_eq!(form_decode("%A"), "%A");
    }

    #[test]
    fn form_decode_handles_multibyte_sequences() {
        assert_eq!(form_decode("%C3%A9t%C3%A9"), "été");
    }
}
