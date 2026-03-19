use anyhow::{Context, Result, bail};
use std::process::Command;

use crate::config::types::JiraConfig;
use crate::jira::auth::Credentials;

/// Resolve Jira credentials using the precedence chain:
/// 1. Environment variables
/// 2. `credential_command` (shell exec)
/// 3. OS keyring
/// 4. credentials file (~/.config/do-next/credentials.json5)
pub fn resolve_credentials(jira: &JiraConfig) -> Result<Credentials> {
    // 1. Environment variables
    if let Ok(token) = std::env::var("DO_NEXT_JIRA_TOKEN") {
        log::debug!("credentials: using DO_NEXT_JIRA_TOKEN env var");
        return Ok(Credentials::Token(token));
    }
    if let (Ok(user), Ok(pass)) = (
        std::env::var("DO_NEXT_JIRA_USERNAME"),
        std::env::var("DO_NEXT_JIRA_PASSWORD"),
    ) {
        log::debug!("credentials: using DO_NEXT_JIRA_USERNAME/PASSWORD env vars");
        return Ok(Credentials::Basic {
            username: user,
            password: pass,
        });
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
        let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
        log::debug!("credentials: credential_command succeeded");
        return Ok(parse_credential_output(raw));
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
                return Ok(parse_credential_output(secret));
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
                     • Set the DO_NEXT_JIRA_TOKEN environment variable\n\
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
                     • Re-run `do-next auth` to store a fresh token\n\
                     • Set the DO_NEXT_JIRA_TOKEN environment variable\n\
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
                     • Re-run `do-next auth` to store a fresh token\n\
                     • Set the DO_NEXT_JIRA_TOKEN environment variable\n\
                     • Add credentials to ~/.config/do-next/credentials.json5"
                );
            }
        }
    }

    // 4. Credentials file
    log::debug!("credentials: checking credentials file");
    if let Some(creds) = load_credentials_file()? {
        log::debug!("credentials: loaded from credentials file");
        return Ok(creds);
    }

    bail!("No Jira credentials found. Set DO_NEXT_JIRA_TOKEN env var or configure credentials.")
}

/// Parse a credential string: either a bare PAT or "username:password".
fn parse_credential_output(s: String) -> Credentials {
    if let Some((user, pass)) = s.split_once(':') {
        Credentials::Basic {
            username: user.to_string(),
            password: pass.to_string(),
        }
    } else {
        Credentials::Token(s)
    }
}

#[derive(serde::Deserialize)]
struct CredentialsFile {
    jira: Option<CredentialsFileJira>,
}

#[derive(serde::Deserialize)]
struct CredentialsFileJira {
    token: Option<String>,
    username: Option<String>,
    password: Option<String>,
}

fn load_credentials_file() -> Result<Option<Credentials>> {
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

    if let Some(token) = jira.token {
        return Ok(Some(Credentials::Token(token)));
    }
    if let (Some(user), Some(pass)) = (jira.username, jira.password) {
        return Ok(Some(Credentials::Basic {
            username: user,
            password: pass,
        }));
    }

    Ok(None)
}

/// Store a token in the OS keyring (called during onboarding).
#[allow(dead_code)]
pub fn store_in_keyring(base_url: &str, key: Option<&str>, token: &str) -> Result<()> {
    let key = key.unwrap_or(base_url);
    let entry = keyring::Entry::new("do-next", key).context("Failed to create keyring entry")?;
    entry
        .set_password(token)
        .context("Failed to store token in keyring")?;
    Ok(())
}
