use anyhow::{Context, Result, bail};
use std::process::Command;

use crate::config::types::{JiraConfig, ResolvedGrafana};
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
    if let Some(token) = read_credentials_file()?.and_then(|f| f.jira).and_then(|j| j.api_token) {
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
        let key = grafana.credential_key.as_deref().unwrap_or(&grafana.oncall_api_url);
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

/// Path of the shared credentials file (`~/.config/do-next/credentials.json5`).
pub fn credentials_file_path() -> Result<std::path::PathBuf> {
    Ok(dirs::config_dir()
        .context("Cannot determine config directory")?
        .join("do-next")
        .join("credentials.json5"))
}

/// Set `grafana.api_token` in credentials-file content, preserving every
/// other section. `existing` is the current file content (`None` when the
/// file doesn't exist yet). Returns the new content.
pub fn merge_grafana_token_into_credentials(
    existing: Option<&str>,
    token: &str,
) -> Result<String> {
    let mut root: serde_json::Value = match existing {
        Some(content) => {
            json5::from_str(content).context("Failed to parse credentials.json5")?
        }
        None => serde_json::json!({}),
    };
    let obj = root
        .as_object_mut()
        .context("credentials.json5 must contain an object")?;
    let grafana = obj
        .entry("grafana")
        .or_insert_with(|| serde_json::json!({}));
    let grafana = grafana
        .as_object_mut()
        .context("`grafana` in credentials.json5 must be an object")?;
    grafana.insert("api_token".into(), serde_json::Value::String(token.into()));
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
    let entry = keyring::Entry::new("do-next", key).context("Failed to create keyring entry")?;
    match entry.get_password() {
        Ok(secret) => {
            log::debug!("credentials: keyring lookup succeeded");
            Ok(Some(secret))
        }
        Err(keyring::Error::NoEntry) => {
            log::debug!("credentials: no keyring entry found, falling through");
            Ok(None)
        }
        Err(keyring::Error::NoStorageAccess(e)) => {
            log::debug!("credentials: keyring storage not accessible: {e}");
            bail!(
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
            );
        }
        Err(keyring::Error::PlatformFailure(e)) => {
            log::debug!("credentials: keyring platform failure: {e}");
            bail!(
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
            );
        }
        Err(e) => {
            log::debug!("credentials: keyring error: {e}");
            bail!(
                "Unexpected keyring error (key={key}): {e}\n\
                 \n\
                 Possible fixes:\n\
                 • {}\n\
                 • Set the {} environment variable\n\
                 • Add credentials to ~/.config/do-next/credentials.json5",
                hints.refresh,
                hints.env_var
            );
        }
    }
}

#[derive(serde::Deserialize)]
struct CredentialsFile {
    jira: Option<CredentialsFileJira>,
    grafana: Option<CredentialsFileGrafana>,
}

#[derive(serde::Deserialize)]
struct CredentialsFileJira {
    api_token: Option<String>,
}

#[derive(serde::Deserialize)]
struct CredentialsFileGrafana {
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
    fn credentials_file_parses_jira_and_grafana_sections() {
        let file: CredentialsFile = json5::from_str(
            r#"{
                jira: { api_token: "jt" },
                grafana: { api_token: "gt" },
            }"#,
        )
        .expect("valid credentials file");
        assert_eq!(file.jira.and_then(|j| j.api_token).as_deref(), Some("jt"));
        assert_eq!(
            file.grafana.and_then(|g| g.api_token).as_deref(),
            Some("gt")
        );

        // Both sections are optional.
        let file: CredentialsFile = json5::from_str("{}").expect("empty file is valid");
        assert!(file.jira.is_none());
        assert!(file.grafana.is_none());
    }

    #[test]
    fn merge_grafana_token_preserves_other_sections() {
        let existing = r#"{
            // hand-written comment (lost on rewrite, values must survive)
            jira: { api_token: "jt" },
            grafana: { api_token: "old" },
        }"#;
        let merged =
            merge_grafana_token_into_credentials(Some(existing), "new-token").expect("merges");
        let file: CredentialsFile = json5::from_str(&merged).expect("output parses");
        assert_eq!(file.jira.and_then(|j| j.api_token).as_deref(), Some("jt"));
        assert_eq!(
            file.grafana.and_then(|g| g.api_token).as_deref(),
            Some("new-token")
        );
    }

    #[test]
    fn merge_grafana_token_creates_file_content_from_scratch() {
        let merged = merge_grafana_token_into_credentials(None, "t0k3n").expect("merges");
        let file: CredentialsFile = json5::from_str(&merged).expect("output parses");
        assert!(file.jira.is_none());
        assert_eq!(file.grafana.and_then(|g| g.api_token).as_deref(), Some("t0k3n"));
    }

    #[test]
    fn merge_grafana_token_preserves_unknown_sections() {
        // Future sections (or user extras) must survive the rewrite even
        // though CredentialsFile doesn't model them.
        let existing = r#"{ future_thing: { key: "v" }, jira: { api_token: "jt" } }"#;
        let merged =
            merge_grafana_token_into_credentials(Some(existing), "gt").expect("merges");
        let root: serde_json::Value = json5::from_str(&merged).expect("output parses");
        assert_eq!(root["future_thing"]["key"], "v");
        assert_eq!(root["jira"]["api_token"], "jt");
        assert_eq!(root["grafana"]["api_token"], "gt");
    }
}
