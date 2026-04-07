use chrono::{DateTime, Utc};

#[derive(Debug, Clone)]
pub enum Auth {
    Basic(BasicCredentials),
    OAuth(OAuthCredentials),
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
