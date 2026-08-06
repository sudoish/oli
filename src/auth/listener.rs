//! Loopback HTTP listener for the OAuth redirect.
//!
//! Hand-rolled on the tokio already in the tree. What it has to do is
//! narrow enough that a web framework would be more code, not less:
//! accept a connection, read one request line, pull two query
//! parameters out of it, write one canned response.
//!
//! The ports are not ours to choose. `1455` (and `1457` as fallback)
//! are allow-listed as redirect URIs against the OAuth client, so
//! "pick any free port" is not an option — if both are busy, the login
//! cannot proceed and says so.
//!
//! Binding is `127.0.0.1`, never `0.0.0.0`: the authorization code
//! arrives in a URL and must not be reachable from the network.

use std::net::SocketAddr;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::auth::{CALLBACK_PATH, CALLBACK_PORT, CALLBACK_PORT_FALLBACK, form_decode};
use crate::error::{AgentError, Result};

/// How long to wait for the user to finish in the browser.
pub const CALLBACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Largest request head we will read. A callback URL is a few hundred
/// bytes; anything beyond this is not our redirect.
const MAX_REQUEST_BYTES: usize = 16 * 1024;

/// A bound loopback listener, plus the port it actually got.
#[derive(Debug)]
pub struct CallbackServer {
    listener: TcpListener,
    port: u16,
}

/// What the browser handed back on the redirect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Callback {
    pub code: String,
    pub state: String,
}

impl CallbackServer {
    /// Bind the preferred port, falling back to the alternate one.
    pub async fn bind() -> Result<Self> {
        Self::bind_ports(&[CALLBACK_PORT, CALLBACK_PORT_FALLBACK]).await
    }

    /// Bind the first available port from `ports`. Exposed so tests
    /// can bind an ephemeral port instead of the real ones.
    pub async fn bind_ports(ports: &[u16]) -> Result<Self> {
        let mut last_err = None;
        for &port in ports {
            let addr = SocketAddr::from(([127, 0, 0, 1], port));
            match TcpListener::bind(addr).await {
                Ok(listener) => {
                    let port = listener.local_addr().map(|a| a.port()).unwrap_or(port);
                    return Ok(Self { listener, port });
                }
                Err(e) => last_err = Some((port, e)),
            }
        }
        let detail = match last_err {
            Some((port, e)) => format!("port {port}: {e}"),
            None => "no ports given".to_string(),
        };
        Err(AgentError::Auth(format!(
            "cannot bind the sign-in callback listener ({detail}). \
             Ports {CALLBACK_PORT} and {CALLBACK_PORT_FALLBACK} are the only ones OpenAI \
             accepts for this client, so free one of them (another `oli login` or a `codex` \
             login may be holding it), or use `oli login --paste`, which needs no port at \
             all. `oli login --device-auth` and API-key auth also work."
        )))
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Wait for the redirect, ignoring unrelated requests (browsers
    /// like to ask for `/favicon.ico` first) until the callback path
    /// shows up or [`CALLBACK_TIMEOUT`] elapses.
    ///
    /// `expected_state` is compared against the returned `state` — a
    /// mismatch means the response is not the one this process
    /// started, and is refused.
    pub async fn accept(&self, expected_state: &str) -> Result<Callback> {
        let deadline = tokio::time::Instant::now() + CALLBACK_TIMEOUT;
        loop {
            let accepted = tokio::time::timeout_at(deadline, self.listener.accept())
                .await
                .map_err(|_| {
                    AgentError::Auth(
                        "timed out waiting for the browser to complete sign-in. \
                         If the browser is on a different machine than Oli (SSH, \
                         Tailscale, a container), its `localhost` is not this host's — \
                         use `oli login --paste` and paste the redirect URL back, or \
                         `oli login --device-auth`."
                            .to_string(),
                    )
                })?;
            let (mut stream, _) =
                accepted.map_err(|e| AgentError::Auth(format!("callback listener failed: {e}")))?;

            let Some(target) = read_request_target(&mut stream).await else {
                respond(&mut stream, 400, "text/plain", "Bad Request").await;
                continue;
            };

            let (path, query) = split_target(&target);
            if path != CALLBACK_PATH {
                respond(&mut stream, 404, "text/plain", "Not Found").await;
                continue;
            }

            match interpret_callback(query, expected_state) {
                Ok(callback) => {
                    respond(&mut stream, 200, "text/html; charset=utf-8", SUCCESS_PAGE).await;
                    return Ok(callback);
                }
                Err(e) => {
                    respond(
                        &mut stream,
                        400,
                        "text/html; charset=utf-8",
                        &failure_page(&e.to_string()),
                    )
                    .await;
                    return Err(e);
                }
            }
        }
    }
}

/// Turn the callback query string into a [`Callback`], or into a loud
/// error. Pure — this is where the interesting cases live, so it is
/// separated from the socket handling to stay testable.
pub fn interpret_callback(query: &str, expected_state: &str) -> Result<Callback> {
    let params = parse_query(query);
    let get = |k: &str| params.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());

    if let Some(error) = get("error") {
        let description = get("error_description").unwrap_or_else(|| "no detail given".into());
        return Err(AgentError::Auth(format!(
            "ChatGPT sign-in was refused: {description} [{error}]. \
             If your plan does not include this, or OpenAI has stopped accepting \
             third-party clients, use API-key auth instead."
        )));
    }

    let state = get("state").unwrap_or_default();
    if state != expected_state {
        return Err(AgentError::Auth(
            "the sign-in response did not match this login attempt (state mismatch); \
             it was discarded. Re-run `oli login`."
                .to_string(),
        ));
    }

    match get("code") {
        Some(code) if !code.is_empty() => Ok(Callback { code, state }),
        _ => Err(AgentError::Auth(
            "the sign-in response carried no authorization code. Re-run `oli login`, \
             or use API-key auth."
                .to_string(),
        )),
    }
}

/// Interpret something the user pasted back from a browser that could
/// not reach this machine's loopback.
///
/// Accepts, in order of preference:
/// 1. the whole redirect URL from the address bar,
///    `http://localhost:1455/auth/callback?code=…&state=…`
/// 2. just the query string, `code=…&state=…`
/// 3. a bare authorization code
///
/// Case 3 cannot verify `state`, so it warns. That is a deliberate
/// trade: in a paste flow the user is the transport, and the CSRF
/// binding that `state` provides against a *drive-by* redirect does
/// not apply to a value someone copied by hand. Refusing it would
/// just push people toward worse workarounds.
pub fn parse_pasted_redirect(input: &str, expected_state: &str) -> Result<Callback> {
    let trimmed = input.trim().trim_matches(['"', '\'']);
    if trimmed.is_empty() {
        return Err(AgentError::Auth(
            "nothing was pasted. Copy the whole URL from the browser's address bar \
             after signing in — it starts with `http://localhost:` and the page \
             failing to load is expected."
                .to_string(),
        ));
    }

    // A full URL: take everything after the first `?`.
    if let Some((_, query)) = trimmed.split_once('?') {
        return interpret_callback(query, expected_state);
    }

    // A bare query string.
    if trimmed.contains("code=") || trimmed.contains("error=") {
        return interpret_callback(trimmed, expected_state);
    }

    // A URL with no query at all is the most common paste mistake:
    // copying before the redirect happened.
    if trimmed.contains("://") || trimmed.contains('/') {
        return Err(AgentError::Auth(format!(
            "that URL has no authorization code in it ({trimmed}). \
             Sign in first, then copy the address bar once it has changed to a \
             `localhost` URL containing `?code=`."
        )));
    }

    // Bare code.
    if trimmed.split_whitespace().count() > 1 {
        return Err(AgentError::Auth(
            "that does not look like a redirect URL or an authorization code. \
             Copy the whole URL from the browser's address bar after signing in."
                .to_string(),
        ));
    }
    crate::log_warn!(
        "[auth] a bare authorization code was pasted, so the `state` value could not \
         be checked against this login attempt"
    );
    Ok(Callback {
        code: trimmed.to_string(),
        state: expected_state.to_string(),
    })
}

/// Read the request head and return the request target (the middle
/// field of the request line). `None` if it isn't a well-formed
/// request line.
async fn read_request_target(stream: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        // The request line is all we need; stop as soon as we have one.
        if buf.windows(2).any(|w| w == b"\r\n") || buf.len() >= MAX_REQUEST_BYTES {
            break;
        }
    }
    let head = String::from_utf8_lossy(&buf);
    let line = head.lines().next()?;
    let mut fields = line.split_whitespace();
    let _method = fields.next()?;
    Some(fields.next()?.to_string())
}

/// Split a request target into path and query.
fn split_target(target: &str) -> (&str, &str) {
    match target.split_once('?') {
        Some((path, query)) => (path, query),
        None => (target, ""),
    }
}

/// Parse `a=1&b=2` into pairs, percent-decoding both sides.
fn parse_query(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((k, v)) => (form_decode(k), form_decode(v)),
            None => (form_decode(pair), String::new()),
        })
        .collect()
}

/// Best-effort response write. A browser that has already navigated
/// away cannot be helped, and must not fail the login.
async fn respond(stream: &mut TcpStream, status: u16, content_type: &str, body: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Not Found",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(body.as_bytes()).await;
    let _ = stream.flush().await;
    let _ = stream.shutdown().await;
}

const SUCCESS_PAGE: &str = r#"<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><title>Signed in to Oli</title>
<style>
  :root { color-scheme: light dark; }
  body { font: 16px/1.6 ui-sans-serif, system-ui, sans-serif; display: grid;
         place-items: center; min-height: 100vh; margin: 0; }
  main { text-align: center; max-width: 32rem; padding: 2rem; }
  h1 { font-size: 1.5rem; margin: 0 0 .5rem; }
  p { margin: 0; opacity: .75; }
  code { font-family: ui-monospace, monospace; }
</style>
</head>
<body><main>
  <h1>Signed in to Oli</h1>
  <p>You can close this tab and return to your terminal.</p>
</main></body>
</html>"#;

fn failure_page(message: &str) -> String {
    let escaped = message
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!(
        r#"<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><title>Oli sign-in failed</title>
<style>
  :root {{ color-scheme: light dark; }}
  body {{ font: 16px/1.6 ui-sans-serif, system-ui, sans-serif; display: grid;
         place-items: center; min-height: 100vh; margin: 0; }}
  main {{ max-width: 36rem; padding: 2rem; }}
  h1 {{ font-size: 1.5rem; margin: 0 0 .5rem; }}
</style>
</head>
<body><main>
  <h1>Sign-in failed</h1>
  <p>{escaped}</p>
</main></body>
</html>"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_path_and_query() {
        assert_eq!(
            split_target("/auth/callback?code=1&state=2"),
            ("/auth/callback", "code=1&state=2")
        );
        assert_eq!(split_target("/favicon.ico"), ("/favicon.ico", ""));
    }

    #[test]
    fn parses_and_decodes_query_pairs() {
        let pairs = parse_query("code=a%20b&state=x%2By");
        assert_eq!(pairs[0], ("code".into(), "a b".into()));
        assert_eq!(pairs[1], ("state".into(), "x+y".into()));
    }

    #[test]
    fn accepts_a_matching_callback() {
        let cb = interpret_callback("code=abc&state=s1", "s1").unwrap();
        assert_eq!(
            cb,
            Callback {
                code: "abc".into(),
                state: "s1".into()
            }
        );
    }

    #[test]
    fn rejects_a_state_mismatch() {
        let err = interpret_callback("code=abc&state=other", "s1").unwrap_err();
        assert!(err.to_string().contains("state mismatch"), "{err}");
    }

    #[test]
    fn rejects_a_missing_state() {
        let err = interpret_callback("code=abc", "s1").unwrap_err();
        assert!(err.to_string().contains("state mismatch"), "{err}");
    }

    #[test]
    fn rejects_a_missing_code() {
        let err = interpret_callback("state=s1", "s1").unwrap_err();
        assert!(err.to_string().contains("no authorization code"), "{err}");
        assert!(err.to_string().contains("API-key"), "{err}");
    }

    #[test]
    fn surfaces_the_providers_error_and_names_the_fallback() {
        let err = interpret_callback(
            "error=access_denied&error_description=Your%20plan%20does%20not%20include%20this",
            "s1",
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Your plan does not include this"), "{msg}");
        assert!(msg.contains("access_denied"), "{msg}");
        assert!(msg.contains("API-key auth"), "{msg}");
    }

    #[test]
    fn provider_error_is_reported_even_without_a_description() {
        let err = interpret_callback("error=server_error", "s1").unwrap_err();
        assert!(err.to_string().contains("server_error"));
    }

    #[test]
    fn provider_error_beats_state_checking() {
        // An error response has no state to match; reporting "state
        // mismatch" there would hide the real reason.
        let err = interpret_callback("error=access_denied", "s1").unwrap_err();
        assert!(err.to_string().contains("access_denied"), "{err}");
    }

    #[tokio::test]
    async fn serves_the_success_page_and_returns_the_code() {
        let server = CallbackServer::bind_ports(&[0]).await.unwrap();
        let port = server.port();

        let client = tokio::spawn(async move {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            stream
                .write_all(b"GET /auth/callback?code=the-code&state=st HTTP/1.1\r\nHost: x\r\n\r\n")
                .await
                .unwrap();
            let mut body = String::new();
            stream.read_to_string(&mut body).await.unwrap();
            body
        });

        let callback = server.accept("st").await.unwrap();
        assert_eq!(callback.code, "the-code");

        let response = client.await.unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("Signed in to Oli"));
    }

    #[tokio::test]
    async fn ignores_unrelated_requests_and_keeps_waiting() {
        let server = CallbackServer::bind_ports(&[0]).await.unwrap();
        let port = server.port();

        tokio::spawn(async move {
            // Browsers routinely ask for this before the redirect.
            let mut favicon = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            favicon
                .write_all(b"GET /favicon.ico HTTP/1.1\r\nHost: x\r\n\r\n")
                .await
                .unwrap();
            let mut sink = String::new();
            let _ = favicon.read_to_string(&mut sink).await;

            let mut real = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            real.write_all(b"GET /auth/callback?code=c2&state=st HTTP/1.1\r\nHost: x\r\n\r\n")
                .await
                .unwrap();
            let mut sink = String::new();
            let _ = real.read_to_string(&mut sink).await;
        });

        let callback = server.accept("st").await.unwrap();
        assert_eq!(callback.code, "c2");
    }

    #[tokio::test]
    async fn reports_an_error_callback_to_the_browser_and_the_caller() {
        let server = CallbackServer::bind_ports(&[0]).await.unwrap();
        let port = server.port();

        let client = tokio::spawn(async move {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            stream
                .write_all(b"GET /auth/callback?error=access_denied HTTP/1.1\r\nHost: x\r\n\r\n")
                .await
                .unwrap();
            let mut body = String::new();
            stream.read_to_string(&mut body).await.unwrap();
            body
        });

        let err = server.accept("st").await.unwrap_err();
        assert!(err.to_string().contains("access_denied"));

        let response = client.await.unwrap();
        assert!(response.starts_with("HTTP/1.1 400"));
        assert!(response.contains("Sign-in failed"));
    }

    #[tokio::test]
    async fn bind_falls_back_to_the_second_port() {
        // Hold the first port so binding it fails, and check the
        // fallback is taken rather than the whole login aborting.
        let first = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let taken = first.local_addr().unwrap().port();
        let second = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let free = second.local_addr().unwrap().port();
        drop(second);

        let server = CallbackServer::bind_ports(&[taken, free]).await.unwrap();
        assert_eq!(server.port(), free);
    }

    #[tokio::test]
    async fn bind_failure_names_both_ports_and_the_alternatives() {
        let held = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = held.local_addr().unwrap().port();

        let err = CallbackServer::bind_ports(&[port]).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("--device-auth"), "{msg}");
        assert!(msg.contains("API-key"), "{msg}");
    }

    // ---- Pasted redirects (remote / SSH / tailscale) --------------

    #[test]
    fn accepts_a_full_pasted_redirect_url() {
        let cb = parse_pasted_redirect(
            "http://localhost:1455/auth/callback?code=abc&state=s1",
            "s1",
        )
        .unwrap();
        assert_eq!(cb.code, "abc");
    }

    #[test]
    fn accepts_a_url_with_surrounding_whitespace_and_quotes() {
        // Terminals and chat clients love adding these.
        let cb = parse_pasted_redirect(
            "  \"http://localhost:1455/auth/callback?code=abc&state=s1\"  ",
            "s1",
        )
        .unwrap();
        assert_eq!(cb.code, "abc");
    }

    #[test]
    fn accepts_a_bare_query_string() {
        let cb = parse_pasted_redirect("code=abc&state=s1", "s1").unwrap();
        assert_eq!(cb.code, "abc");
    }

    #[test]
    fn accepts_a_bare_authorization_code() {
        let cb = parse_pasted_redirect("just-the-code", "s1").unwrap();
        assert_eq!(cb.code, "just-the-code");
    }

    #[test]
    fn a_pasted_url_still_has_its_state_checked() {
        let err = parse_pasted_redirect(
            "http://localhost:1455/auth/callback?code=abc&state=wrong",
            "s1",
        )
        .unwrap_err();
        assert!(err.to_string().contains("state mismatch"), "{err}");
    }

    #[test]
    fn a_pasted_error_redirect_is_surfaced() {
        let err = parse_pasted_redirect(
            "http://localhost:1455/auth/callback?error=access_denied",
            "s1",
        )
        .unwrap_err();
        assert!(err.to_string().contains("access_denied"), "{err}");
    }

    #[test]
    fn empty_input_explains_what_to_copy() {
        let err = parse_pasted_redirect("   ", "s1").unwrap_err().to_string();
        assert!(err.contains("address bar"), "{err}");
        assert!(err.contains("failing to load is expected"), "{err}");
    }

    #[test]
    fn a_url_without_a_code_says_to_sign_in_first() {
        // Copying the authorize URL instead of the redirect is the
        // most common mistake in this flow.
        let err = parse_pasted_redirect("https://auth.openai.com/oauth/authorize", "s1")
            .unwrap_err()
            .to_string();
        assert!(err.contains("no authorization code"), "{err}");
        assert!(err.contains("?code="), "{err}");
    }

    #[test]
    fn prose_is_rejected_rather_than_treated_as_a_code() {
        let err = parse_pasted_redirect("I signed in but nothing happened", "s1")
            .unwrap_err()
            .to_string();
        assert!(err.contains("does not look like"), "{err}");
    }

    #[test]
    fn failure_page_escapes_the_message() {
        let page = failure_page("<script>alert(1)</script>");
        assert!(!page.contains("<script>"));
        assert!(page.contains("&lt;script&gt;"));
    }
}
