use std::io::{self, Write};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{Duration as ChronoDuration, Utc};
use rand::Rng;
use sha2::{Digest, Sha256};

use crate::jira::auth::{OAuthCredentials, OAuthStore};

const AUTH_URL: &str = "https://auth.atlassian.com/authorize";
const TOKEN_URL: &str = "https://auth.atlassian.com/oauth/token";
const RESOURCES_URL: &str = "https://api.atlassian.com/oauth/token/accessible-resources";

const SCOPES: &str = "read:jira-work write:jira-work read:jira-user offline_access";

/// Run the full OAuth 2.0 (3LO) authorization flow with PKCE.
///
/// Opens the user's browser for Atlassian authorization, listens for the
/// callback on a local HTTP server, exchanges the code for tokens, and
/// resolves the cloud ID.
#[allow(clippy::too_many_lines)]
pub fn run_oauth_flow(
    client_id: &str,
    client_secret: &str,
    store: OAuthStore,
) -> Result<OAuthCredentials> {
    // 1. Start local HTTP server on a fixed port (must match the callback URL
    //    registered in the Atlassian Developer Console).
    let server = tiny_http::Server::http("127.0.0.1:19872")
        .map_err(|e| anyhow::anyhow!("Failed to start local HTTP server on port 19872: {e}"))?;
    let redirect_uri = "http://localhost:19872/callback".to_string();

    // 2. Generate PKCE code_verifier and code_challenge (S256).
    let code_verifier = generate_code_verifier();
    let code_challenge = generate_code_challenge(&code_verifier);

    // 3. Generate random state for CSRF protection.
    let state = generate_state();

    // 4. Build authorization URL.
    let auth_url = format!(
        "{AUTH_URL}?\
         audience=api.atlassian.com&\
         client_id={client_id}&\
         scope={scopes}&\
         redirect_uri={redirect_uri}&\
         state={state}&\
         response_type=code&\
         prompt=consent&\
         code_challenge={code_challenge}&\
         code_challenge_method=S256",
        scopes = urlencoded(SCOPES),
        redirect_uri = urlencoded(&redirect_uri),
    );

    // 5. Open browser.
    println!("Opening browser for Atlassian authorization...");
    if open::that(&auth_url).is_err() {
        println!("Could not open browser automatically.");
    }
    println!("If the browser didn't open, visit this URL:");
    println!("  {auth_url}");
    println!();
    println!("Waiting for authorization (up to 2 minutes)...");

    // 6. Wait for the callback.
    let request = server
        .recv_timeout(Duration::from_secs(120))
        .context("Error waiting for OAuth callback")?
        .ok_or_else(|| anyhow::anyhow!(
            "Timed out waiting for authorization.\n\
             Make sure you complete the authorization in your browser within 2 minutes.\n\
             Run `do-next auth` to try again."
        ))?;

    let url = request.url().to_string();
    let (code, callback_state) = parse_callback_params(&url)?;

    if callback_state != state {
        bail!("OAuth state mismatch — possible CSRF attack. Run `do-next auth` to try again.");
    }

    // Respond to the browser.
    let response = tiny_http::Response::from_string(
        "<html><body><h2>Authorization complete!</h2>\
         <p>You can close this tab and return to the terminal.</p></body></html>",
    )
    .with_header(
        "Content-Type: text/html"
            .parse::<tiny_http::Header>()
            .expect("static header is valid"),
    );
    let _ = request.respond(response);
    drop(server); // Release the listener before making outbound HTTP calls.

    println!("Authorization received. Exchanging for tokens...");

    // 7–8. Exchange code for tokens and fetch accessible resources.
    // Run in a dedicated thread with its own tokio runtime to avoid
    // conflicts with the main runtime (which we're called from synchronously).
    let client_id_owned = client_id.to_string();
    let client_secret_owned = client_secret.to_string();
    let (token_data, resources) = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("Failed to build HTTP runtime")?;

        rt.block_on(async {
            let http = reqwest::Client::new();

            // 7. Token exchange.
            let token_resp = http
                .post(TOKEN_URL)
                .json(&serde_json::json!({
                    "grant_type": "authorization_code",
                    "client_id": client_id_owned,
                    "client_secret": client_secret_owned,
                    "code": code,
                    "redirect_uri": redirect_uri,
                    "code_verifier": code_verifier,
                }))
                .send()
                .await
                .context("Failed to exchange authorization code for tokens")?;

            if !token_resp.status().is_success() {
                let status = token_resp.status();
                let body = token_resp.text().await.unwrap_or_default();
                bail!("Token exchange failed ({status}): {body}");
            }

            let token_data: TokenResponse = token_resp
                .json()
                .await
                .context("Failed to parse token response")?;

            // 8. Get accessible resources to find the cloud ID.
            let resources_resp = http
                .get(RESOURCES_URL)
                .bearer_auth(&token_data.access_token)
                .send()
                .await
                .context("Failed to fetch accessible resources")?;

            if !resources_resp.status().is_success() {
                let status = resources_resp.status();
                let body = resources_resp.text().await.unwrap_or_default();
                bail!("Failed to fetch accessible Jira sites ({status}): {body}");
            }

            let resources: Vec<AccessibleResource> = resources_resp
                .json()
                .await
                .context("Failed to parse accessible resources")?;

            Ok((token_data, resources))
        })
    })
    .join()
    .map_err(|_| anyhow::anyhow!("Token exchange thread panicked"))??;

    let expires_at = Utc::now() + ChronoDuration::seconds(token_data.expires_in);

    if resources.is_empty() {
        bail!(
            "No Jira Cloud sites found for this account.\n\
             Make sure you have access to at least one Jira Cloud site."
        );
    }

    let resource = if resources.len() == 1 {
        &resources[0]
    } else {
        println!();
        println!("Multiple Jira Cloud sites found:");
        for (i, r) in resources.iter().enumerate() {
            println!("  [{}] {} ({})", i + 1, r.name, r.url);
        }
        print!("Choose a site [1-{}]: ", resources.len());
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let idx: usize = input
            .trim()
            .parse::<usize>()
            .context("Invalid selection")?
            .checked_sub(1)
            .context("Selection out of range")?;
        resources
            .get(idx)
            .ok_or_else(|| anyhow::anyhow!("Selection out of range"))?
    };

    println!("Using Jira site: {} ({})", resource.name, resource.url);

    let creds = OAuthCredentials {
        access_token: token_data.access_token,
        refresh_token: token_data.refresh_token,
        expires_at,
        cloud_id: resource.id.clone(),
        client_id: client_id.to_string(),
        client_secret: client_secret.to_string(),
        store,
    };

    save_oauth_tokens(&creds)?;
    println!("OAuth tokens saved.");

    Ok(creds)
}

/// Refresh an expired access token using the refresh token.
pub async fn refresh_access_token(creds: &OAuthCredentials) -> Result<OAuthCredentials> {
    let client = reqwest::Client::new();
    let resp = client
        .post(TOKEN_URL)
        .json(&serde_json::json!({
            "grant_type": "refresh_token",
            "client_id": creds.client_id,
            "client_secret": creds.client_secret,
            "refresh_token": creds.refresh_token,
        }))
        .send()
        .await
        .context("Failed to refresh OAuth token")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!(
            "OAuth token refresh failed ({status}): {body}\n\
             Your session may have expired. Run `do-next auth` to re-authenticate."
        );
    }

    let token_data: TokenResponse = resp
        .json()
        .await
        .context("Failed to parse refresh token response")?;

    let expires_at = Utc::now() + ChronoDuration::seconds(token_data.expires_in);

    Ok(OAuthCredentials {
        access_token: token_data.access_token,
        refresh_token: token_data.refresh_token,
        expires_at,
        cloud_id: creds.cloud_id.clone(),
        client_id: creds.client_id.clone(),
        client_secret: creds.client_secret.clone(),
        store: creds.store.clone(),
    })
}

/// Save OAuth tokens using the store indicated by `creds.store`.
pub fn save_oauth_tokens(creds: &OAuthCredentials) -> Result<()> {
    let data = StoredTokens {
        access_token: creds.access_token.clone(),
        refresh_token: creds.refresh_token.clone(),
        expires_at: creds.expires_at.to_rfc3339(),
        cloud_id: creds.cloud_id.clone(),
        client_id: creds.client_id.clone(),
        client_secret: creds.client_secret.clone(),
    };
    let json = serde_json::to_string(&data).context("Failed to serialize OAuth tokens")?;

    match creds.store {
        OAuthStore::Keyring => {
            let key = format!("oauth:{}", creds.cloud_id);
            let entry = keyring::Entry::new("do-next", &key)
                .context("Failed to access keyring for OAuth tokens")?;
            entry
                .set_password(&json)
                .context("Failed to store OAuth tokens in keyring")?;
            // Store an index entry so load_oauth_tokens can find the cloud_id.
            let index = keyring::Entry::new("do-next", "oauth:_index")
                .context("Failed to access keyring for OAuth index")?;
            index
                .set_password(&creds.cloud_id)
                .context("Failed to store OAuth index in keyring")?;
            log::debug!("OAuth tokens saved to keyring (key={key})");
        }
        OAuthStore::File => {
            let dir = dirs::config_dir()
                .context("Cannot determine config directory")?
                .join("do-next");
            std::fs::create_dir_all(&dir)?;
            let path = dir.join("oauth_tokens.json5");
            std::fs::write(&path, &json)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            }
            log::debug!("OAuth tokens saved to {}", path.display());
        }
    }
    Ok(())
}

/// Load OAuth tokens, trying keyring first then file.
pub fn load_oauth_tokens() -> Result<Option<OAuthCredentials>> {
    // Try keyring — we don't know the cloud_id yet, so scan for any oauth:* entry.
    if let Some(creds) = load_oauth_from_keyring()? {
        return Ok(Some(creds));
    }
    load_oauth_from_file()
}

fn load_oauth_from_keyring() -> Result<Option<OAuthCredentials>> {
    // We need the cloud_id to build the key, but we don't know it before loading.
    // Try loading from a well-known probe key first; if the user has tokens in the
    // keyring, we stored a pointer under "oauth:_index" with the cloud_id.
    let index_entry = keyring::Entry::new("do-next", "oauth:_index")
        .context("Failed to access keyring")?;
    let cloud_id = match index_entry.get_password() {
        Ok(id) => id,
        Err(keyring::Error::NoEntry) => return Ok(None),
        Err(e) => {
            log::debug!("keyring index lookup failed: {e}");
            return Ok(None);
        }
    };

    let key = format!("oauth:{cloud_id}");
    let entry = keyring::Entry::new("do-next", &key)
        .context("Failed to access keyring")?;
    let json = match entry.get_password() {
        Ok(s) => s,
        Err(keyring::Error::NoEntry) => return Ok(None),
        Err(e) => {
            log::debug!("keyring OAuth token lookup failed: {e}");
            return Ok(None);
        }
    };

    let stored: StoredTokens =
        serde_json::from_str(&json).context("Failed to parse OAuth tokens from keyring")?;
    parse_stored_tokens(stored, OAuthStore::Keyring).map(Some)
}

fn load_oauth_from_file() -> Result<Option<OAuthCredentials>> {
    let path = dirs::config_dir()
        .context("Cannot determine config directory")?
        .join("do-next")
        .join("oauth_tokens.json5");

    if !path.exists() {
        return Ok(None);
    }

    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let stored: StoredTokens =
        json5::from_str(&content).context("Failed to parse oauth_tokens.json5")?;
    parse_stored_tokens(stored, OAuthStore::File).map(Some)
}

fn parse_stored_tokens(stored: StoredTokens, store: OAuthStore) -> Result<OAuthCredentials> {
    let expires_at = chrono::DateTime::parse_from_rfc3339(&stored.expires_at)
        .context("Failed to parse expires_at timestamp")?
        .with_timezone(&Utc);

    Ok(OAuthCredentials {
        access_token: stored.access_token,
        refresh_token: stored.refresh_token,
        expires_at,
        cloud_id: stored.cloud_id,
        client_id: stored.client_id,
        client_secret: stored.client_secret,
        store,
    })
}

// --- Internal helpers ---

#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
}

#[derive(serde::Deserialize)]
struct AccessibleResource {
    id: String,
    name: String,
    url: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredTokens {
    access_token: String,
    refresh_token: String,
    expires_at: String,
    cloud_id: String,
    client_id: String,
    client_secret: String,
}

fn generate_code_verifier() -> String {
    let bytes: [u8; 32] = rand::rng().random();
    URL_SAFE_NO_PAD.encode(bytes)
}

fn generate_code_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

fn generate_state() -> String {
    let bytes: [u8; 16] = rand::rng().random();
    hex::encode(&bytes)
}

/// Minimal percent-encoding for URL query values.
fn urlencoded(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push(char::from(b"0123456789ABCDEF"[(b >> 4) as usize]));
                out.push(char::from(b"0123456789ABCDEF"[(b & 0x0F) as usize]));
            }
        }
    }
    out
}

/// Parse `code` and `state` from the OAuth callback URL query string.
fn parse_callback_params(url: &str) -> Result<(String, String)> {
    let query = url
        .split_once('?')
        .map_or("", |(_, q)| q);

    let mut code = None;
    let mut state = None;

    for pair in query.split('&') {
        if let Some((key, value)) = pair.split_once('=') {
            match key {
                "code" => code = Some(value.to_string()),
                "state" => state = Some(value.to_string()),
                _ => {}
            }
        }
    }

    let code = code.ok_or_else(|| anyhow::anyhow!(
        "Authorization callback missing 'code' parameter.\n\
         The authorization may have been denied. Run `do-next auth` to try again."
    ))?;
    let state = state.ok_or_else(|| anyhow::anyhow!("Authorization callback missing 'state' parameter"))?;

    Ok((code, state))
}

/// Encode bytes as hexadecimal string.
mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        let mut s = String::with_capacity(bytes.len() * 2);
        for &b in bytes {
            s.push(char::from(b"0123456789abcdef"[(b >> 4) as usize]));
            s.push(char::from(b"0123456789abcdef"[(b & 0x0F) as usize]));
        }
        s
    }
}
