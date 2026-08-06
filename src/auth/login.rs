//! Orchestration for `oli login` — the browser half.
//!
//! Sequence: generate PKCE + state, bind the loopback listener, send
//! the user to the authorize URL, wait for the redirect, exchange the
//! code, persist the bundle.
//!
//! The listener is bound *before* the browser opens, so there is no
//! window where the redirect arrives at a closed port.
//!
//! [`run_paste`] is the same flow minus the listener, for when the
//! browser is on a different machine than Oli and `localhost` in the
//! redirect therefore points somewhere else entirely.

use crate::auth::listener::{CallbackServer, parse_pasted_redirect};
use crate::auth::oauth::{authorize_url, exchange_code};
use crate::auth::store::AuthStore;
use crate::auth::token::Tokens;
use crate::auth::{CALLBACK_PORT, ISSUER, client_id, pkce, redirect_uri};
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
        // If they open it on another machine, the redirect lands on
        // that machine's localhost and this wait never ends. Say so
        // now rather than after a ten-minute timeout.
        println!(
            "Open the URL above in a browser on this machine. Waiting for sign-in…\n\
             (Browser on a different machine? Ctrl-C and use `oli login --paste`.)"
        );
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

/// Browser login where the browser is on a *different machine* — SSH,
/// a Tailscale-connected host, a container.
///
/// The redirect goes to `http://localhost:1455/…`, which resolves on
/// whichever machine the browser is running on. When that isn't this
/// one, nothing can catch it: the browser shows a connection error
/// while the code sits unread in its address bar. So this mode skips
/// the listener entirely and asks the user to paste that URL back.
///
/// The redirect URI still has to be the loopback one — it is
/// allow-listed against the OAuth client and cannot be swapped for
/// something reachable. Nothing needs to be listening on it, though,
/// which also means this mode works when 1455 and 1457 are both busy.
pub async fn run_paste(opts: &LoginOptions, store: &AuthStore) -> Result<Tokens> {
    let pkce = pkce::generate();
    let state = pkce::generate_state();
    let redirect = redirect_uri(CALLBACK_PORT);
    let url = authorize_url(&opts.issuer, &opts.client_id, &redirect, &pkce, &state);

    println!(
        "Open this URL in a browser — any machine, it does not have to be this one:\n\n  {url}\n\n\
         After you sign in, the browser will try to reach {redirect}\n\
         and show a connection error. That is expected: nothing is listening there.\n\
         Copy the full URL out of the address bar and paste it below.\n"
    );

    let pasted = read_line("Pasted URL: ").await?;
    let callback = parse_pasted_redirect(&pasted, &state)?;

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

/// Prompt and read one line from stdin, off the async runtime.
async fn read_line(prompt: &str) -> Result<String> {
    use std::io::{BufRead, Write};

    let prompt = prompt.to_string();
    tokio::task::spawn_blocking(move || {
        print!("{prompt}");
        std::io::stdout().flush().ok();
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        Ok(line)
    })
    .await
    .map_err(|e| crate::error::AgentError::Auth(format!("could not read input: {e}")))?
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

#[cfg(test)]
mod paste_tests {
    //! End-to-end cover for the paste flow's non-interactive half:
    //! everything from a pasted URL through to a persisted bundle.
    use super::*;
    use crate::auth::listener::parse_pasted_redirect;
    use crate::auth::pkce::Pkce;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn stub_token_endpoint(body: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let _ = stream.read(&mut buf).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.flush().await;
        });
        format!("http://127.0.0.1:{port}")
    }

    #[tokio::test]
    async fn a_pasted_redirect_completes_the_exchange_and_persists() {
        let issuer = stub_token_endpoint(
            r#"{"access_token":"at-1","refresh_token":"rt-1","id_token":"h.e30.s"}"#,
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let store = AuthStore::at(dir.path().join("auth.json"));

        // What `run_paste` does between prompt and save.
        let state = "state-xyz";
        let pkce = Pkce {
            verifier: "v".into(),
            challenge: "c".into(),
        };
        let callback = parse_pasted_redirect(
            "http://localhost:1455/auth/callback?code=the-code&state=state-xyz",
            state,
        )
        .unwrap();
        let tokens = exchange_code(
            &reqwest::Client::new(),
            &issuer,
            "client-1",
            &redirect_uri(CALLBACK_PORT),
            &pkce,
            &callback.code,
        )
        .await
        .unwrap();
        store.save(&tokens).unwrap();

        assert_eq!(store.load().unwrap().unwrap().access_token, "at-1");
    }

    #[test]
    fn the_paste_redirect_uri_matches_the_authorize_redirect_uri() {
        // These must be byte-identical or the exchange is rejected
        // with an opaque invalid_grant.
        let pkce = Pkce {
            verifier: "v".into(),
            challenge: "c".into(),
        };
        let redirect = redirect_uri(CALLBACK_PORT);
        let url = authorize_url("https://i", "c", &redirect, &pkce, "s");
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback"));
        assert_eq!(redirect, "http://localhost:1455/auth/callback");
    }
}
