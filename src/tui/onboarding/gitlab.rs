//! Interactive GitLab token setup. Runs before the TUI when a team has GitLab
//! sources but no token is configured, and from `do-next auth` for later
//! rotation. Mirrors the Grafana `OnCall` flow: storage menu, masked input,
//! live validation against the API before anything is stored.

use anyhow::{Context, Result};

use crate::config::credentials::{
    credentials_file_path, merge_token_into_credentials, run_credential_command,
};
use crate::config::types::{Config, GitlabConfig};

use super::{
    SetupOutcome, StorageChoice, check_keyring_available, prompt, prompt_masked,
    prompt_token_storage, prompt_yes_no, write_user_config,
};

/// Offer to configure the personal access token for the teams behind one
/// GitLab instance. `raw` is the user's on-disk config (never the merged
/// view); it is rewritten only when the chosen storage needs config fields
/// (keyring, credential command).
pub async fn setup_gitlab_token(
    target: &crate::gitlab::TokenSetupTarget,
    raw: &mut Config,
) -> Result<SetupOutcome> {
    let base_url = target.base_url.as_str();
    let teams = target.team_ids.join("', '");
    println!();
    println!("Team '{teams}' lists GitLab merge requests alongside its Jira issues.");
    println!("Reading them needs a personal access token.");
    println!();
    if !prompt_yes_no("Configure the token now? [Y/n]: ", true)? {
        println!("Skipping. Set it up any time with `do-next auth`.");
        return Ok(SetupOutcome::Declined);
    }

    print_gitlab_token_instructions(base_url);
    let storage = prompt_token_storage(None, None, "DO_NEXT_GITLAB_TOKEN")?;

    match storage {
        StorageChoice::Keyring => {
            check_keyring_available(base_url)?;
            let token = prompt_validated_token(base_url).await?;
            let entry = keyring::Entry::new("do-next", base_url)
                .map_err(|e| anyhow::anyhow!("Failed to access keyring: {e}"))?;
            entry
                .set_password(&token)
                .map_err(|e| anyhow::anyhow!("Failed to store token in keyring: {e}"))?;
            raw.gitlab
                .get_or_insert_with(GitlabConfig::default)
                .credential_store = Some("keyring".into());
            write_user_config(raw)?;
            println!("GitLab token stored in the system keyring.");
            Ok(SetupOutcome::Configured)
        }
        StorageChoice::File => {
            let token = prompt_validated_token(base_url).await?;
            let path = credentials_file_path()?;
            let existing = match std::fs::read_to_string(&path) {
                Ok(content) => Some(content),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => {
                    return Err(e).with_context(|| format!("Failed to read {}", path.display()));
                }
            };
            let merged = merge_token_into_credentials(existing.as_deref(), "gitlab", &token)?;
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&path, merged)
                .with_context(|| format!("Failed to write {}", path.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            }
            println!("GitLab token written to {}", path.display());
            Ok(SetupOutcome::Configured)
        }
        StorageChoice::Command => {
            println!("Enter the shell command whose stdout is your GitLab token.");
            println!("Examples:  pass show gitlab/token");
            println!("           op read 'op://Private/GitLab/credential'");
            println!();
            let cmd = prompt("Credential command: ", None)?;
            let token = run_credential_command(&cmd)?;
            greet(base_url, token).await?;
            raw.gitlab
                .get_or_insert_with(GitlabConfig::default)
                .credential_command = Some(cmd);
            write_user_config(raw)?;
            println!("Credential command saved to your config.");
            Ok(SetupOutcome::Configured)
        }
        StorageChoice::Env => {
            println!();
            println!("Set the following environment variable before running do-next:");
            println!("  DO_NEXT_GITLAB_TOKEN=<your-personal-access-token>");
            println!("GitLab sources report an error until it is set.");
            Ok(SetupOutcome::EnvOnly)
        }
    }
}

fn print_gitlab_token_instructions(base_url: &str) {
    println!();
    println!("GitLab Personal Access Token");
    println!("  A personal token identifies you to the API, so do-next can list the");
    println!("  merge requests waiting on you.");
    println!("  Create one here:");
    println!(
        "    {}/-/user_settings/personal_access_tokens",
        base_url.trim_end_matches('/')
    );
    // do-next never writes to GitLab, so the read-only scope is enough.
    println!("  Required scope: read_api (read-only — do-next never writes).");
    println!();
    println!("  GitLab instance in use: {base_url}");
    println!();
}

/// Masked token prompt with live validation; loops until a token checks out
/// or the user gives up.
async fn prompt_validated_token(base_url: &str) -> Result<String> {
    loop {
        let token = prompt_masked("GitLab token: ")?;
        if token.is_empty() {
            println!("Empty token.");
        } else {
            println!("Checking the token against the GitLab API...");
            match greet(base_url, token.clone()).await {
                Ok(()) => return Ok(token),
                Err(e) => println!("{e:#}"),
            }
        }
        if !prompt_yes_no("Try again? [Y/n]: ", true)? {
            anyhow::bail!("GitLab token setup cancelled");
        }
    }
}

/// Validate the token and print who it belongs to.
async fn greet(base_url: &str, token: String) -> Result<()> {
    let user = crate::gitlab::validate_token(base_url, token).await?;
    println!("Authenticated as {} (@{}).", user.display(), user.username);
    Ok(())
}
