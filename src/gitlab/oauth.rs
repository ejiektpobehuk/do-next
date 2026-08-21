//! GitLab OAuth 2.0: the Web (authorization code + PKCE) and Device
//! authorization flows, plus token persistence and refresh.
//!
//! GitLab issues no client secret for a public app, so nothing secret is
//! shipped or shared — the app's `client_id` is a connection setting. The
//! registered app must have **Confidential unchecked**, or the token request
//! fails with `invalid_client`.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{Duration as ChronoDuration, Utc};

use crate::jira::auth::OAuthStore;
use crate::oauth::{self, LoopbackServer, percent_encode};

use super::auth::GitlabOAuthCredentials;

/// The only scope we need. Every endpoint `do-next` calls is a GET, and
/// GitLab's `read_api` is enforced as "any GET/HEAD under /api/v4", so this
/// covers merge requests, approvals, pipelines, `/events` and `/projects`
/// alike. Deliberately narrower than `glab`, which asks for
/// `openid profile read_user write_repository api`.
pub const SCOPES: &str = "read_api";

/// Preferred callback port. Falls back to an OS-assigned one when taken —
/// which works because the registered redirect URI uses a literal loopback IP
/// (see [`redirect_uri`]).
pub const PREFERRED_PORT: u16 = 19873;

/// The redirect URI to register for the app: a literal `127.0.0.1`, no port.
///
/// GitLab's Doorkeeper applies RFC 8252 §7.3 loopback matching — ignoring the
/// port — only when both URIs parse as literal loopback IPs. A `localhost` URI
/// is compared as a plain string, which would pin us to one port forever.
pub const REGISTERED_REDIRECT_URI: &str = "http://127.0.0.1:19873/callback";

/// The redirect URI for an actually-bound port.
fn redirect_uri(port: u16) -> String {
    format!("http://127.0.0.1:{port}/callback")
}

/// How long to wait for the user to consent in the browser.
const CONSENT_TIMEOUT: Duration = Duration::from_mins(3);

/// Attempts to re-read a rotated refresh token before giving up, and the pause
/// between them. GitLab refresh tokens are single-use, so when a concurrent
/// process wins the race our token is already spent; it writes the replacement
/// moments later, and adopting it beats forcing the user to log in again.
const ADOPT_ATTEMPTS: u32 = 3;
const ADOPT_BACKOFF: Duration = Duration::from_millis(50);

fn authorize_url(base_url: &str) -> String {
    format!("{}/oauth/authorize", base_url.trim_end_matches('/'))
}

fn token_url(base_url: &str) -> String {
    format!("{}/oauth/token", base_url.trim_end_matches('/'))
}

fn device_url(base_url: &str) -> String {
    format!("{}/oauth/authorize_device", base_url.trim_end_matches('/'))
}

/// Run the Web flow: browser consent, loopback callback, code exchange.
///
/// Synchronous because it blocks on the browser round trip; the HTTP calls go
/// through [`oauth::blocking_http`].
pub fn run_web_flow(
    base_url: &str,
    client_id: &str,
    client_secret: Option<&str>,
    store: OAuthStore,
) -> Result<GitlabOAuthCredentials> {
    let server = LoopbackServer::bind(PREFERRED_PORT)?;
    let port = server.port();
    if port != PREFERRED_PORT {
        println!(
            "Port {PREFERRED_PORT} is busy — using {port} instead \
             (accepted because the app's redirect URI is a 127.0.0.1 address)."
        );
    }
    let redirect = redirect_uri(port);

    let pkce = oauth::pkce();
    let state = oauth::state();

    let auth_url = format!(
        "{authorize}?\
         client_id={client_id}&\
         redirect_uri={redirect_enc}&\
         response_type=code&\
         state={state}&\
         scope={scope}&\
         code_challenge={challenge}&\
         code_challenge_method=S256",
        authorize = authorize_url(base_url),
        redirect_enc = percent_encode(&redirect),
        scope = percent_encode(SCOPES),
        challenge = pkce.challenge,
    );

    println!("Opening browser for GitLab authorization...");
    if open::that(&auth_url).is_err() {
        println!("Could not open browser automatically.");
    }
    // Printed unconditionally: over SSH or in a terminal multiplexer the
    // browser often "opens" somewhere the user cannot see.
    println!("If the browser didn't open, visit this URL:");
    println!("  {auth_url}");
    println!();
    println!("Waiting for authorization (up to 3 minutes)...");

    let code = server.await_code(&state, CONSENT_TIMEOUT)?;
    println!("Authorization received. Exchanging for tokens...");

    let form = vec![
        ("grant_type".to_string(), "authorization_code".to_string()),
        ("client_id".to_string(), client_id.to_string()),
        ("code".to_string(), code),
        ("redirect_uri".to_string(), redirect),
        ("code_verifier".to_string(), pkce.verifier),
    ];
    let token_url = token_url(base_url);
    let secret = client_secret.map(str::to_owned);
    let token_data = oauth::blocking_http(move || {
        Box::pin(async move { post_token(&token_url, form, secret.as_deref()).await })
    })?;

    let creds = into_credentials(token_data, base_url, client_id, client_secret, store);
    save_oauth_tokens(&creds)?;
    println!("OAuth tokens saved.");
    Ok(creds)
}

/// Run the Device authorization flow: show a code, poll for the token.
///
/// No listener and no redirect URI, so this is the flow that works when
/// `do-next` runs over SSH. Needs GitLab 17.9+ and an app whose allowed grant
/// types include `device_code`.
pub fn run_device_flow(
    base_url: &str,
    client_id: &str,
    client_secret: Option<&str>,
    store: OAuthStore,
) -> Result<GitlabOAuthCredentials> {
    let authorization = request_device_authorization(base_url, client_id)?;

    println!();
    println!("First copy your one-time code: {}", authorization.user_code);
    // `verification_uri_complete` embeds the code, so the user only has to
    // confirm. Fall back to the bare URI when the server omits it.
    let open_url = authorization
        .verification_uri_complete
        .as_deref()
        .unwrap_or(&authorization.verification_uri);
    println!("Then open this URL on any device to authorize:");
    println!("  {open_url}");
    println!();
    if open::that(open_url).is_ok() {
        println!("(Tried opening it in your browser too.)");
    }
    println!("Waiting for authorization...");

    let token_data = poll_for_device_token(base_url, client_id, client_secret, &authorization)?;
    let creds = into_credentials(token_data, base_url, client_id, client_secret, store);
    save_oauth_tokens(&creds)?;
    println!("OAuth tokens saved.");
    Ok(creds)
}

/// Ask the instance for a device code and the URL the user should visit.
fn request_device_authorization(base_url: &str, client_id: &str) -> Result<DeviceAuthorization> {
    let device_url = device_url(base_url);
    let id = client_id.to_string();
    oauth::blocking_http(move || {
        Box::pin(async move {
            let http = reqwest::Client::new();
            let resp = http
                .post(&device_url)
                .form(&[("client_id", id.as_str()), ("scope", SCOPES)])
                .send()
                .await
                .context("Failed to start the GitLab device authorization flow")?;
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                bail!(
                    "Device authorization failed ({status}): {}\n\
                     The app's allowed grant types must include `device_code`, \
                     and the instance must run GitLab 17.9 or later.",
                    snippet(&body)
                );
            }
            resp.json::<DeviceAuthorization>()
                .await
                .context("Failed to parse the device authorization response")
        })
    })
}

/// Poll the token endpoint until the user authorizes, refuses, or the code
/// expires.
fn poll_for_device_token(
    base_url: &str,
    client_id: &str,
    client_secret: Option<&str>,
    authorization: &DeviceAuthorization,
) -> Result<TokenResponse> {
    let token_url = token_url(base_url);
    let id = client_id.to_string();
    let secret = client_secret.map(str::to_owned);
    let device_code = authorization.device_code.clone();
    let interval = Duration::from_secs(authorization.interval.max(1));
    let expires_in = Duration::from_secs(authorization.expires_in.max(60));

    oauth::blocking_http(move || {
        Box::pin(async move {
            let http = reqwest::Client::new();
            let deadline = tokio::time::Instant::now() + expires_in;
            let mut wait = interval;
            loop {
                if tokio::time::Instant::now() >= deadline {
                    bail!(
                        "The one-time code expired before it was authorized.\n\
                         Run `do-next auth` to try again."
                    );
                }
                tokio::time::sleep(wait).await;

                let mut form = vec![
                    (
                        "grant_type",
                        "urn:ietf:params:oauth:grant-type:device_code".to_string(),
                    ),
                    ("client_id", id.clone()),
                    ("device_code", device_code.clone()),
                ];
                if let Some(secret) = &secret {
                    form.push(("client_secret", secret.clone()));
                }
                let resp = http
                    .post(&token_url)
                    .form(&form)
                    .send()
                    .await
                    .context("Failed to poll for the device authorization token")?;

                if resp.status().is_success() {
                    return resp
                        .json::<TokenResponse>()
                        .await
                        .context("Failed to parse the device token response");
                }

                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                match device_poll_outcome(error_code(&body).as_deref()) {
                    PollOutcome::KeepWaiting => {}
                    PollOutcome::SlowDown => wait += SLOW_DOWN_STEP,
                    PollOutcome::Expired => bail!(
                        "The one-time code expired before it was authorized.\n\
                         Run `do-next auth` to try again."
                    ),
                    PollOutcome::Denied => {
                        bail!("Authorization was denied. Run `do-next auth` to try again.")
                    }
                    PollOutcome::Failed => {
                        bail!("Device authorization failed ({status}): {}", snippet(&body))
                    }
                }
            }
        })
    })
}

/// RFC 8628 says to lengthen the interval by 5 seconds on `slow_down`.
const SLOW_DOWN_STEP: Duration = Duration::from_secs(5);

/// What a non-success poll response means.
#[derive(Debug, PartialEq, Eq)]
enum PollOutcome {
    /// Not confirmed in the browser yet.
    KeepWaiting,
    /// Poll less often.
    SlowDown,
    Expired,
    Denied,
    /// Anything else — a real error.
    Failed,
}

/// Classify a device-flow poll error. Pure, so the polling rules are testable
/// without a server.
fn device_poll_outcome(error: Option<&str>) -> PollOutcome {
    match error {
        Some("authorization_pending") => PollOutcome::KeepWaiting,
        Some("slow_down") => PollOutcome::SlowDown,
        Some("expired_token") => PollOutcome::Expired,
        Some("access_denied") => PollOutcome::Denied,
        _ => PollOutcome::Failed,
    }
}

/// Exchange or refresh at the token endpoint.
async fn post_token(
    token_url: &str,
    mut form: Vec<(String, String)>,
    client_secret: Option<&str>,
) -> Result<TokenResponse> {
    if let Some(secret) = client_secret {
        form.push(("client_secret".to_string(), secret.to_string()));
    }
    let http = reqwest::Client::new();
    let resp = http
        .post(token_url)
        .form(&form)
        .send()
        .await
        .context("Failed to reach the GitLab token endpoint")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if error_code(&body).as_deref() == Some("invalid_client") {
            bail!(
                "GitLab rejected the OAuth app ({status}: invalid_client).\n\
                 The app must have `Confidential` unchecked — a confidential app \
                 requires a client secret.\n\
                 Check the client ID too, then run `do-next auth` again."
            );
        }
        bail!("GitLab token request failed ({status}): {}", snippet(&body));
    }

    resp.json()
        .await
        .context("Failed to parse the GitLab token response")
}

/// Refresh an expired access token. GitLab rotates the refresh token, so the
/// caller must persist both halves of the result.
pub async fn refresh_access_token(
    creds: &GitlabOAuthCredentials,
) -> Result<GitlabOAuthCredentials> {
    let form = vec![
        ("grant_type".to_string(), "refresh_token".to_string()),
        ("client_id".to_string(), creds.client_id.clone()),
        ("refresh_token".to_string(), creds.refresh_token.clone()),
    ];
    let token_url = token_url(&creds.base_url);

    match post_token(&token_url, form, creds.client_secret.as_deref()).await {
        Ok(token_data) => Ok(apply_token_response(token_data, creds)),
        Err(e) if is_invalid_grant(&e) => {
            // Most likely another process spent this single-use refresh token.
            // Give it a moment to write the replacement, then adopt it.
            for _ in 0..ADOPT_ATTEMPTS {
                tokio::time::sleep(ADOPT_BACKOFF).await;
                if let Some(fresh) = reload(creds)
                    && fresh.refresh_token != creds.refresh_token
                {
                    log::debug!("adopted a GitLab refresh token rotated by another process");
                    return Ok(fresh);
                }
            }
            bail!(
                "Your GitLab session has expired and could not be refreshed.\n\
                 Run `do-next auth` to sign in again."
            )
        }
        Err(e) => Err(e),
    }
}

fn is_invalid_grant(e: &anyhow::Error) -> bool {
    e.to_string().contains("invalid_grant")
}

/// Best-effort re-read of the stored credentials for this instance, keeping the
/// client identity from `current` (config may override what was persisted).
pub fn reload(current: &GitlabOAuthCredentials) -> Option<GitlabOAuthCredentials> {
    let mut fresh = load_oauth_tokens(&current.base_url).ok().flatten()?;
    fresh.client_id.clone_from(&current.client_id);
    fresh.client_secret.clone_from(&current.client_secret);
    Some(fresh)
}

fn into_credentials(
    token_data: TokenResponse,
    base_url: &str,
    client_id: &str,
    client_secret: Option<&str>,
    store: OAuthStore,
) -> GitlabOAuthCredentials {
    GitlabOAuthCredentials {
        access_token: token_data.access_token,
        refresh_token: token_data.refresh_token,
        expires_at: expires_at(token_data.expires_in),
        client_id: client_id.to_string(),
        client_secret: client_secret.map(str::to_owned),
        base_url: base_url.trim_end_matches('/').to_string(),
        store,
    }
}

fn apply_token_response(
    token_data: TokenResponse,
    previous: &GitlabOAuthCredentials,
) -> GitlabOAuthCredentials {
    GitlabOAuthCredentials {
        access_token: token_data.access_token,
        // GitLab always returns a fresh refresh token, but tolerate a server
        // that reuses the old one rather than dropping our only copy.
        refresh_token: if token_data.refresh_token.is_empty() {
            previous.refresh_token.clone()
        } else {
            token_data.refresh_token
        },
        expires_at: expires_at(token_data.expires_in),
        client_id: previous.client_id.clone(),
        client_secret: previous.client_secret.clone(),
        base_url: previous.base_url.clone(),
        store: previous.store.clone(),
    }
}

/// Absolute expiry from the response's `expires_in`.
///
/// Never assume 7200: instance admins can set the OAuth token lifetime as low
/// as 300 seconds. A response without the field is treated as already expired
/// so the next request refreshes rather than sending a token we cannot vouch
/// for.
fn expires_at(expires_in: Option<i64>) -> chrono::DateTime<Utc> {
    Utc::now() + ChronoDuration::seconds(expires_in.unwrap_or(0))
}

// --- Storage -----------------------------------------------------------------

/// Keyring key for one instance's tokens. The instance URL is known before the
/// tokens are loaded, so — unlike the Atlassian flow, which must look up a
/// cloud id — no index entry is needed.
pub fn keyring_key(base_url: &str) -> String {
    format!("gitlab-oauth:{}", base_url.trim_end_matches('/'))
}

fn tokens_file_path() -> Result<std::path::PathBuf> {
    Ok(dirs::config_dir()
        .context("Cannot determine config directory")?
        .join("do-next")
        .join("gitlab_oauth_tokens.json5"))
}

/// Persist tokens to the store they came from.
pub fn save_oauth_tokens(creds: &GitlabOAuthCredentials) -> Result<()> {
    let stored = StoredTokens {
        access_token: creds.access_token.clone(),
        refresh_token: creds.refresh_token.clone(),
        expires_at: creds.expires_at.to_rfc3339(),
        base_url: creds.base_url.clone(),
        client_id: creds.client_id.clone(),
        client_secret: creds.client_secret.clone(),
    };
    let json = serde_json::to_string(&stored).context("Failed to serialize GitLab OAuth tokens")?;

    match creds.store {
        OAuthStore::Keyring => {
            let key = keyring_key(&creds.base_url);
            let entry = keyring::Entry::new("do-next", &key)
                .context("Failed to access keyring for GitLab OAuth tokens")?;
            entry
                .set_password(&json)
                .context("Failed to store GitLab OAuth tokens in keyring")?;
            log::debug!("GitLab OAuth tokens saved to keyring (key={key})");
        }
        OAuthStore::File => {
            let path = tokens_file_path()?;
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut all = read_tokens_file()?;
            all.retain(|t| t.base_url != stored.base_url);
            all.push(stored);
            let json =
                serde_json::to_string(&all).context("Failed to serialize GitLab OAuth tokens")?;
            std::fs::write(&path, &json)
                .with_context(|| format!("Failed to write {}", path.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            }
            log::debug!("GitLab OAuth tokens saved to {}", path.display());
        }
    }
    Ok(())
}

/// Load stored tokens for one instance: keyring first, then the file.
pub fn load_oauth_tokens(base_url: &str) -> Result<Option<GitlabOAuthCredentials>> {
    let base_url = base_url.trim_end_matches('/');
    if let Some(creds) = load_from_keyring(base_url)? {
        return Ok(Some(creds));
    }
    load_from_file(base_url)
}

fn load_from_keyring(base_url: &str) -> Result<Option<GitlabOAuthCredentials>> {
    let key = keyring_key(base_url);
    // Since keyring 4 the platform store initializes on the first `Entry::new`,
    // so an unavailable keyring fails here. Treat that like a missing entry so
    // the file store still gets a chance.
    let entry = match keyring::Entry::new("do-next", &key) {
        Ok(entry) => entry,
        Err(e) => {
            log::debug!("keyring unavailable for GitLab OAuth lookup: {e}");
            return Ok(None);
        }
    };
    let json = match entry.get_password() {
        Ok(json) => json,
        Err(keyring::Error::NoEntry) => return Ok(None),
        Err(e) => {
            log::debug!("keyring GitLab OAuth lookup failed: {e}");
            return Ok(None);
        }
    };
    let stored: StoredTokens = serde_json::from_str(&json)
        .context("Failed to parse GitLab OAuth tokens from the keyring")?;
    parse_stored(stored, OAuthStore::Keyring).map(Some)
}

fn load_from_file(base_url: &str) -> Result<Option<GitlabOAuthCredentials>> {
    let Some(stored) = read_tokens_file()?
        .into_iter()
        .find(|t| t.base_url.trim_end_matches('/') == base_url)
    else {
        return Ok(None);
    };
    parse_stored(stored, OAuthStore::File).map(Some)
}

fn read_tokens_file() -> Result<Vec<StoredTokens>> {
    let path = tokens_file_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    json5::from_str(&content).context("Failed to parse gitlab_oauth_tokens.json5")
}

fn parse_stored(stored: StoredTokens, store: OAuthStore) -> Result<GitlabOAuthCredentials> {
    let expires_at = chrono::DateTime::parse_from_rfc3339(&stored.expires_at)
        .context("Failed to parse expires_at timestamp")?
        .with_timezone(&Utc);
    Ok(GitlabOAuthCredentials {
        access_token: stored.access_token,
        refresh_token: stored.refresh_token,
        expires_at,
        client_id: stored.client_id,
        client_secret: stored.client_secret,
        base_url: stored.base_url,
        store,
    })
}

// --- Wire types --------------------------------------------------------------

#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    /// Absent on instances that disable expiry; see [`expires_at`].
    #[serde(default)]
    expires_in: Option<i64>,
}

#[derive(serde::Deserialize)]
struct DeviceAuthorization {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    #[serde(default = "default_expires_in")]
    expires_in: u64,
    #[serde(default = "default_interval")]
    interval: u64,
}

const fn default_expires_in() -> u64 {
    300
}

const fn default_interval() -> u64 {
    5
}

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredTokens {
    access_token: String,
    refresh_token: String,
    expires_at: String,
    base_url: String,
    client_id: String,
    #[serde(default)]
    client_secret: Option<String>,
}

/// The `error` code from an OAuth error body, if it parses as one.
fn error_code(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .get("error")?
        .as_str()
        .map(str::to_owned)
}

/// Longest error-body excerpt carried into a message. Matches `crate::http`.
const MAX_BODY_SNIPPET: usize = 300;

fn snippet(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "(empty response body)".to_string();
    }
    if trimmed.chars().count() <= MAX_BODY_SNIPPET {
        return trimmed.to_string();
    }
    let truncated: String = trimmed.chars().take(MAX_BODY_SNIPPET).collect();
    format!("{truncated}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creds() -> GitlabOAuthCredentials {
        GitlabOAuthCredentials {
            access_token: "at".into(),
            refresh_token: "rt".into(),
            expires_at: Utc::now(),
            client_id: "cid".into(),
            client_secret: None,
            base_url: "https://gitlab.example.com".into(),
            store: OAuthStore::File,
        }
    }

    #[test]
    fn the_registered_redirect_uri_is_a_literal_loopback_ip() {
        // Doorkeeper only ignores the port when both URIs are literal loopback
        // IPs, which is what makes the busy-port fallback legal at all.
        assert!(REGISTERED_REDIRECT_URI.starts_with("http://127.0.0.1:"));
        assert!(!REGISTERED_REDIRECT_URI.contains("localhost"));
        assert_eq!(REGISTERED_REDIRECT_URI, redirect_uri(PREFERRED_PORT));
    }

    #[test]
    fn a_fallback_port_lands_in_the_redirect_uri() {
        assert_eq!(redirect_uri(54417), "http://127.0.0.1:54417/callback");
    }

    #[test]
    fn endpoints_are_derived_from_the_instance_url_without_forcing_https() {
        // A plain-http self-managed instance must not be silently upgraded —
        // the bug `glab` has by hardcoding its default protocol.
        assert_eq!(
            authorize_url("http://gitlab.internal"),
            "http://gitlab.internal/oauth/authorize"
        );
        // A trailing slash must not double up.
        assert_eq!(
            token_url("https://gitlab.example.com/"),
            "https://gitlab.example.com/oauth/token"
        );
        assert_eq!(
            device_url("https://gitlab.example.com"),
            "https://gitlab.example.com/oauth/authorize_device"
        );
    }

    #[test]
    fn only_read_api_is_requested() {
        // The read-only guarantee is a documented property of GitLab support,
        // so widening this should require changing a test.
        assert_eq!(SCOPES, "read_api");
    }

    #[test]
    fn expires_in_is_honoured_rather_than_assumed() {
        // An instance admin can set the lifetime as low as 300s.
        let short = expires_at(Some(300));
        assert!(short > Utc::now() + ChronoDuration::seconds(250));
        assert!(short < Utc::now() + ChronoDuration::seconds(350));
        // A missing expires_in must not be optimistically treated as 2h.
        let missing = expires_at(None);
        assert!(missing <= Utc::now() + ChronoDuration::seconds(1));
    }

    #[test]
    fn a_refresh_replaces_both_tokens() {
        let previous = creds();
        let response = TokenResponse {
            access_token: "new-at".into(),
            refresh_token: "new-rt".into(),
            expires_in: Some(7200),
        };
        let next = apply_token_response(response, &previous);
        assert_eq!(next.access_token, "new-at");
        assert_eq!(next.refresh_token, "new-rt");
        // Client identity and destination are carried over, not re-derived.
        assert_eq!(next.client_id, "cid");
        assert_eq!(next.base_url, "https://gitlab.example.com");
    }

    #[test]
    fn a_refresh_response_without_a_new_refresh_token_keeps_the_old_one() {
        let previous = creds();
        let response = TokenResponse {
            access_token: "new-at".into(),
            refresh_token: String::new(),
            expires_in: Some(7200),
        };
        let next = apply_token_response(response, &previous);
        assert_eq!(next.refresh_token, "rt", "must not lose the only copy");
    }

    #[test]
    fn error_bodies_are_recognised() {
        assert_eq!(
            error_code(r#"{"error":"invalid_grant"}"#).as_deref(),
            Some("invalid_grant")
        );
        assert_eq!(
            error_code(r#"{"error":"authorization_pending","error_description":"x"}"#).as_deref(),
            Some("authorization_pending")
        );
        // Non-JSON (an HTML error page from a proxy) must not panic.
        assert_eq!(error_code("<html>nope</html>"), None);
        assert_eq!(error_code(""), None);
    }

    #[test]
    fn invalid_grant_is_detected_from_an_error_chain() {
        let body = r#"{"error":"invalid_grant"}"#;
        assert!(is_invalid_grant(&anyhow::anyhow!(
            "GitLab token request failed (400): {body}"
        )));
        assert!(!is_invalid_grant(&anyhow::anyhow!(
            "GitLab token request failed (500): server on fire"
        )));
    }

    #[test]
    fn the_keyring_key_is_per_instance_and_slash_insensitive() {
        assert_eq!(
            keyring_key("https://gitlab.example.com"),
            "gitlab-oauth:https://gitlab.example.com"
        );
        assert_eq!(
            keyring_key("https://gitlab.example.com/"),
            keyring_key("https://gitlab.example.com")
        );
    }

    #[test]
    fn stored_tokens_round_trip_through_json() {
        let original = creds();
        let stored = StoredTokens {
            access_token: original.access_token.clone(),
            refresh_token: original.refresh_token.clone(),
            expires_at: original.expires_at.to_rfc3339(),
            base_url: original.base_url.clone(),
            client_id: original.client_id.clone(),
            client_secret: None,
        };
        let json = serde_json::to_string(&stored).expect("serializes");
        let back: StoredTokens = json5::from_str(&json).expect("parses");
        let parsed = parse_stored(back, OAuthStore::File).expect("converts");
        assert_eq!(parsed.access_token, original.access_token);
        assert_eq!(parsed.refresh_token, original.refresh_token);
        assert_eq!(parsed.base_url, original.base_url);
        // Round-tripping through RFC3339 keeps sub-second drift out of the way.
        assert_eq!(
            parsed.expires_at.timestamp(),
            original.expires_at.timestamp()
        );
    }

    #[test]
    fn a_tokens_file_holds_one_entry_per_instance() {
        let file = r#"[
            { access_token: "a1", refresh_token: "r1", expires_at: "2026-01-01T00:00:00+00:00",
              base_url: "https://gitlab.one", client_id: "c1" },
            { access_token: "a2", refresh_token: "r2", expires_at: "2026-01-01T00:00:00+00:00",
              base_url: "https://gitlab.two", client_id: "c2" },
        ]"#;
        let all: Vec<StoredTokens> = json5::from_str(file).expect("parses");
        assert_eq!(all.len(), 2);
        let two = all
            .into_iter()
            .find(|t| t.base_url == "https://gitlab.two")
            .expect("finds the second instance");
        assert_eq!(two.access_token, "a2");
        // client_secret is optional — a public app has none.
        assert!(two.client_secret.is_none());
    }

    #[test]
    fn a_device_authorization_falls_back_to_sane_polling_defaults() {
        let minimal: DeviceAuthorization = serde_json::from_str(
            r#"{"device_code":"dc","user_code":"ABCD-EFGH",
                "verification_uri":"https://gitlab.example.com/oauth/device"}"#,
        )
        .expect("parses");
        assert_eq!(minimal.interval, 5);
        assert_eq!(minimal.expires_in, 300);
        assert!(minimal.verification_uri_complete.is_none());

        let full: DeviceAuthorization = serde_json::from_str(
            r#"{"device_code":"dc","user_code":"ABCD-EFGH",
                "verification_uri":"https://gitlab.example.com/oauth/device",
                "verification_uri_complete":"https://gitlab.example.com/oauth/device?user_code=ABCD-EFGH",
                "expires_in":600,"interval":10}"#,
        )
        .expect("parses");
        assert_eq!(full.interval, 10);
        assert_eq!(full.expires_in, 600);
        assert!(
            full.verification_uri_complete
                .expect("present")
                .contains("user_code=")
        );
    }

    #[test]
    fn device_polling_keeps_waiting_until_the_user_acts() {
        assert_eq!(
            device_poll_outcome(Some("authorization_pending")),
            PollOutcome::KeepWaiting
        );
        assert_eq!(
            device_poll_outcome(Some("slow_down")),
            PollOutcome::SlowDown
        );
    }

    #[test]
    fn device_polling_stops_on_a_terminal_answer() {
        assert_eq!(
            device_poll_outcome(Some("expired_token")),
            PollOutcome::Expired
        );
        assert_eq!(
            device_poll_outcome(Some("access_denied")),
            PollOutcome::Denied
        );
        // An unrecognised or absent error must not become an infinite poll.
        assert_eq!(
            device_poll_outcome(Some("invalid_client")),
            PollOutcome::Failed
        );
        assert_eq!(device_poll_outcome(None), PollOutcome::Failed);
    }

    #[test]
    fn slow_down_lengthens_the_interval_by_the_rfc_step() {
        // RFC 8628 §3.5: add 5 seconds each time the server says slow_down.
        assert_eq!(SLOW_DOWN_STEP, Duration::from_secs(5));
        let mut wait = Duration::from_secs(5);
        wait += SLOW_DOWN_STEP;
        assert_eq!(wait, Duration::from_secs(10));
    }

    #[test]
    fn error_snippets_stay_short_and_say_something() {
        assert_eq!(snippet("   "), "(empty response body)");
        assert_eq!(snippet(" boom "), "boom");
        let long = "x".repeat(MAX_BODY_SNIPPET + 50);
        let short = snippet(&long);
        assert!(short.ends_with('…'));
        assert_eq!(short.chars().count(), MAX_BODY_SNIPPET + 1);
    }
}
