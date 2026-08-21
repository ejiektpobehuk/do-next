//! GitLab authentication: a personal access token or OAuth credentials, behind
//! one handle the client can refresh through.
//!
//! Mirrors [`crate::atlassian::auth`]. The header differs per mode and cannot be
//! unified: GitLab's `PRIVATE-TOKEN` looks tokens up in the personal-access
//! token table, so it rejects an OAuth token outright.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use reqwest::RequestBuilder;
use tokio::sync::RwLock;

use crate::atlassian::auth::OAuthStore;

/// Refresh once the access token is this close to expiring.
const REFRESH_SKEW: chrono::TimeDelta = chrono::TimeDelta::seconds(60);

#[derive(Debug, Clone)]
pub enum GitlabAuth {
    /// A personal access token, sent as `PRIVATE-TOKEN`.
    Token(String),
    /// OAuth credentials, sent as `Authorization: Bearer`.
    OAuth(GitlabOAuthCredentials),
}

#[derive(Debug, Clone)]
pub struct GitlabOAuthCredentials {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: DateTime<Utc>,
    pub client_id: String,
    /// Only set for a confidential app. The flows we drive use PKCE with a
    /// public client, where GitLab issues no secret at all.
    pub client_secret: Option<String>,
    /// Instance this token belongs to — also its key in the token store.
    pub base_url: String,
    /// Where the tokens live, so a refresh can save back to the same place.
    pub store: OAuthStore,
}

/// Refresh the OAuth access token if it expires within the next minute.
/// No-op for a personal access token.
///
/// GitLab's refresh tokens are single-use, which makes this more delicate than
/// the Atlassian equivalent: two `do-next` processes sharing one store can each
/// try to spend the same refresh token. So before spending ours we re-read what
/// is on disk, and if a concurrent process already rotated it we adopt their
/// result instead of burning a token that is no longer current.
// The write guard deliberately spans the whole refresh: it is what stops two
// tasks in this process from each spending the same single-use refresh token.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the guard must outlive the refresh to serialize it"
)]
pub async fn maybe_refresh(auth: &RwLock<GitlabAuth>) -> Result<()> {
    let needs_refresh = {
        let auth = auth.read().await;
        match &*auth {
            GitlabAuth::OAuth(o) => o.expires_at - Utc::now() < REFRESH_SKEW,
            GitlabAuth::Token(_) => false,
        }
    };
    if !needs_refresh {
        return Ok(());
    }

    let mut auth = auth.write().await;
    // Another task may have refreshed while we waited for the write lock.
    let GitlabAuth::OAuth(creds) = &*auth else {
        return Ok(());
    };
    if creds.expires_at - Utc::now() >= REFRESH_SKEW {
        return Ok(());
    }

    // A different *process* may have refreshed. Re-read before spending ours.
    if let Some(fresh) = super::oauth::reload(creds)
        && fresh.expires_at - Utc::now() >= REFRESH_SKEW
    {
        log::debug!("GitLab OAuth token was refreshed elsewhere; adopting it");
        *auth = GitlabAuth::OAuth(fresh);
        return Ok(());
    }

    log::debug!("GitLab OAuth token expiring soon, refreshing");
    let refreshed = super::oauth::refresh_access_token(creds).await?;
    super::oauth::save_oauth_tokens(&refreshed)
        .context("Failed to persist the refreshed GitLab OAuth tokens")?;
    *auth = GitlabAuth::OAuth(refreshed);
    Ok(())
}

/// Attach the current credentials to a request.
pub async fn apply(auth: &RwLock<GitlabAuth>, req: RequestBuilder) -> RequestBuilder {
    let auth = auth.read().await;
    match &*auth {
        GitlabAuth::Token(token) => req.header("PRIVATE-TOKEN", token),
        GitlabAuth::OAuth(creds) => req.bearer_auth(&creds.access_token),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oauth_creds(expires_in_secs: i64) -> GitlabOAuthCredentials {
        GitlabOAuthCredentials {
            access_token: "access".into(),
            refresh_token: "refresh".into(),
            expires_at: Utc::now() + chrono::TimeDelta::seconds(expires_in_secs),
            client_id: "cid".into(),
            client_secret: None,
            base_url: "https://gitlab.example.com".into(),
            store: OAuthStore::File,
        }
    }

    /// The header a request would carry, as a readable string.
    async fn header_of(auth: GitlabAuth) -> String {
        let lock = RwLock::new(auth);
        let req = reqwest::Client::new().get("https://gitlab.example.com/api/v4/user");
        let req = apply(&lock, req).await.build().expect("request builds");
        let headers = req.headers();
        if let Some(private) = headers.get("PRIVATE-TOKEN") {
            return format!("PRIVATE-TOKEN: {}", private.to_str().expect("ascii"));
        }
        if let Some(authorization) = headers.get(reqwest::header::AUTHORIZATION) {
            return format!("Authorization: {}", authorization.to_str().expect("ascii"));
        }
        "(no credential header)".to_string()
    }

    #[tokio::test]
    async fn a_personal_access_token_goes_out_as_private_token() {
        // GitLab looks PRIVATE-TOKEN values up in the personal-access-token
        // table, so this header is the only one that works for a PAT.
        assert_eq!(
            header_of(GitlabAuth::Token("glpat-xyz".into())).await,
            "PRIVATE-TOKEN: glpat-xyz"
        );
    }

    #[tokio::test]
    async fn an_oauth_token_goes_out_as_a_bearer() {
        // ...and PRIVATE-TOKEN rejects OAuth tokens outright, so the header
        // must switch with the mode rather than being unified.
        assert_eq!(
            header_of(GitlabAuth::OAuth(oauth_creds(3600))).await,
            "Authorization: Bearer access"
        );
    }

    #[tokio::test]
    async fn a_personal_access_token_is_never_refreshed() {
        // No network is available in tests, so this passing at all proves no
        // refresh was attempted.
        let auth = RwLock::new(GitlabAuth::Token("glpat-xyz".into()));
        maybe_refresh(&auth).await.expect("a PAT needs no refresh");
    }

    #[tokio::test]
    async fn an_unexpired_oauth_token_is_left_alone() {
        let auth = RwLock::new(GitlabAuth::OAuth(oauth_creds(3600)));
        maybe_refresh(&auth).await.expect("no refresh needed");
        let GitlabAuth::OAuth(creds) = &*auth.read().await else {
            panic!("still OAuth");
        };
        assert_eq!(creds.access_token, "access");
    }

    #[test]
    fn the_refresh_skew_leaves_room_to_finish_a_request() {
        // Refreshing exactly at expiry would race the request in flight.
        assert_eq!(REFRESH_SKEW, chrono::TimeDelta::seconds(60));
        let nearly_expired = oauth_creds(30);
        assert!(nearly_expired.expires_at - Utc::now() < REFRESH_SKEW);
        let fresh = oauth_creds(3600);
        assert!(fresh.expires_at - Utc::now() >= REFRESH_SKEW);
    }
}
