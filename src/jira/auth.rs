use anyhow::Result;
use chrono::{DateTime, Utc};
use reqwest::RequestBuilder;
use tokio::sync::RwLock;

#[derive(Debug, Clone)]
pub enum Auth {
    Basic(BasicCredentials),
    OAuth(OAuthCredentials),
}

/// Refresh the OAuth access token if it expires within the next minute.
/// No-op for basic auth. Shared by the Jira and Confluence clients (which
/// may hold the same `Arc<RwLock<Auth>>` for one Atlassian site).
pub async fn maybe_refresh(auth: &RwLock<Auth>) -> Result<()> {
    let needs_refresh = {
        let auth = auth.read().await;
        match &*auth {
            Auth::OAuth(o) => o.expires_at - Utc::now() < chrono::Duration::seconds(60),
            Auth::Basic(_) => false,
        }
    };

    if needs_refresh {
        let mut auth = auth.write().await;
        // Double-check after acquiring write lock (another task may have refreshed).
        if let Auth::OAuth(o) = &*auth
            && o.expires_at - Utc::now() < chrono::Duration::seconds(60)
        {
            log::debug!("OAuth token expiring soon, refreshing");
            let refreshed = crate::jira::oauth::refresh_access_token(o).await?;
            crate::jira::oauth::save_oauth_tokens(&refreshed)?;
            *auth = Auth::OAuth(refreshed);
        }
    }

    Ok(())
}

/// Attach the current credentials to a request.
pub async fn apply(auth: &RwLock<Auth>, req: RequestBuilder) -> RequestBuilder {
    let auth = auth.read().await;
    match &*auth {
        Auth::Basic(creds) => req.basic_auth(&creds.email, Some(&creds.api_token)),
        Auth::OAuth(creds) => req.bearer_auth(&creds.access_token),
    }
}

#[derive(Debug, Clone)]
pub struct BasicCredentials {
    pub email: String,
    pub api_token: String,
}

#[derive(Debug, Clone)]
pub struct OAuthCredentials {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: DateTime<Utc>,
    pub cloud_id: String,
    pub client_id: String,
    pub client_secret: String,
    /// Where to persist tokens (keyring vs file). Carried so that token
    /// refresh in `JiraClient` can save back to the same store.
    pub store: OAuthStore,
}

#[derive(Debug, Clone, Default)]
pub enum OAuthStore {
    Keyring,
    #[default]
    File,
}
