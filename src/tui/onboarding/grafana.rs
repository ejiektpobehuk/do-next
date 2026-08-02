//! Interactive Grafana `OnCall` token setup. Runs before the TUI when a team
//! wants the on-call view but no token is configured, and from `do-next auth`
//! for later rotation. Mirrors the Jira token flow: storage menu, masked
//! input, live validation against the `OnCall` API before anything is stored.

use anyhow::{Context, Result};

use crate::config::credentials::{
    credentials_file_path, merge_token_into_credentials, run_credential_command,
};
use crate::config::types::{Config, GrafanaConfig};

use super::{
    SetupOutcome, StorageChoice, check_keyring_available, prompt, prompt_masked,
    prompt_token_storage, prompt_yes_no, write_user_config,
};

/// Offer to configure the `OnCall` API token for the teams behind one
/// `OnCall` API URL. `raw` is the user's on-disk config (never the merged
/// view); it is rewritten only when the chosen storage needs config fields
/// (keyring, credential command).
pub async fn setup_grafana_token(
    target: &crate::grafana::TokenSetupTarget,
    raw: &mut Config,
) -> Result<SetupOutcome> {
    let oncall_api_url = target.oncall_api_url.as_str();
    let teams = target.team_ids.join("', '");
    println!();
    println!("Team '{teams}' switches its sources while you are on call (Grafana OnCall).");
    println!("That check needs a personal OnCall API token.");
    println!();
    if !prompt_yes_no("Configure the token now? [Y/n]: ", true)? {
        println!("Skipping. Set it up any time with `do-next auth`.");
        return Ok(SetupOutcome::Declined);
    }

    print_grafana_token_instructions(target);
    let storage = prompt_token_storage(None, None, "DO_NEXT_GRAFANA_TOKEN")?;

    match storage {
        StorageChoice::Keyring => {
            check_keyring_available(oncall_api_url)?;
            let token = prompt_validated_token(oncall_api_url).await?;
            let entry = keyring::Entry::new("do-next", oncall_api_url)
                .map_err(|e| anyhow::anyhow!("Failed to access keyring: {e}"))?;
            entry
                .set_password(&token)
                .map_err(|e| anyhow::anyhow!("Failed to store token in keyring: {e}"))?;
            raw.grafana
                .get_or_insert_with(GrafanaConfig::default)
                .credential_store = Some("keyring".into());
            write_user_config(raw)?;
            println!("OnCall API token stored in the system keyring.");
            Ok(SetupOutcome::Configured)
        }
        StorageChoice::File => {
            let token = prompt_validated_token(oncall_api_url).await?;
            let path = credentials_file_path()?;
            let existing = match std::fs::read_to_string(&path) {
                Ok(content) => Some(content),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => {
                    return Err(e).with_context(|| format!("Failed to read {}", path.display()));
                }
            };
            let merged = merge_token_into_credentials(existing.as_deref(), "grafana", &token)?;
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
            println!("OnCall API token written to {}", path.display());
            Ok(SetupOutcome::Configured)
        }
        StorageChoice::Command => {
            println!("Enter the shell command whose stdout is your OnCall API token.");
            println!("Examples:  pass show grafana/oncall");
            println!("           op read 'op://Private/Grafana OnCall/credential'");
            println!();
            let cmd = prompt("Credential command: ", None)?;
            let token = run_credential_command(&cmd)?;
            greet(oncall_api_url, token).await?;
            raw.grafana
                .get_or_insert_with(GrafanaConfig::default)
                .credential_command = Some(cmd);
            write_user_config(raw)?;
            println!("Credential command saved to your config.");
            Ok(SetupOutcome::Configured)
        }
        StorageChoice::Env => {
            println!();
            println!("Set the following environment variable before running do-next:");
            println!("  DO_NEXT_GRAFANA_TOKEN=<your-oncall-api-token>");
            println!("The on-call view stays disabled until it is set.");
            Ok(SetupOutcome::EnvOnly)
        }
    }
}

fn print_grafana_token_instructions(target: &crate::grafana::TokenSetupTarget) {
    println!();
    println!("Grafana OnCall API Token");
    println!("  A personal token identifies you to the OnCall API, so do-next can");
    println!("  tell whether you are the one on call.");
    // The token is created in the Grafana web UI — a different host from the
    // OnCall API, so only a configured instance_url gives a clickable link.
    if let Some(instance) = &target.instance_url {
        let instance = instance.trim_end_matches('/');
        println!("  Create one under \"API tokens\" on the IRM settings page:");
        println!("    {instance}/a/grafana-irm-app/settings");
    } else {
        println!("  To create one, open your Grafana stack: IRM → Settings → API tokens.");
    }
    println!("  It must be a personal OnCall token, not a service account token.");
    println!();
    println!("  OnCall API URL in use: {}", target.oncall_api_url);
    println!();
}

/// Masked token prompt with live validation; loops until a token checks out
/// or the user gives up.
async fn prompt_validated_token(oncall_api_url: &str) -> Result<String> {
    loop {
        let token = prompt_masked("OnCall API token: ")?;
        if token.is_empty() {
            println!("Empty token.");
        } else {
            println!("Checking the token against the OnCall API...");
            match greet(oncall_api_url, token.clone()).await {
                Ok(()) => return Ok(token),
                Err(e) => println!("{e:#}"),
            }
        }
        if !prompt_yes_no("Try again? [Y/n]: ", true)? {
            anyhow::bail!("OnCall token setup cancelled");
        }
    }
}

/// Validate the token and print who it belongs to.
async fn greet(oncall_api_url: &str, token: String) -> Result<()> {
    let user = crate::grafana::validate_token(oncall_api_url, token).await?;
    let name = user.username.or(user.email).unwrap_or(user.id);
    println!("Authenticated as {name}.");
    Ok(())
}
