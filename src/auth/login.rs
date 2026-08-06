//! Orchestration for `oli login` — the browser half.
//!
//! Sequence: generate PKCE + state, bind the loopback listener, send
//! the user to the authorize URL, wait for the redirect, exchange the
//! code, persist the bundle.
//!
//! The listener is bound *before* the browser opens, so there is no
//! window where the redirect arrives at a closed port.

use crate::auth::listener::CallbackServer;
use crate::auth::oauth::{authorize_url, exchange_code};
use crate::auth::store::AuthStore;
use crate::auth::token::Tokens;
use crate::auth::{ISSUER, client_id, pkce, redirect_uri};
use crate::error::Result;

/// Knobs for a login run. Defaults target the real issuer; tests and
/// `--no-browser` vary from there.
#[derive(Clone, Debug)]
pub struct LoginOptions {
    pub issuer: String,
    pub client_id: String,
    /// Try to launch a browser. When false (or when launching fails),
    /// the URL is printed for the user to open themselves.
    pub open_browser: bool,
}

impl Default for LoginOptions {
    fn default() -> Self {
        Self {
            issuer: ISSUER.to_string(),
            client_id: client_id(),
            open_browser: true,
        }
    }
}

/// Run the browser login flow and persist the result to `store`.
pub async fn run(opts: &LoginOptions, store: &AuthStore) -> Result<Tokens> {
    let pkce = pkce::generate();
    let state = pkce::generate_state();

    // Bind first: the redirect must never arrive at a dead port.
    let server = CallbackServer::bind().await?;
    let redirect = redirect_uri(server.port());
    let url = authorize_url(&opts.issuer, &opts.client_id, &redirect, &pkce, &state);

    println!("Sign in to ChatGPT to authorize Oli:\n\n  {url}\n");
    if opts.open_browser && open_browser(&url) {
        println!("Opened your browser. Waiting for sign-in…");
    } else {
        println!("Open the URL above in a browser. Waiting for sign-in…");
    }

    let callback = server.accept(&state).await?;

    let http = reqwest::Client::new();
    let tokens = exchange_code(
        &http,
        &opts.issuer,
        &opts.client_id,
        &redirect,
        &pkce,
        &callback.code,
    )
    .await?;

    store.save(&tokens)?;
    println!("{}", describe(&tokens, store));
    Ok(tokens)
}

/// One-line summary of who is now signed in and where that was
/// recorded.
pub fn describe(tokens: &Tokens, store: &AuthStore) -> String {
    let claims = tokens.claims();
    let who = claims
        .email
        .unwrap_or_else(|| "your ChatGPT account".into());
    let plan = claims
        .chatgpt_plan_type
        .map(|p| format!(" ({p} plan)"))
        .unwrap_or_default();
    format!(
        "Signed in as {who}{plan}. Credentials saved to {}.",
        store.path().display()
    )
}

/// Best-effort browser launch. Returns whether a launcher was actually
/// started — the caller prints the URL either way, so a false negative
/// costs the user nothing.
fn open_browser(url: &str) -> bool {
    // On Linux, a headless session has no browser to open. Detecting
    // that up front is better than spawning xdg-open into the void.
    #[cfg(target_os = "linux")]
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return false;
    }

    let launcher = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(launcher)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    fn id_token(claims: serde_json::Value) -> String {
        format!("h.{}.s", URL_SAFE_NO_PAD.encode(claims.to_string()))
    }

    #[test]
    fn default_options_target_the_real_issuer() {
        let opts = LoginOptions::default();
        assert_eq!(opts.issuer, "https://auth.openai.com");
        assert!(!opts.client_id.is_empty());
    }

    #[test]
    fn describe_names_the_account_plan_and_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = AuthStore::at(dir.path().join("auth.json"));
        let tokens = Tokens {
            id_token: id_token(serde_json::json!({
                "email": "user@example.com",
                "https://api.openai.com/auth": { "chatgpt_plan_type": "pro" }
            })),
            ..Default::default()
        };
        let line = describe(&tokens, &store);
        assert!(line.contains("user@example.com"), "{line}");
        assert!(line.contains("pro plan"), "{line}");
        assert!(line.contains("auth.json"), "{line}");
    }

    #[test]
    fn describe_degrades_gracefully_without_claims() {
        let dir = tempfile::tempdir().unwrap();
        let store = AuthStore::at(dir.path().join("auth.json"));
        let line = describe(&Tokens::default(), &store);
        assert!(line.contains("your ChatGPT account"), "{line}");
    }
}
