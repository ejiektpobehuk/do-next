use anyhow::{Context, Result, bail};
use std::process::Command;

use crate::config::types::JiraConfig;
use crate::jira::auth::Credentials;

/// Resolve Jira credentials (email + API token).
///
/// Email precedence: `DO_NEXT_JIRA_EMAIL` env → `config.jira.email`.
///
/// API token precedence:
/// 1. `DO_NEXT_JIRA_API_TOKEN` env
/// 2. `credential_command` (shell exec, stdout = API token)
/// 3. OS keyring
/// 4. credentials file (`~/.config/do-next/credentials.json5`)
pub fn resolve_credentials(jira: &JiraConfig) -> Result<Credentials> {
    let email = resolve_email(jira)?;
    let api_token = resolve_api_token(jira)?;
    Ok(Credentials { email, api_token })
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
        return Ok(token);
    }

    // 3. Keyring
    if jira.credential_store.as_deref() == Some("keyring") {
        let key = jira.credential_key.as_deref().unwrap_or(&jira.base_url);
        log::debug!("credentials: looking up keyring entry for key={key}");
        let entry =
            keyring::Entry::new("do-next", key).context("Failed to create keyring entry")?;
        match entry.get_password() {
            Ok(secret) => {
                log::debug!("credentials: keyring lookup succeeded");
                return Ok(secret);
            }
            Err(keyring::Error::NoEntry) => {
                log::debug!("credentials: no keyring entry found, falling through");
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
                     • Set the DO_NEXT_JIRA_API_TOKEN environment variable\n\
                     • Add credentials to ~/.config/do-next/credentials.json5\n\
                     \n\
                     Run with --log <file> for details."
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
                     • Re-run `do-next auth` to store a fresh API token\n\
                     • Set the DO_NEXT_JIRA_API_TOKEN environment variable\n\
                     • Add credentials to ~/.config/do-next/credentials.json5\n\
                     \n\
                     Run with --log <file> for details."
                );
            }
            Err(e) => {
                log::debug!("credentials: keyring error: {e}");
                bail!(
                    "Unexpected keyring error (key={key}): {e}\n\
                     \n\
                     Possible fixes:\n\
                     • Re-run `do-next auth` to store a fresh API token\n\
                     • Set the DO_NEXT_JIRA_API_TOKEN environment variable\n\
                     • Add credentials to ~/.config/do-next/credentials.json5"
                );
            }
        }
    }

    // 4. Credentials file
    log::debug!("credentials: checking credentials file");
    if let Some(token) = load_credentials_file()? {
        log::debug!("credentials: loaded from credentials file");
        return Ok(token);
    }

    bail!(
        "No Jira API token found.\n\
         Set DO_NEXT_JIRA_API_TOKEN env var or run `do-next auth` to configure credentials."
    )
}

#[derive(serde::Deserialize)]
struct CredentialsFile {
    jira: Option<CredentialsFileJira>,
}

#[derive(serde::Deserialize)]
struct CredentialsFileJira {
    api_token: Option<String>,
}

fn load_credentials_file() -> Result<Option<String>> {
    let path = dirs::config_dir()
        .context("Cannot determine config directory")?
        .join("do-next")
        .join("credentials.json5");

    if !path.exists() {
        return Ok(None);
    }

    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let file: CredentialsFile =
        json5::from_str(&content).context("Failed to parse credentials.json5")?;

    let Some(jira) = file.jira else {
        return Ok(None);
    };

    Ok(jira.api_token)
}
