use anyhow::{Context, Result, anyhow, bail};
use std::process::Command;

use crate::config::types::{JiraConfig, ResolvedGitlab, ResolvedGrafana};
use crate::gitlab::auth::GitlabAuth;
use crate::jira::auth::{Auth, BasicCredentials};
use crate::jira::oauth;

/// Resolve Jira authentication (basic auth or OAuth).
///
/// If `auth_method` is `"oauth"`, loads saved OAuth tokens.
/// Otherwise falls back to basic auth (email + API token).
///
/// Email precedence: `DO_NEXT_JIRA_EMAIL` env → `config.jira.email`.
///
/// API token precedence:
/// 1. `DO_NEXT_JIRA_API_TOKEN` env
/// 2. `credential_command` (shell exec, stdout = API token)
/// 3. OS keyring
/// 4. credentials file (`~/.config/do-next/credentials.json5`)
pub fn resolve_auth(jira: &JiraConfig) -> Result<Auth> {
    if jira.auth_method.as_deref() == Some("oauth") {
        return resolve_oauth(jira);
    }
    resolve_basic(jira)
}

/// Resolve Confluence authentication from the effective Confluence connection
/// config (a `JiraConfig`-shaped value). Same resolution as [`resolve_auth`],
/// except `DO_NEXT_CONFLUENCE_API_TOKEN` takes precedence over everything.
pub fn resolve_confluence_auth(conf: &JiraConfig) -> Result<Auth> {
    if conf.auth_method.as_deref() == Some("oauth") {
        return resolve_oauth(conf);
    }
    let email = resolve_email(conf)?;
    if let Ok(token) = std::env::var("DO_NEXT_CONFLUENCE_API_TOKEN") {
        log::debug!("credentials: using DO_NEXT_CONFLUENCE_API_TOKEN env var");
        return Ok(Auth::Basic(BasicCredentials {
            email,
            api_token: token,
        }));
    }
    let api_token = resolve_api_token(conf)?;
    Ok(Auth::Basic(BasicCredentials { email, api_token }))
}

fn resolve_oauth(jira: &JiraConfig) -> Result<Auth> {
    match oauth::load_oauth_tokens()? {
        Some(mut creds) => {
            // Stored tokens embed the client id/secret they were minted with.
            // Prefer the config's current values (e.g. a rotated secret in a
            // company config repo) so token refresh heals after rotation
            // without a manual re-auth.
            if let (Some(id), Some(secret)) = (&jira.oauth_client_id, &jira.oauth_client_secret) {
                creds.client_id.clone_from(id);
                creds.client_secret.clone_from(secret);
            }
            Ok(Auth::OAuth(creds))
        }
        None => bail!(
            "No OAuth tokens found.\n\
             Run `do-next auth` to authenticate with your browser."
        ),
    }
}

fn resolve_basic(jira: &JiraConfig) -> Result<Auth> {
    let email = resolve_email(jira)?;
    let api_token = resolve_api_token(jira)?;
    Ok(Auth::Basic(BasicCredentials { email, api_token }))
}

fn resolve_email(jira: &JiraConfig) -> Result<String> {
    if let Ok(email) = std::env::var("DO_NEXT_JIRA_EMAIL") {
        log::debug!("credentials: using DO_NEXT_JIRA_EMAIL env var");
        return Ok(email);
    }
    if let Some(email) = &jira.email {
        log::debug!("credentials: using email from config");
        return Ok(email.clone());
    }
    bail!(
        "No Jira email configured.\n\
         Set DO_NEXT_JIRA_EMAIL env var or add `email` to your Jira config.\n\
         Run `do-next auth` to reconfigure."
    )
}

fn resolve_api_token(jira: &JiraConfig) -> Result<String> {
    // 1. Environment variable
    if let Ok(token) = std::env::var("DO_NEXT_JIRA_API_TOKEN") {
        log::debug!("credentials: using DO_NEXT_JIRA_API_TOKEN env var");
        return Ok(token);
    }

    // 2. credential_command
    if let Some(cmd) = &jira.credential_command {
        return run_credential_command(cmd);
    }

    // 3. Keyring
    if jira.credential_store.as_deref() == Some("keyring") {
        let key = jira.credential_key.as_deref().unwrap_or(&jira.base_url);
        let hints = KeyringHints {
            env_var: "DO_NEXT_JIRA_API_TOKEN",
            refresh: "Re-run `do-next auth` to store a fresh API token",
        };
        if let Some(secret) = keyring_lookup(key, &hints)? {
            return Ok(secret);
        }
    }

    // 4. Credentials file
    log::debug!("credentials: checking credentials file");
    if let Some(token) = read_credentials_file()?
        .and_then(|f| f.jira)
        .and_then(|j| j.api_token)
    {
        log::debug!("credentials: loaded from credentials file");
        return Ok(token);
    }

    bail!(
        "No Jira API token found.\n\
         Set DO_NEXT_JIRA_API_TOKEN env var or run `do-next auth` to configure credentials."
    )
}

/// Resolve the Grafana `OnCall` API token for a team's on-duty check.
/// Same precedence as the Jira token: env → `credential_command` → keyring →
/// credentials file. `Ok(None)` means no token is configured anywhere (the
/// caller may offer interactive setup); `Err` is a hard failure (locked
/// keyring, failing command). This token is created by the user in their
/// Grafana IRM/`OnCall` profile.
pub fn resolve_grafana_token(grafana: &ResolvedGrafana) -> Result<Option<String>> {
    if let Ok(token) = std::env::var("DO_NEXT_GRAFANA_TOKEN") {
        log::debug!("credentials: using DO_NEXT_GRAFANA_TOKEN env var");
        return Ok(Some(token));
    }

    if let Some(cmd) = &grafana.credential_command {
        return run_credential_command(cmd).map(Some);
    }

    if grafana.credential_store.as_deref() == Some("keyring") {
        let key = grafana
            .credential_key
            .as_deref()
            .unwrap_or(&grafana.oncall_api_url);
        let hints = KeyringHints {
            env_var: "DO_NEXT_GRAFANA_TOKEN",
            refresh: "Store the OnCall API token in the keyring again",
        };
        if let Some(secret) = keyring_lookup(key, &hints)? {
            return Ok(Some(secret));
        }
    }

    log::debug!("credentials: checking credentials file for grafana token");
    if let Some(token) = read_credentials_file()?
        .and_then(|f| f.grafana)
        .and_then(|g| g.api_token)
    {
        log::debug!("credentials: loaded grafana token from credentials file");
        return Ok(Some(token));
    }

    Ok(None)
}

/// Resolve the GitLab personal access token for a team's merge-request
/// sources. Same precedence and error shape as [`resolve_grafana_token`]:
/// env → `credential_command` → keyring → credentials file. `Ok(None)` means
/// no token is configured anywhere (the caller may offer interactive setup);
/// `Err` is a hard failure (locked keyring, failing command).
pub fn resolve_gitlab_token(gitlab: &ResolvedGitlab) -> Result<Option<String>> {
    if let Ok(token) = std::env::var("DO_NEXT_GITLAB_TOKEN") {
        log::debug!("credentials: using DO_NEXT_GITLAB_TOKEN env var");
        return Ok(Some(token));
    }

    if let Some(cmd) = &gitlab.credential_command {
        return run_credential_command(cmd).map(Some);
    }

    if gitlab.credential_store.as_deref() == Some("keyring") {
        let key = gitlab.credential_key.as_deref().unwrap_or(&gitlab.base_url);
        let hints = KeyringHints {
            env_var: "DO_NEXT_GITLAB_TOKEN",
            refresh: "Store the GitLab personal access token in the keyring again",
        };
        if let Some(secret) = keyring_lookup(key, &hints)? {
            return Ok(Some(secret));
        }
    }

    log::debug!("credentials: checking credentials file for gitlab token");
    if let Some(token) = read_credentials_file()?
        .and_then(|f| f.gitlab)
        .and_then(|g| g.api_token)
    {
        log::debug!("credentials: loaded gitlab token from credentials file");
        return Ok(Some(token));
    }

    Ok(None)
}

/// Resolve GitLab authentication for a team's merge-request sources.
///
/// `Ok(None)` means nothing is configured anywhere (the caller may offer
/// interactive setup); `Err` is a hard failure (locked keyring, failing
/// command, expired OAuth session).
///
/// An explicit `DO_NEXT_GITLAB_TOKEN` wins over everything, *including* an
/// `auth_method: "oauth"` config. That variable holds a personal access token,
/// which must go out as `PRIVATE-TOKEN`; deciding the header from a config flag
/// while taking the token from the environment is what made `glab` send PATs
/// under a `Bearer` header (gitlab-org/cli#8482). Here the token and the
/// knowledge of what kind of token it is always travel together.
pub fn resolve_gitlab_auth(gitlab: &ResolvedGitlab) -> Result<Option<GitlabAuth>> {
    match gitlab_auth_source(
        std::env::var("DO_NEXT_GITLAB_TOKEN").ok(),
        gitlab.uses_oauth(),
    ) {
        GitlabAuthSource::EnvToken(token) => {
            log::debug!("credentials: using DO_NEXT_GITLAB_TOKEN env var");
            Ok(Some(GitlabAuth::Token(token)))
        }
        GitlabAuthSource::Oauth => resolve_gitlab_oauth(gitlab),
        GitlabAuthSource::StoredToken => Ok(resolve_gitlab_token(gitlab)?.map(GitlabAuth::Token)),
    }
}

/// Which GitLab credential source wins.
#[derive(Debug, PartialEq, Eq)]
enum GitlabAuthSource {
    /// A personal access token from the environment.
    EnvToken(String),
    /// Stored OAuth credentials for the instance.
    Oauth,
    /// A personal access token from the command/keyring/file chain.
    StoredToken,
}

/// Decide the credential source. Pure, so the precedence rule is testable
/// without mutating the process environment.
fn gitlab_auth_source(env_token: Option<String>, uses_oauth: bool) -> GitlabAuthSource {
    // An empty variable is someone unsetting it awkwardly, not a token.
    if let Some(token) = env_token.filter(|t| !t.is_empty()) {
        return GitlabAuthSource::EnvToken(token);
    }
    if uses_oauth {
        return GitlabAuthSource::Oauth;
    }
    GitlabAuthSource::StoredToken
}

/// Load stored OAuth credentials for one instance.
///
/// The config's client id/secret override whatever the stored tokens were
/// minted with, so a rotated company app heals token refresh without a manual
/// re-auth — the same trick [`resolve_oauth`] plays for Jira.
fn resolve_gitlab_oauth(gitlab: &ResolvedGitlab) -> Result<Option<GitlabAuth>> {
    // No stored tokens is "not set up yet", not a failure: the caller offers
    // an interactive sign-in, exactly as it does for a missing token.
    let Some(mut creds) = crate::gitlab::oauth::load_oauth_tokens(&gitlab.base_url)? else {
        return Ok(None);
    };
    if let Some(id) = &gitlab.oauth_client_id {
        creds.client_id.clone_from(id);
    }
    if gitlab.oauth_client_secret.is_some() {
        creds.client_secret.clone_from(&gitlab.oauth_client_secret);
    }
    Ok(Some(GitlabAuth::OAuth(creds)))
}

/// Path of the shared credentials file (`~/.config/do-next/credentials.json5`).
pub fn credentials_file_path() -> Result<std::path::PathBuf> {
    Ok(dirs::config_dir()
        .context("Cannot determine config directory")?
        .join("do-next")
        .join("credentials.json5"))
}

/// Set `<section>.api_token` in credentials-file content, preserving every
/// other section (including ones this file doesn't model). `existing` is the
/// current file content (`None` when the file doesn't exist yet). Returns the
/// new content.
pub fn merge_token_into_credentials(
    existing: Option<&str>,
    section: &str,
    token: &str,
) -> Result<String> {
    let mut root: serde_json::Value = match existing {
        Some(content) => json5::from_str(content).context("Failed to parse credentials.json5")?,
        None => serde_json::json!({}),
    };
    let obj = root
        .as_object_mut()
        .context("credentials.json5 must contain an object")?;
    let entry = obj.entry(section).or_insert_with(|| serde_json::json!({}));
    let entry = entry
        .as_object_mut()
        .with_context(|| format!("`{section}` in credentials.json5 must be an object"))?;
    entry.insert("api_token".into(), serde_json::Value::String(token.into()));
    json5::to_string(&root).context("Failed to serialize credentials.json5")
}

/// Run a `credential_command` and return its trimmed stdout as the token.
pub fn run_credential_command(cmd: &str) -> Result<String> {
    log::debug!("credentials: running credential_command: {cmd}");
    let output = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .output()
        .with_context(|| format!("Failed to run credential_command: {cmd}"))?;
    if !output.status.success() {
        bail!("credential_command exited with non-zero status");
    }
    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    log::debug!("credentials: credential_command succeeded");
    Ok(token)
}

/// Fix-it lines for keyring error messages, so each caller points at its own
/// env var and refresh procedure.
struct KeyringHints {
    env_var: &'static str,
    refresh: &'static str,
}

/// Look up a secret in the OS keyring under the "do-next" service.
/// `Ok(None)` means "no entry, fall through to the next source"; storage and
/// platform failures are hard errors with actionable messages.
fn keyring_lookup(key: &str, hints: &KeyringHints) -> Result<Option<String>> {
    log::debug!("credentials: looking up keyring entry for key={key}");
    // Since keyring 4 the platform store is initialized on the first `Entry::new`,
    // so an unavailable or locked keyring surfaces here rather than on the lookup.
    let entry = keyring::Entry::new("do-next", key).map_err(|e| keyring_failure(key, hints, &e))?;
    match entry.get_password() {
        Ok(secret) => {
            log::debug!("credentials: keyring lookup succeeded");
            Ok(Some(secret))
        }
        Err(keyring::Error::NoEntry) => {
            log::debug!("credentials: no keyring entry found, falling through");
            Ok(None)
        }
        Err(e) => Err(keyring_failure(key, hints, &e)),
    }
}

/// Turn a keyring failure into an actionable error. `NoEntry` never reaches
/// here: a missing entry means "fall through to the next source", not a failure.
fn keyring_failure(key: &str, hints: &KeyringHints, err: &keyring::Error) -> anyhow::Error {
    match err {
        keyring::Error::NoStorageAccess(_) | keyring::Error::NoDefaultStore => {
            log::debug!("credentials: keyring storage not accessible: {err}");
            anyhow!(
                "The system keyring is not accessible (key={key}).\n\
                 The secret service may not be running or the keyring may be locked.\n\
                 \n\
                 Possible fixes:\n\
                 • Ensure your keyring daemon is running (gnome-keyring-daemon, kwallet, pass-secret-service)\n\
                 • Unlock the keyring or GPG agent and try again\n\
                 • Set the {} environment variable\n\
                 • Add credentials to ~/.config/do-next/credentials.json5\n\
                 \n\
                 Run with --log <file> for details.",
                hints.env_var
            )
        }
        keyring::Error::PlatformFailure(_) => {
            log::debug!("credentials: keyring platform failure: {err}");
            anyhow!(
                "The keyring returned an error while reading the secret (key={key}).\n\
                 The keyring may be locked or the stored entry may be corrupted.\n\
                 \n\
                 Possible fixes:\n\
                 • Unlock your keyring or GPG agent and try again\n\
                 • {}\n\
                 • Set the {} environment variable\n\
                 • Add credentials to ~/.config/do-next/credentials.json5\n\
                 \n\
                 Run with --log <file> for details.",
                hints.refresh,
                hints.env_var
            )
        }
        _ => {
            log::debug!("credentials: keyring error: {err}");
            anyhow!(
                "Unexpected keyring error (key={key}): {err}\n\
                 \n\
                 Possible fixes:\n\
                 • {}\n\
                 • Set the {} environment variable\n\
                 • Add credentials to ~/.config/do-next/credentials.json5",
                hints.refresh,
                hints.env_var
            )
        }
    }
}

#[derive(serde::Deserialize)]
struct CredentialsFile {
    jira: Option<CredentialsFileToken>,
    grafana: Option<CredentialsFileToken>,
    gitlab: Option<CredentialsFileToken>,
}

#[derive(serde::Deserialize)]
struct CredentialsFileToken {
    api_token: Option<String>,
}

fn read_credentials_file() -> Result<Option<CredentialsFile>> {
    let path = credentials_file_path()?;

    if !path.exists() {
        return Ok(None);
    }

    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let file: CredentialsFile =
        json5::from_str(&content).context("Failed to parse credentials.json5")?;
    Ok(Some(file))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_env_token_wins_over_an_oauth_config() {
        // DO_NEXT_GITLAB_TOKEN holds a personal access token, which must go out
        // as PRIVATE-TOKEN. Taking the token from the environment while letting
        // a config flag decide the header is exactly how `glab` ended up
        // sending PATs as bearer tokens (gitlab-org/cli#8482).
        assert_eq!(
            gitlab_auth_source(Some("glpat-xyz".into()), true),
            GitlabAuthSource::EnvToken("glpat-xyz".into())
        );
        assert_eq!(
            gitlab_auth_source(Some("glpat-xyz".into()), false),
            GitlabAuthSource::EnvToken("glpat-xyz".into())
        );
    }

    #[test]
    fn without_an_env_token_the_config_picks_the_source() {
        assert_eq!(gitlab_auth_source(None, true), GitlabAuthSource::Oauth);
        assert_eq!(
            gitlab_auth_source(None, false),
            GitlabAuthSource::StoredToken
        );
    }

    #[test]
    fn an_empty_env_token_is_not_a_token() {
        // `DO_NEXT_GITLAB_TOKEN=` must not shadow a working configuration.
        assert_eq!(
            gitlab_auth_source(Some(String::new()), true),
            GitlabAuthSource::Oauth
        );
        assert_eq!(
            gitlab_auth_source(Some(String::new()), false),
            GitlabAuthSource::StoredToken
        );
    }

    #[test]
    fn credentials_file_parses_every_token_section() {
        let file: CredentialsFile = json5::from_str(
            r#"{
                jira: { api_token: "jt" },
                grafana: { api_token: "gt" },
                gitlab: { api_token: "glt" },
            }"#,
        )
        .expect("valid credentials file");
        assert_eq!(file.jira.and_then(|j| j.api_token).as_deref(), Some("jt"));
        assert_eq!(
            file.grafana.and_then(|g| g.api_token).as_deref(),
            Some("gt")
        );
        assert_eq!(
            file.gitlab.and_then(|g| g.api_token).as_deref(),
            Some("glt")
        );

        // Every section is optional.
        let file: CredentialsFile = json5::from_str("{}").expect("empty file is valid");
        assert!(file.jira.is_none());
        assert!(file.grafana.is_none());
        assert!(file.gitlab.is_none());
    }

    #[test]
    fn merge_token_preserves_the_other_sections() {
        let existing = r#"{
            // hand-written comment (lost on rewrite, values must survive)
            jira: { api_token: "jt" },
            grafana: { api_token: "old" },
            gitlab: { api_token: "glt" },
        }"#;
        let merged =
            merge_token_into_credentials(Some(existing), "grafana", "new-token").expect("merges");
        let file: CredentialsFile = json5::from_str(&merged).expect("output parses");
        assert_eq!(file.jira.and_then(|j| j.api_token).as_deref(), Some("jt"));
        assert_eq!(
            file.grafana.and_then(|g| g.api_token).as_deref(),
            Some("new-token")
        );
        assert_eq!(
            file.gitlab.and_then(|g| g.api_token).as_deref(),
            Some("glt"),
            "the gitlab token must survive a grafana rewrite"
        );

        // ...and the mirror image: writing gitlab leaves grafana alone.
        let merged =
            merge_token_into_credentials(Some(existing), "gitlab", "fresh").expect("merges");
        let file: CredentialsFile = json5::from_str(&merged).expect("output parses");
        assert_eq!(
            file.gitlab.and_then(|g| g.api_token).as_deref(),
            Some("fresh")
        );
        assert_eq!(
            file.grafana.and_then(|g| g.api_token).as_deref(),
            Some("old")
        );
    }

    #[test]
    fn merge_token_creates_file_content_from_scratch() {
        for section in ["grafana", "gitlab"] {
            let merged = merge_token_into_credentials(None, section, "t0k3n").expect("merges");
            let root: serde_json::Value = json5::from_str(&merged).expect("output parses");
            assert_eq!(root[section]["api_token"], "t0k3n");
            let file: CredentialsFile = json5::from_str(&merged).expect("output parses");
            assert!(file.jira.is_none());
        }
    }

    #[test]
    fn merge_token_preserves_unknown_sections() {
        // Future sections (or user extras) must survive the rewrite even
        // though CredentialsFile doesn't model them.
        let existing = r#"{ future_thing: { key: "v" }, jira: { api_token: "jt" } }"#;
        let merged = merge_token_into_credentials(Some(existing), "gitlab", "glt").expect("merges");
        let root: serde_json::Value = json5::from_str(&merged).expect("output parses");
        assert_eq!(root["future_thing"]["key"], "v");
        assert_eq!(root["jira"]["api_token"], "jt");
        assert_eq!(root["gitlab"]["api_token"], "glt");
    }
}
