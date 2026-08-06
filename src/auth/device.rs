//! Headless sign-in — `oli login --device-auth`.
//!
//! SSH is a normal way to use Oli, so this is not optional: without it
//! subscription auth would only work on a machine with a browser and a
//! loopback port.
//!
//! **This is not RFC 8628.** OpenAI's device flow is a private
//! protocol and differs from the standard in ways that matter:
//!
//! - Endpoints are `POST /api/accounts/deviceauth/usercode` and
//!   `.../deviceauth/token`, both JSON, not the RFC's
//!   `/device_authorization` + form-encoded polling.
//! - The **server** generates the PKCE pair and returns it alongside
//!   the authorization code. The client does not choose a verifier.
//! - "Keep waiting" is signalled by HTTP 403/404, not by an
//!   `authorization_pending` error code.
//!
//! What comes back is an ordinary authorization code, which then goes
//! through the same exchange as the browser flow — against a
//! server-side redirect URI rather than a loopback one.

use serde::Deserialize;
use std::time::{Duration, Instant};

use crate::auth::oauth::{describe_token_error, exchange_code};
use crate::auth::pkce::Pkce;
use crate::auth::store::AuthStore;
use crate::auth::token::Tokens;
use crate::error::{AgentError, Result};

/// Give up after this long. Matches the code's own stated lifetime.
pub const DEVICE_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Poll interval when the server doesn't specify one.
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Bounds on the server-supplied interval. A zero would busy-loop
/// against OpenAI's servers; a huge one would look like a hang.
const MIN_POLL_INTERVAL: Duration = Duration::from_secs(1);
const MAX_POLL_INTERVAL: Duration = Duration::from_secs(60);

/// A pending device authorization: what to show the user, and what to
/// poll with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceCode {
    /// Page the user opens on another device.
    pub verification_url: String,
    /// Code the user types into that page.
    pub user_code: String,
    device_auth_id: String,
    interval: Duration,
}

impl DeviceCode {
    pub fn interval(&self) -> Duration {
        self.interval
    }
}

/// Raw `usercode` response. `interval` arrives as a string in some
/// responses and a number in others, hence the custom deserializer.
#[derive(Deserialize)]
struct UserCodeResponse {
    device_auth_id: String,
    #[serde(alias = "usercode")]
    user_code: String,
    #[serde(default, deserialize_with = "deserialize_interval")]
    interval: Option<u64>,
}

/// The grant handed back once the user has approved. Note the PKCE
/// pair comes *from the server* here.
#[derive(Debug, Deserialize)]
struct CodeResponse {
    authorization_code: String,
    code_verifier: String,
    #[serde(default)]
    code_challenge: String,
}

/// How a poll response should be treated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PollOutcome {
    /// User hasn't finished yet — wait and poll again.
    Pending,
    /// Approved; the body carries the authorization code.
    Approved,
    /// Anything else. Polling stops.
    Failed,
}

/// Classify a poll status code. Separated out because the mapping is
/// the surprising part of this protocol: 403 and 404 mean "not yet",
/// not "denied" and "gone".
pub fn classify_poll(status: u16) -> PollOutcome {
    match status {
        200..=299 => PollOutcome::Approved,
        403 | 404 => PollOutcome::Pending,
        _ => PollOutcome::Failed,
    }
}

/// Accept an interval given as either `"5"` or `5`.
fn deserialize_interval<'de, D>(deserializer: D) -> std::result::Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        Text(String),
        Number(u64),
    }
    Ok(match Option::<Either>::deserialize(deserializer)? {
        Some(Either::Number(n)) => Some(n),
        Some(Either::Text(s)) => s.trim().parse().ok(),
        None => None,
    })
}

/// Clamp a server-supplied interval into something sane.
fn poll_interval(seconds: Option<u64>) -> Duration {
    match seconds {
        Some(s) if s > 0 => Duration::from_secs(s).clamp(MIN_POLL_INTERVAL, MAX_POLL_INTERVAL),
        _ => DEFAULT_POLL_INTERVAL,
    }
}

/// The device-auth API lives under this prefix, not at the issuer root.
fn device_api_base(issuer: &str) -> String {
    format!("{}/api/accounts", issuer.trim_end_matches('/'))
}

/// Ask for a user code to display.
pub async fn request_device_code(
    http: &reqwest::Client,
    issuer: &str,
    client_id: &str,
) -> Result<DeviceCode> {
    let url = format!("{}/deviceauth/usercode", device_api_base(issuer));
    let resp = http
        .post(&url)
        .json(&serde_json::json!({ "client_id": client_id }))
        .send()
        .await
        .map_err(|e| {
            AgentError::Auth(format!(
                "could not reach the device-authorization endpoint ({url}): {e}. \
                 Check your network, or use API-key auth."
            ))
        })?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(AgentError::Auth(format!(
            "device-code sign-in is not available at {url} (HTTP 404). OpenAI may have \
             withdrawn it for third-party clients. Use `oli login` on a machine with a \
             browser, or use API-key auth."
        )));
    }
    if !status.is_success() {
        return Err(AgentError::Auth(format!(
            "device-code sign-in was rejected (HTTP {}): {}. \
             Use `oli login` with a browser, or API-key auth.",
            status.as_u16(),
            describe_token_error(&body)
        )));
    }

    let parsed: UserCodeResponse = serde_json::from_str(&body).map_err(|e| {
        AgentError::Auth(format!(
            "the device-authorization endpoint returned a response Oli could not parse ({e}). \
             Use API-key auth if this persists."
        ))
    })?;

    Ok(DeviceCode {
        // Codex-branded path; it is the page this OAuth client's
        // device codes are redeemed on, so it is not ours to rename.
        verification_url: format!("{}/codex/device", issuer.trim_end_matches('/')),
        user_code: parsed.user_code,
        device_auth_id: parsed.device_auth_id,
        interval: poll_interval(parsed.interval),
    })
}

/// Poll until the user approves, the code expires, or the server says
/// no. Returns the server-issued authorization code plus the PKCE pair
/// it was bound to.
async fn poll_for_authorization(
    http: &reqwest::Client,
    issuer: &str,
    device: &DeviceCode,
) -> Result<(String, Pkce)> {
    let url = format!("{}/deviceauth/token", device_api_base(issuer));
    let started = Instant::now();

    loop {
        let resp = http
            .post(&url)
            .json(&serde_json::json!({
                "device_auth_id": device.device_auth_id,
                "user_code": device.user_code,
            }))
            .send()
            .await
            .map_err(|e| {
                AgentError::Auth(format!(
                    "lost contact with the device-authorization endpoint ({url}): {e}. \
                     Re-run `oli login --device-auth`, or use API-key auth."
                ))
            })?;

        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();

        match classify_poll(status) {
            PollOutcome::Approved => {
                let parsed: CodeResponse = serde_json::from_str(&body).map_err(|e| {
                    AgentError::Auth(format!(
                        "the device-authorization endpoint approved sign-in but returned a \
                         response Oli could not parse ({e}). Use API-key auth if this persists."
                    ))
                })?;
                return Ok((
                    parsed.authorization_code,
                    Pkce {
                        verifier: parsed.code_verifier,
                        challenge: parsed.code_challenge,
                    },
                ));
            }
            PollOutcome::Pending => {
                if started.elapsed() >= DEVICE_TIMEOUT {
                    return Err(AgentError::Auth(
                        "the device code expired before it was approved (15 minutes). \
                         Re-run `oli login --device-auth`, or use API-key auth."
                            .to_string(),
                    ));
                }
                tokio::time::sleep(device.interval).await;
            }
            PollOutcome::Failed => {
                return Err(AgentError::Auth(format!(
                    "device-code sign-in failed (HTTP {status}): {}. \
                     Subscription auth is not a documented OpenAI feature and can be \
                     withdrawn without notice — use API-key auth \
                     (`api_key_env = \"OPENAI_API_KEY\"`) if this persists.",
                    describe_token_error(&body)
                )));
            }
        }
    }
}

/// Message shown while waiting. Kept separate so its content is
/// testable without driving the whole flow.
pub fn prompt(device: &DeviceCode) -> String {
    format!(
        "To sign in, open this page on any device with a browser:\n\n  {}\n\n\
         and enter this code (valid for 15 minutes):\n\n  {}\n\n\
         Only continue if you started this sign-in yourself. If someone sent you \
         this code, stop.\n\nWaiting for approval…",
        device.verification_url, device.user_code
    )
}

/// Run the whole headless flow and persist the result.
pub async fn run(issuer: &str, client_id: &str, store: &AuthStore) -> Result<Tokens> {
    let http = reqwest::Client::new();

    let device = request_device_code(&http, issuer, client_id).await?;
    println!("{}", prompt(&device));

    let (code, pkce) = poll_for_authorization(&http, issuer, &device).await?;

    // The redirect URI here is the server's own device callback, not a
    // loopback one — it has to match what the code was issued against.
    let redirect = format!("{}/deviceauth/callback", issuer.trim_end_matches('/'));
    let tokens = exchange_code(&http, issuer, client_id, &redirect, &pkce, &code).await?;

    store.save(&tokens)?;
    println!("{}", crate::auth::login::describe(&tokens, store));
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn device(interval: Duration) -> DeviceCode {
        DeviceCode {
            verification_url: "https://auth.openai.com/codex/device".into(),
            user_code: "ABCD-1234".into(),
            device_auth_id: "dev-1".into(),
            interval,
        }
    }

    #[test]
    fn pending_is_signalled_by_403_and_404_not_by_an_error_code() {
        assert_eq!(classify_poll(403), PollOutcome::Pending);
        assert_eq!(classify_poll(404), PollOutcome::Pending);
    }

    #[test]
    fn success_and_failure_statuses_are_classified() {
        assert_eq!(classify_poll(200), PollOutcome::Approved);
        assert_eq!(classify_poll(201), PollOutcome::Approved);
        assert_eq!(classify_poll(400), PollOutcome::Failed);
        assert_eq!(classify_poll(401), PollOutcome::Failed);
        assert_eq!(classify_poll(500), PollOutcome::Failed);
    }

    #[test]
    fn interval_accepts_a_string_or_a_number() {
        let from_string: UserCodeResponse =
            serde_json::from_str(r#"{"device_auth_id":"d","user_code":"u","interval":"7"}"#)
                .unwrap();
        assert_eq!(from_string.interval, Some(7));

        let from_number: UserCodeResponse =
            serde_json::from_str(r#"{"device_auth_id":"d","user_code":"u","interval":7}"#).unwrap();
        assert_eq!(from_number.interval, Some(7));
    }

    #[test]
    fn user_code_accepts_the_usercode_alias() {
        let parsed: UserCodeResponse =
            serde_json::from_str(r#"{"device_auth_id":"d","usercode":"XYZ"}"#).unwrap();
        assert_eq!(parsed.user_code, "XYZ");
    }

    #[test]
    fn interval_is_clamped_into_a_sane_range() {
        assert_eq!(poll_interval(Some(7)), Duration::from_secs(7));
        // A zero or missing interval would busy-loop against OpenAI.
        assert_eq!(poll_interval(Some(0)), DEFAULT_POLL_INTERVAL);
        assert_eq!(poll_interval(None), DEFAULT_POLL_INTERVAL);
        assert_eq!(poll_interval(Some(9_999)), MAX_POLL_INTERVAL);
    }

    #[test]
    fn device_api_lives_under_api_accounts() {
        assert_eq!(
            device_api_base("https://auth.openai.com/"),
            "https://auth.openai.com/api/accounts"
        );
    }

    #[test]
    fn prompt_shows_the_url_the_code_and_the_phishing_warning() {
        let text = prompt(&device(DEFAULT_POLL_INTERVAL));
        assert!(
            text.contains("https://auth.openai.com/codex/device"),
            "{text}"
        );
        assert!(text.contains("ABCD-1234"), "{text}");
        assert!(text.contains("Only continue if you started"), "{text}");
    }

    /// Serve a scripted sequence of responses, one per connection.
    async fn stub_sequence(responses: Vec<(u16, String)>) -> String {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            for (status, body) in responses {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
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

    #[tokio::test]
    async fn requests_a_user_code_and_builds_the_verification_url() {
        let issuer = stub_sequence(vec![(
            200,
            r#"{"device_auth_id":"dev-9","user_code":"WXYZ-1","interval":"3"}"#.into(),
        )])
        .await;

        let device = request_device_code(&reqwest::Client::new(), &issuer, "client-1")
            .await
            .unwrap();

        assert_eq!(device.user_code, "WXYZ-1");
        assert_eq!(device.interval(), Duration::from_secs(3));
        assert_eq!(device.verification_url, format!("{issuer}/codex/device"));
    }

    #[tokio::test]
    async fn a_404_on_usercode_says_the_flow_may_be_withdrawn() {
        let issuer = stub_sequence(vec![(404, "{}".into())]).await;

        let err = request_device_code(&reqwest::Client::new(), &issuer, "c")
            .await
            .unwrap_err()
            .to_string();

        assert!(err.contains("not available"), "{err}");
        assert!(err.contains("withdrawn"), "{err}");
        assert!(err.contains("API-key auth"), "{err}");
    }

    #[tokio::test]
    async fn polling_waits_through_403_then_takes_the_code() {
        let issuer = stub_sequence(vec![
            (403, "{}".into()),
            (403, "{}".into()),
            (
                200,
                r#"{"authorization_code":"ac-1","code_verifier":"ver-1","code_challenge":"ch-1"}"#
                    .into(),
            ),
        ])
        .await;

        let (code, pkce) = poll_for_authorization(
            &reqwest::Client::new(),
            &issuer,
            &device(Duration::from_millis(1)),
        )
        .await
        .unwrap();

        assert_eq!(code, "ac-1");
        // The verifier comes from the server in this flow, not from us.
        assert_eq!(pkce.verifier, "ver-1");
        assert_eq!(pkce.challenge, "ch-1");
    }

    #[tokio::test]
    async fn polling_stops_loudly_on_an_unexpected_status() {
        let issuer = stub_sequence(vec![(
            500,
            r#"{"error":"server_error","error_description":"upstream is down"}"#.into(),
        )])
        .await;

        let err = poll_for_authorization(
            &reqwest::Client::new(),
            &issuer,
            &device(Duration::from_millis(1)),
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(err.contains("500"), "{err}");
        assert!(err.contains("upstream is down"), "{err}");
        assert!(err.contains("OPENAI_API_KEY"), "{err}");
    }
}
