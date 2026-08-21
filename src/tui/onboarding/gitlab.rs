//! Interactive GitLab credential setup. Runs before the TUI when a team has
//! GitLab sources but nothing is configured, and from `do-next auth` for later
//! rotation.
//!
//! Offers the same three login types as `glab`: **Web** (browser), **Device**
//! (one-time code, for SSH sessions) and a **personal access token**. The
//! token path mirrors the Grafana `OnCall` flow — storage menu, masked input,
//! live validation against the API before anything is stored.

use anyhow::{Context, Result};

use crate::atlassian::auth::OAuthStore;
use crate::config::credentials::{
    credentials_file_path, merge_token_into_credentials, run_credential_command,
};
use crate::config::types::{Config, GitlabConfig};
use crate::gitlab::oauth::{PREFERRED_PORT, REGISTERED_REDIRECT_URI, SCOPES};

use super::{
    SetupOutcome, StorageChoice, check_keyring_available, prompt, prompt_masked,
    prompt_oauth_storage, prompt_token_storage, prompt_yes_no, run_selection, write_user_config,
};

/// How to sign in to GitLab.
#[derive(PartialEq, Clone, Copy)]
enum LoginType {
    Web,
    Device,
    Token,
}

const LOGIN_TYPE_COUNT: usize = 3;

const LOGIN_TYPE_LABELS: [&str; LOGIN_TYPE_COUNT] = [
    "Web (browser)     ",
    "Device (code)     ",
    "Personal token    ",
];

const LOGIN_TYPE_DESCRIPTIONS: [&str; LOGIN_TYPE_COUNT] = [
    "sign in through your browser (recommended)",
    "one-time code — for SSH or headless sessions",
    "create a token by hand in GitLab",
];

fn prompt_login_type() -> Result<LoginType> {
    let tags = vec![String::new(); LOGIN_TYPE_COUNT];
    let idx = run_selection(
        "How would you like to sign in to GitLab?",
        &LOGIN_TYPE_LABELS,
        &LOGIN_TYPE_DESCRIPTIONS,
        &tags,
        0,
        None,
    )?;
    Ok(match idx {
        0 => LoginType::Web,
        1 => LoginType::Device,
        _ => LoginType::Token,
    })
}

/// Offer to configure the personal access token for the teams behind one
/// GitLab instance. `raw` is the user's on-disk config (never the merged
/// view); it is rewritten only when the chosen storage needs config fields
/// (keyring, credential command).
pub async fn setup_gitlab_token(
    target: &crate::gitlab::TokenSetupTarget,
    raw: &mut Config,
) -> Result<SetupOutcome> {
    let teams = target.team_ids.join("', '");
    println!();
    println!("Team '{teams}' lists GitLab merge requests alongside its Jira issues.");
    println!("Reading them needs GitLab credentials.");
    println!();
    if !prompt_yes_no("Set that up now? [Y/n]: ", true)? {
        println!("Skipping. Set it up any time with `do-next auth`.");
        return Ok(SetupOutcome::Declined);
    }

    configure_gitlab_token(target, raw).await
}

/// The sign-in flow with no "do you want to?" gate.
///
/// The gate above belongs to the startup offer, where the user asked to launch
/// the app rather than to configure anything. Choosing a row in `do-next auth`
/// *is* the consent, so asking again is noise.
pub async fn configure_gitlab_token(
    target: &crate::gitlab::TokenSetupTarget,
    raw: &mut Config,
) -> Result<SetupOutcome> {
    match prompt_login_type()? {
        LoginType::Web => setup_oauth(target, raw, false),
        LoginType::Device => setup_oauth(target, raw, true),
        LoginType::Token => setup_token(target, raw).await,
    }
}

/// Sign in through the browser (or a one-time code) and store the tokens.
fn setup_oauth(
    target: &crate::gitlab::TokenSetupTarget,
    raw: &mut Config,
    device: bool,
) -> Result<SetupOutcome> {
    let base_url = target.base_url.as_str();
    let (client_id, prompted) = resolve_oauth_client_id(target, device)?;
    let client_secret = target.oauth_client_secret.clone();

    let storage = prompt_oauth_storage(None)?;
    let store = match storage {
        StorageChoice::Keyring => {
            // Probe the key the tokens will actually live under.
            check_keyring_available(&crate::gitlab::oauth::keyring_key(base_url))?;
            OAuthStore::Keyring
        }
        _ => OAuthStore::File,
    };

    let creds = if device {
        crate::gitlab::oauth::run_device_flow(base_url, &client_id, client_secret.as_deref(), store)
    } else {
        crate::gitlab::oauth::run_web_flow(base_url, &client_id, client_secret.as_deref(), store)
    }?;

    // Confirm the tokens actually work, and say who they belong to — the same
    // reassurance the token path gives.
    let auth = crate::gitlab::auth::GitlabAuth::OAuth(creds);
    let url = base_url.to_string();
    let user = crate::oauth::blocking_http(move || {
        Box::pin(async move { crate::gitlab::validate_auth(&url, auth).await })
    })?;
    println!("Authenticated as {} (@{}).", user.display(), user.username);

    let gitlab = raw.gitlab.get_or_insert_with(GitlabConfig::default);
    gitlab.auth_method = Some("oauth".into());
    if prompted {
        gitlab.oauth_client_id = Some(client_id);
    }
    write_user_config(raw)?;
    println!("GitLab OAuth is configured for {base_url}.");
    Ok(SetupOutcome::Configured)
}

/// Resolve the OAuth application id: env, then config (including company
/// manifest defaults), then a prompt with registration instructions.
///
/// The bool says whether the user typed it in. Only a typed-in id is written to
/// their config: one that came from the company manifest must stay there, or
/// rotating it in the config repo would stop propagating.
fn resolve_oauth_client_id(
    target: &crate::gitlab::TokenSetupTarget,
    device: bool,
) -> Result<(String, bool)> {
    if let Ok(id) = std::env::var("DO_NEXT_GITLAB_OAUTH_CLIENT_ID")
        && !id.is_empty()
    {
        println!("Using the OAuth app from DO_NEXT_GITLAB_OAUTH_CLIENT_ID.");
        return Ok((id, false));
    }
    // The *effective* config, so an application id shared through a company
    // manifest is picked up instead of being prompted for.
    if let Some(id) = target
        .oauth_client_id
        .as_deref()
        .filter(|id| !id.is_empty())
    {
        println!("Using the configured OAuth app (application id: {id}).");
        return Ok((id.to_string(), false));
    }
    print_oauth_app_instructions(&target.base_url, device);
    let id = prompt("Application ID: ", None)?;
    if id.is_empty() {
        anyhow::bail!(
            "An application ID is required for OAuth.\n\
             Run `do-next auth` to try again, or choose the personal-token option."
        );
    }
    Ok((id, true))
}

fn print_oauth_app_instructions(base_url: &str, device: bool) {
    let base = base_url.trim_end_matches('/');
    println!();
    println!("GitLab OAuth application");
    println!("  GitLab has no public app registry, so do-next needs an application");
    println!("  registered on the instance. Register it once (an instance-wide app in");
    println!("  the Admin area covers the whole team) and share the application ID.");
    println!();
    println!("  1. Open {base}/-/user_settings/applications");
    println!("     (or Admin area > Applications for an instance-wide app)");
    println!("  2. Name: do-next");
    if device {
        println!("  3. Redirect URI: not needed for device sign-in");
    } else {
        println!("  3. Redirect URI: {REGISTERED_REDIRECT_URI}");
        println!("     Keep the 127.0.0.1 form — it lets do-next fall back to another");
        println!("     port when {PREFERRED_PORT} is busy.");
    }
    println!("  4. Leave 'Confidential' UNCHECKED — a confidential app needs a secret");
    println!("     and sign-in fails with `invalid_client`.");
    println!("  5. Scopes: {SCOPES} (read-only — do-next never writes)");
    if device {
        println!("  6. Allowed grant types must include `device_code`,");
        println!("     and the instance must run GitLab 17.9 or later.");
    }
    println!();
    println!("  Then paste the application ID below.");
    println!();
    println!("  To share it with the team, put it in the company manifest as");
    println!("  `defaults.gitlab.oauth_client_id` — it is not a secret.");
    println!();
}

/// The original flow: a personal access token the user creates by hand.
async fn setup_token(
    target: &crate::gitlab::TokenSetupTarget,
    raw: &mut Config,
) -> Result<SetupOutcome> {
    let base_url = target.base_url.as_str();
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
            let gitlab = raw.gitlab.get_or_insert_with(GitlabConfig::default);
            gitlab.credential_store = Some("keyring".into());
            // An explicit token choice overrides a company manifest that
            // implies OAuth.
            gitlab.auth_method = Some("token".into());
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
            let gitlab = raw.gitlab.get_or_insert_with(GitlabConfig::default);
            gitlab.credential_command = Some(cmd);
            gitlab.auth_method = Some("token".into());
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
