mod company;
pub mod gitlab;
pub mod grafana;
pub mod menu;

pub use company::{run_company_join_command, run_company_teams_command};

use anyhow::Result;
use crossterm::cursor::MoveUp;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::style::{Color, Print, ResetColor, SetForegroundColor};
use crossterm::terminal::{Clear, ClearType, disable_raw_mode, enable_raw_mode};
use std::io;
use std::io::Write;

use crate::atlassian::auth::OAuthStore;
use crate::config::LoadedConfig;
use crate::config::types::{
    AtlassianConfig, AtlassianOverride, Config, ResolvedTeam, TeamConfig, TeamRef,
};

// ── Step 1: auth method ─────────────────────────────────────────────────────

#[derive(PartialEq, Clone)]
enum AuthMethod {
    OAuth,
    PersonalToken,
}

const AUTH_METHOD_COUNT: usize = 2;

const AUTH_METHOD_LABELS: [&str; AUTH_METHOD_COUNT] = ["Personal API token", "OAuth (browser)   "];

const AUTH_METHOD_DESCRIPTIONS: [&str; AUTH_METHOD_COUNT] = [
    "create a token at id.atlassian.com (recommended)",
    "requires an app registered by you at developer.atlassian.com",
];

/// The user backed out of a prompt (Esc, `q`, Ctrl-C).
///
/// A typed error so a caller can tell "went back" from "went wrong" without
/// matching on message text. The auth menu needs that distinction: a nested Esc
/// means "return to the menu", not `error: Cancelled`.
#[derive(Debug, thiserror::Error)]
#[error("Cancelled")]
pub struct Cancelled;

/// True when `e` is a [`Cancelled`] — the user backed out rather than failed.
pub fn is_cancelled(e: &anyhow::Error) -> bool {
    e.downcast_ref::<Cancelled>().is_some()
}

/// Result of one interactive side-service token setup (Grafana `OnCall`,
/// GitLab). Both flows share it so `main` can treat them alike.
pub enum SetupOutcome {
    /// A token was stored (and, for keyring/command, the user config updated)
    /// — the caller should reload the config so resolution picks it up.
    Configured,
    /// The user skipped; ask again next launch, or via `do-next auth`.
    Declined,
    /// The user chose the env var; instructions were printed, nothing stored.
    EnvOnly,
}

/// Persist the raw user config (mirrors the `do-next auth` rewrite). Shared by
/// the side-service token flows, which store `credential_store` /
/// `credential_command` in the user's config.
pub(super) fn write_user_config(raw: &Config) -> Result<()> {
    let config_path = crate::config::user_config_path()?;
    if config_path.exists() {
        println!("Note: config file will be rewritten in minimal format (comments removed).");
    }
    if let Some(dir) = config_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json5_content = json5::to_string(raw)?;
    std::fs::write(&config_path, json5_content)
        .map_err(|e| anyhow::anyhow!("Failed to write {}: {e}", config_path.display()))?;
    Ok(())
}

// ── Step 2: storage ─────────────────────────────────────────────────────────

#[derive(PartialEq, Clone)]
pub(super) enum StorageChoice {
    Keyring,
    File,
    Command,
    Env,
}

// OAuth storage options (2).
const OAUTH_STORAGE_COUNT: usize = 2;

const OAUTH_STORAGE_LABELS: [&str; OAUTH_STORAGE_COUNT] = ["System keyring  ", "Credentials file"];

const OAUTH_STORAGE_DESCRIPTIONS: [&str; OAUTH_STORAGE_COUNT] = [
    KEYRING_DESCRIPTION,
    "~/.config/do-next/oauth_tokens.json5 (chmod 600)",
];

// Token storage options (4).
const TOKEN_STORAGE_COUNT: usize = 4;

const TOKEN_STORAGE_LABELS: [&str; TOKEN_STORAGE_COUNT] = [
    "System keyring  ",
    "Credentials file",
    "External command",
    "Environment var ",
];

/// Storage menu descriptions; the env row names the flow's own variable
/// (Jira vs Grafana).
fn token_storage_descriptions(env_var: &str) -> [String; TOKEN_STORAGE_COUNT] {
    [
        KEYRING_DESCRIPTION.into(),
        "~/.config/do-next/credentials.json5 (chmod 600)".into(),
        "fetch via shell command (pass, bitwarden CLI, …)".into(),
        format!("set {env_var} env manually"),
    ]
}

const KEYRING_DESCRIPTION: &str = if cfg!(target_os = "macos") {
    "macOS Keychain (recommended)"
} else if cfg!(target_os = "windows") {
    "Windows Credential Manager (recommended)"
} else {
    "Linux Secret Service (recommended)"
};

// ── Status probing ──────────────────────────────────────────────────────────

pub(super) struct CredentialStatus {
    env_set: bool,
    file_exists: bool,
    keyring_found: bool,
    command: Option<String>,
}

enum ConfigStyle {
    Minimal,
    Template,
}

// ── Onboarding (first run) ──────────────────────────────────────────────────

const SETUP_KIND_COUNT: usize = 2;

const SETUP_KIND_LABELS: [&str; SETUP_KIND_COUNT] = ["Personal setup", "Join a company"];

const SETUP_KIND_DESCRIPTIONS: [&str; SETUP_KIND_COUNT] = [
    "configure your own Jira connection",
    "your company shares a do-next config repo (git URL or local path)",
];

/// Run the interactive first-run wizard.
/// Returns a fully configured `LoadedConfig` (credentials stored per user's choice).
#[allow(clippy::too_many_lines)]
pub fn run_onboarding() -> Result<LoadedConfig> {
    println!("Welcome to do-next! Let's set up your configuration.\n");

    let tags = vec![String::new(); SETUP_KIND_COUNT];
    let setup_kind = run_selection(
        "How would you like to start?",
        &SETUP_KIND_LABELS,
        &SETUP_KIND_DESCRIPTIONS,
        &tags,
        0,
        None,
    )?;
    if setup_kind == 1 {
        return company::run_first_run_company_join();
    }
    println!();

    let base_url = prompt(
        "Jira base URL (e.g. https://mycompany.atlassian.net): ",
        None,
    )?;
    let default_project = prompt("Default project key (e.g. PTMT): ", None)?;

    // Step 1: auth method.
    println!();
    let auth_method = prompt_auth_method(None)?;

    // Step 2: storage.
    println!();
    let storage = match auth_method {
        AuthMethod::OAuth => prompt_oauth_storage(None)?,
        AuthMethod::PersonalToken => {
            prompt_token_storage(None, None, "DO_NEXT_ATLASSIAN_API_TOKEN")?
        }
    };

    // Step 3: email (only for personal token).
    let email = if auth_method == AuthMethod::PersonalToken {
        Some(prompt("Jira account email: ", None)?)
    } else {
        None
    };

    let mut jira_config = AtlassianConfig {
        base_url: base_url.clone(),
        default_project: default_project.clone(),
        email,
        ..Default::default()
    };

    let config_dir = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine config directory"))?
        .join("do-next");
    std::fs::create_dir_all(&config_dir)?;

    match auth_method {
        AuthMethod::OAuth => {
            let (client_id, client_secret) = resolve_oauth_client_credentials(&jira_config)?;
            let store = match storage {
                StorageChoice::Keyring => OAuthStore::Keyring,
                _ => OAuthStore::File,
            };
            // First run: no team sources exist yet, so no granular scopes.
            crate::atlassian::oauth::run_oauth_flow(
                &client_id,
                &client_secret,
                store,
                crate::atlassian::oauth::ExtraScopes::default(),
            )?;
            jira_config.auth_method = Some("oauth".into());
            jira_config.oauth_client_id = Some(client_id);
            jira_config.oauth_client_secret = Some(client_secret);
            if matches!(storage, StorageChoice::Keyring) {
                jira_config.credential_store = Some("keyring".into());
            }
        }
        AuthMethod::PersonalToken => {
            apply_token_storage(&storage, &mut jira_config, &config_dir)?;
        }
    }

    // Create the personal team directory and config.
    let team_dir = config_dir.join("teams").join("personal");
    std::fs::create_dir_all(&team_dir)?;

    let team_ref = TeamRef {
        id: "personal".into(),
        path: team_dir.to_string_lossy().into_owned(),
        file: None,
        backlog: None,
    };

    let config = Config {
        atlassian: jira_config.clone(),
        teams: vec![team_ref],
        ..Default::default()
    };

    println!();
    let config_style = prompt_config_style()?;

    // Write user config (jira credentials + team reference).
    let config_path = config_dir.join("config.json5");
    let user_json5 = match config_style {
        ConfigStyle::Minimal => json5::to_string(&config)?,
        ConfigStyle::Template => template_user_config(&base_url, &default_project, &jira_config),
    };
    std::fs::write(&config_path, &user_json5)?;
    println!("Config written to {}", config_path.display());

    // Write team config (sources, views, etc.).
    let team_config = default_personal_team_config(&default_project);
    let team_config_path = team_dir.join("do-next.json5");
    let team_json5 = match config_style {
        ConfigStyle::Minimal => json5::to_string(&team_config)?,
        ConfigStyle::Template => template_team_config(&default_project),
    };
    std::fs::write(&team_config_path, &team_json5)?;
    println!("Team config written to {}", team_config_path.display());

    let resolved = ResolvedTeam {
        id: "personal".into(),
        path: team_dir.to_string_lossy().into_owned(),
        normal_sources: team_config.sources.clone(),
        config: team_config,
        confluence: jira_config.clone(),
        atlassian: jira_config,
        open_slack_in_app: true,
        slack_team_id: None,
        grafana: None,
        gitlab: crate::config::types::ResolvedGitlab::default(),
        on_duty: false,
    };

    Ok(LoadedConfig {
        raw: config.clone(),
        config,
        teams: vec![resolved],
        load_errors: Vec::new(),
    })
}

// ── Auth reset ──────────────────────────────────────────────────────────────

/// Reconfigure authentication for an existing install without overwriting other config.
///
/// Detection and OAuth client credentials use `effective_jira` (user config
/// with company manifest values merged in); everything persisted lands on
/// `raw` — the on-disk config — so manifest values are never baked into the
/// user's file.
#[allow(clippy::too_many_lines)]
/// Reconfigure authentication for one Atlassian site.
///
/// `slot` decides which config block the result is written to: the user's
/// primary `atlassian:` block, or their `confluence:` override when the row is
/// a second site. Routing an override at the primary slot would rewrite the
/// wrong connection, so the caller must be explicit.
///
/// Writes land in a scratch config first and are applied to the slot at the
/// end. That keeps one copy of the write logic, and it means the keyring key is
/// derived from the *effective* base URL — company users have an empty one on
/// disk, which this used to work around by swapping the value in and out.
pub fn run_auth_reset(
    effective: &AtlassianConfig,
    raw: &mut Config,
    slot: &crate::auth::SlotRef,
    extra_scopes: crate::atlassian::oauth::ExtraScopes,
) -> Result<()> {
    if effective.base_url.is_empty() {
        return Err(anyhow::anyhow!(
            "No configuration found. Run do-next first to complete initial setup."
        ));
    }
    if let crate::auth::SlotRef::Team(team) = slot {
        return Err(anyhow::anyhow!(
            "This site comes from team '{team}'\u{2019}s own config, which may live in a \
             shared repository — do-next will not rewrite it.\n\
             Set its credentials there, or through the environment."
        ));
    }

    println!(
        "Reconfiguring Atlassian authentication for {}",
        effective.base_url
    );
    println!();

    let current_auth = detect_auth_method(effective);
    let auth_method = prompt_auth_method(Some(&current_auth))?;

    println!();
    let status = probe_credential_status(effective);
    let current_storage = detect_storage_method(effective);
    let storage = match auth_method {
        AuthMethod::OAuth => prompt_oauth_storage(Some(&current_storage))?,
        AuthMethod::PersonalToken => prompt_token_storage(
            Some(&current_storage),
            Some(&status),
            "DO_NEXT_ATLASSIAN_API_TOKEN",
        )?,
    };

    // The company manifest supplies the OAuth app when the user hasn't set
    // one; those creds must not be copied into the user's file, or rotation
    // in the config repo would stop propagating.
    let company_supplies_oauth = raw.company.is_some() && raw.atlassian.oauth_client_id.is_none();

    let config_dir = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine config directory"))?
        .join("do-next");

    // Scratch: the site's identity, none of its auth. Each branch fills in
    // only what it needs, and anything left unset is cleared on the slot.
    let mut draft = AtlassianConfig {
        base_url: effective.base_url.clone(),
        credential_key: effective.credential_key.clone(),
        ..Default::default()
    };

    match auth_method {
        AuthMethod::OAuth => {
            let (client_id, client_secret) = resolve_oauth_client_credentials(effective)?;
            let store = match storage {
                StorageChoice::Keyring => OAuthStore::Keyring,
                _ => OAuthStore::File,
            };
            crate::atlassian::oauth::run_oauth_flow(
                &client_id,
                &client_secret,
                store,
                extra_scopes,
            )?;
            let reused_company_app = company_supplies_oauth
                && effective.oauth_client_id.as_deref() == Some(client_id.as_str())
                && effective.oauth_client_secret.as_deref() == Some(client_secret.as_str());
            if !reused_company_app {
                draft.auth_method = Some("oauth".into());
                draft.oauth_client_id = Some(client_id);
                draft.oauth_client_secret = Some(client_secret);
            }
            if matches!(storage, StorageChoice::Keyring) {
                draft.credential_store = Some("keyring".into());
            }
        }
        AuthMethod::PersonalToken => {
            let current_email = effective.email.as_deref().unwrap_or("");
            let email_prompt = if current_email.is_empty() {
                "Atlassian account email: ".to_string()
            } else {
                format!("Atlassian account email [{current_email}]: ")
            };
            draft.email = Some(prompt(&email_prompt, Some(current_email))?);
            println!();

            apply_token_storage(&storage, &mut draft, &config_dir)?;

            // A company manifest with an OAuth app implies `oauth` for users
            // without an explicit method — switching to a token needs an
            // explicit override.
            if raw.company.is_some() {
                draft.auth_method = Some("basic".into());
            }
        }
    }

    apply_auth_to_slot(raw, slot, &draft);

    // Write updated config back.
    let config_path = config_dir.join("config.json5");
    if config_path.exists() {
        println!("Note: config file will be rewritten in minimal format (comments removed).");
    }
    std::fs::create_dir_all(&config_dir)?;
    let json5_content = json5::to_string(&raw)?;
    std::fs::write(&config_path, json5_content)?;
    println!("Config updated at {}", config_path.display());

    Ok(())
}

/// Copy the freshly chosen auth settings onto the config block `slot` names.
///
/// Fields the draft leaves unset are cleared, so switching from a token to
/// OAuth (or the reverse) does not strand the previous method's settings. The
/// OAuth app is the exception: it is only written when the draft carries one,
/// because reusing a company manifest's app deliberately leaves it out.
fn apply_auth_to_slot(raw: &mut Config, slot: &crate::auth::SlotRef, draft: &AtlassianConfig) {
    match slot {
        crate::auth::SlotRef::Primary => {
            let target = &mut raw.atlassian;
            target.email.clone_from(&draft.email);
            target
                .credential_command
                .clone_from(&draft.credential_command);
            target.credential_store.clone_from(&draft.credential_store);
            target.auth_method.clone_from(&draft.auth_method);
            if draft.oauth_client_id.is_some() {
                target.oauth_client_id.clone_from(&draft.oauth_client_id);
                target
                    .oauth_client_secret
                    .clone_from(&draft.oauth_client_secret);
            }
        }
        crate::auth::SlotRef::Override => {
            let target = raw
                .confluence
                .get_or_insert_with(AtlassianOverride::default);
            // Pin the site: this block exists precisely to point elsewhere.
            target.base_url = Some(draft.base_url.clone());
            target.email.clone_from(&draft.email);
            target
                .credential_command
                .clone_from(&draft.credential_command);
            target.credential_store.clone_from(&draft.credential_store);
            target.auth_method.clone_from(&draft.auth_method);
            if draft.oauth_client_id.is_some() {
                target.oauth_client_id.clone_from(&draft.oauth_client_id);
                target
                    .oauth_client_secret
                    .clone_from(&draft.oauth_client_secret);
            }
        }
        // Refused up front in `run_auth_reset`.
        crate::auth::SlotRef::Team(_) => {}
    }
}

// ── Team setup (no teams configured) ────────────────────────────────────────

const TEAM_SETUP_COUNT: usize = 3;

const TEAM_SETUP_LABELS: [&str; TEAM_SETUP_COUNT] = [
    "Create personal space",
    "Use existing config  ",
    "Join a company       ",
];

const TEAM_SETUP_DESCRIPTIONS: [&str; TEAM_SETUP_COUNT] = [
    "create a local team config for your personal sources",
    "provide a path to an existing team config (e.g. a cloned git repo)",
    "clone your company's config repo and pick teams from its catalog",
];

/// Interactive prompt when config exists but has no teams.
/// Adds at least one team to the config and returns the updated `LoadedConfig`.
pub fn run_team_setup(config: &mut Config) -> Result<LoadedConfig> {
    println!("No teams configured. Let's set one up.\n");

    let tags = vec![String::new(); TEAM_SETUP_COUNT];
    let choice = run_selection(
        "How would you like to add a team?",
        &TEAM_SETUP_LABELS,
        &TEAM_SETUP_DESCRIPTIONS,
        &tags,
        0,
        None,
    )?;

    let config_dir = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine config directory"))?
        .join("do-next");

    if choice == 2 {
        // Join a company: writes the config itself, then reload from disk so
        // the manifest merge and team resolution run through the normal path.
        println!();
        let source = prompt("Company config repo (git URL or local path): ", None)?;
        company::join_company_into(config, &source)?;
        return crate::config::load();
    }

    let (team_ref, team_config, team_jira) = if choice == 0 {
        // Create personal space
        let team_dir = config_dir.join("teams").join("personal");
        std::fs::create_dir_all(&team_dir)?;

        let default_project = &config.atlassian.default_project;
        let team_config_path = team_dir.join("do-next.json5");
        let tc = if team_config_path.exists() {
            // Reuse existing team config
            let raw = std::fs::read_to_string(&team_config_path)?;
            let existing: TeamConfig = json5::from_str(&raw)?;
            println!(
                "Using existing team config at {}",
                team_config_path.display()
            );
            existing
        } else {
            let tc = default_personal_team_config(default_project);
            let json5_content = template_team_config(default_project);
            std::fs::write(&team_config_path, &json5_content)?;
            println!("Team config created at {}", team_config_path.display());
            tc
        };

        let tr = TeamRef {
            id: "personal".into(),
            path: team_dir.to_string_lossy().into_owned(),
            file: None,
            backlog: None,
        };
        (tr, tc, config.atlassian.clone())
    } else {
        // Use existing path
        println!();
        let path = prompt("Path to team config directory: ", None)?;
        let expanded = crate::config::expand_tilde(&path);
        let file_name = "do-next.json5";
        let config_path = expanded.join(file_name);
        if !config_path.exists() {
            return Err(anyhow::anyhow!(
                "No {} found at {}",
                file_name,
                expanded.display()
            ));
        }

        println!();
        let id = prompt("Team ID (short name for tab label): ", None)?;

        let raw = std::fs::read_to_string(&config_path)?;
        let tc: TeamConfig = json5::from_str(&raw)?;

        let jira = if let Some(ref overlay) = tc.atlassian {
            let mut j = config.atlassian.clone();
            crate::config::apply_team_atlassian_override(&mut j, overlay);
            j
        } else {
            config.atlassian.clone()
        };

        let tr = TeamRef {
            id: id.clone(),
            path: path.clone(),
            file: None,
            backlog: None,
        };
        println!("Team '{id}' added from {path}");
        (tr, tc, jira)
    };

    config.teams.push(team_ref.clone());

    // Save updated config
    let config_path = config_dir.join("config.json5");
    let json5_content = json5::to_string(&config)?;
    std::fs::write(&config_path, &json5_content)?;
    println!("Config updated at {}", config_path.display());

    let resolved = ResolvedTeam {
        id: team_ref.id,
        path: team_ref.path,
        normal_sources: team_config.sources.clone(),
        config: team_config,
        confluence: team_jira.clone(),
        atlassian: team_jira,
        open_slack_in_app: true,
        slack_team_id: None,
        grafana: None,
        gitlab: crate::config::types::ResolvedGitlab::default(),
        on_duty: false,
    };

    Ok(LoadedConfig {
        config: config.clone(),
        raw: config.clone(),
        teams: vec![resolved],
        load_errors: Vec::new(),
    })
}

// ── Token storage application ───────────────────────────────────────────────

pub(super) fn apply_token_storage(
    storage: &StorageChoice,
    atlassian: &mut AtlassianConfig,
    config_dir: &std::path::Path,
) -> Result<()> {
    match storage {
        StorageChoice::Keyring => {
            let key = atlassian
                .credential_key
                .as_deref()
                .unwrap_or(&atlassian.base_url)
                .to_string();
            check_keyring_available(&key)?;
            let entry = keyring::Entry::new("do-next", &key)
                .map_err(|e| anyhow::anyhow!("Failed to access keyring: {e}"))?;

            let already_exists = match entry.get_password() {
                Ok(_) => true,
                Err(keyring::Error::NoEntry) => false,
                Err(e) => return Err(anyhow::anyhow!("Keyring error: {e}")),
            };

            if already_exists {
                println!("An API token is already stored in the keyring for this URL.");
                let reuse = prompt_yes_no("Use the existing token? [Y/n]: ", true)?;
                if !reuse {
                    print_api_token_instructions();
                    let token = prompt_masked("API token: ")?;
                    entry
                        .set_password(&token)
                        .map_err(|e| anyhow::anyhow!("Failed to store token in keyring: {e}"))?;
                    println!("API token updated in system keyring.");
                }
            } else {
                print_api_token_instructions();
                let token = prompt_masked("API token: ")?;
                entry
                    .set_password(&token)
                    .map_err(|e| anyhow::anyhow!("Failed to store token in keyring: {e}"))?;
                println!("API token stored in system keyring.");
            }

            atlassian.credential_store = Some("keyring".into());
        }

        StorageChoice::File => {
            print_api_token_instructions();
            let token = prompt_masked("API token: ")?;

            let creds_path = config_dir.join("credentials.json5");
            // Merge, never overwrite: the file also holds the GitLab and
            // Grafana tokens, and writing it whole used to destroy them.
            let existing = match std::fs::read_to_string(&creds_path) {
                Ok(content) => Some(content),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(e.into()),
            };
            let creds_content = crate::config::credentials::merge_token_into_credentials(
                existing.as_deref(),
                crate::config::credentials::ATLASSIAN_CREDENTIALS_SECTION,
                &token,
            )?;
            std::fs::create_dir_all(config_dir)?;
            std::fs::write(&creds_path, &creds_content)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&creds_path, std::fs::Permissions::from_mode(0o600))?;
            }
            println!("Credentials written to {}", creds_path.display());
        }

        StorageChoice::Command => {
            println!("Enter the shell command whose stdout is your Atlassian API token.");
            println!("Examples:  pass show atlassian/do-next");
            println!("           op read 'op://Private/Jira/credential'");
            println!();
            let cmd = prompt("Credential command: ", None)?;
            atlassian.credential_command = Some(cmd);
        }

        StorageChoice::Env => {
            println!();
            println!("Set the following environment variables before running do-next:");
            println!("  DO_NEXT_ATLASSIAN_EMAIL=<your-email>");
            println!("  DO_NEXT_ATLASSIAN_API_TOKEN=<your-api-token>");
            println!();
        }
    }
    Ok(())
}

// ── OAuth client credentials ───────────────────────────────────────────────

/// Resolve OAuth `client_id` and `client_secret`.
///
/// Precedence:
/// 1. Environment variables (`DO_NEXT_OAUTH_CLIENT_ID` + `DO_NEXT_OAUTH_CLIENT_SECRET`)
/// 2. Config fields (`jira.oauth_client_id` + `jira.oauth_client_secret`)
/// 3. Interactive prompt with setup instructions
fn resolve_oauth_client_credentials(jira: &AtlassianConfig) -> Result<(String, String)> {
    // 1. Environment variables.
    if let (Ok(id), Ok(secret)) = (
        std::env::var("DO_NEXT_OAUTH_CLIENT_ID"),
        std::env::var("DO_NEXT_OAUTH_CLIENT_SECRET"),
    ) {
        return Ok((id, secret));
    }

    // 2. Config fields — offer to reuse or replace.
    if let (Some(id), Some(secret)) = (&jira.oauth_client_id, &jira.oauth_client_secret)
        && !id.is_empty()
        && !secret.is_empty()
    {
        println!("An OAuth app is already configured (client_id: {id}).");
        let reuse = prompt_yes_no("Use the existing app? [Y/n]: ", true)?;
        if reuse {
            return Ok((id.clone(), secret.clone()));
        }
    }

    // 3. Interactive prompt.
    prompt_oauth_client_credentials()
}

fn prompt_oauth_client_credentials() -> Result<(String, String)> {
    print_oauth_app_instructions();
    let client_id = prompt("Client ID: ", None)?;
    let client_secret = prompt_masked("Client Secret: ")?;
    if client_id.is_empty() || client_secret.is_empty() {
        return Err(anyhow::anyhow!(
            "Both Client ID and Client Secret are required.\n\
             Run `do-next auth` to try again."
        ));
    }
    Ok((client_id, client_secret))
}

fn print_oauth_app_instructions() {
    println!();
    println!("Atlassian OAuth App Setup");
    println!("  do-next needs an OAuth 2.0 (3LO) app to authenticate with Jira Cloud.");
    println!();
    println!("  1. Go to https://developer.atlassian.com/console/myapps/");
    println!("  2. Click \"Create\" → \"OAuth 2.0 integration\"");
    println!("  3. Give it a name (e.g. \"do-next\")");
    println!("  4. Under \"Authorization\", add a callback URL:");
    println!("       http://localhost:19872/callback");
    println!("  5. Under \"Permissions\", add the Jira API with these scopes:");
    println!("       read:jira-work, write:jira-work, read:jira-user");
    println!("     Also enable: offline_access (for token refresh)");
    println!("  6. Under \"Settings\", copy the Client ID and Secret");
    println!();
}

// ── Detection helpers ───────────────────────────────────────────────────────

fn detect_auth_method(jira: &AtlassianConfig) -> AuthMethod {
    if jira.auth_method.as_deref() == Some("oauth") {
        AuthMethod::OAuth
    } else {
        AuthMethod::PersonalToken
    }
}

fn detect_storage_method(jira: &AtlassianConfig) -> StorageChoice {
    if jira.credential_command.is_some() {
        StorageChoice::Command
    } else if jira.credential_store.as_deref() == Some("keyring") {
        StorageChoice::Keyring
    } else {
        StorageChoice::File
    }
}

fn probe_credential_status(jira: &AtlassianConfig) -> CredentialStatus {
    let env_set =
        crate::config::credentials::pick_env_var(&crate::config::credentials::ATLASSIAN_TOKEN_VARS)
            .is_some();

    // Section-aware: a file holding only a gitlab token is not an Atlassian
    // credential, though the old existence check reported it as one.
    let file_exists = crate::config::credentials::stored_token_present(
        crate::config::credentials::ATLASSIAN_CREDENTIALS_SECTION,
    );

    let keyring_key = jira.credential_key.as_deref().unwrap_or(&jira.base_url);
    let keyring_found = keyring::Entry::new("do-next", keyring_key)
        .map(|e| e.get_password().is_ok())
        .unwrap_or(false);

    CredentialStatus {
        env_set,
        file_exists,
        keyring_found,
        command: jira.credential_command.clone(),
    }
}

// ── Generic selection UI ────────────────────────────────────────────────────

/// Render a vertical selection list and return the chosen index.
/// One row of a single-select list.
///
/// `sublabel` renders as an indented second line — used for the Atlassian
/// product list, where the row's identity is the site and its detail is what
/// that site serves. `selectable: false` is a dimmed row the cursor skips:
/// separators, and headings that exist only to group.
pub(super) struct SelectRow {
    pub label: String,
    pub description: String,
    pub tag: String,
    pub sublabel: Option<String>,
    pub selectable: bool,
}

impl SelectRow {
    /// A normal, choosable row.
    pub fn new(
        label: impl Into<String>,
        description: impl Into<String>,
        tag: impl Into<String>,
    ) -> Self {
        Self {
            label: label.into(),
            description: description.into(),
            tag: tag.into(),
            sublabel: None,
            selectable: true,
        }
    }

    /// An inert divider. The cursor jumps over it in both directions.
    pub fn separator() -> Self {
        Self {
            label: "\u{2500}".repeat(8),
            description: String::new(),
            tag: String::new(),
            sublabel: None,
            selectable: false,
        }
    }

    pub fn with_sublabel(mut self, sublabel: impl Into<String>) -> Self {
        self.sublabel = Some(sublabel.into());
        self
    }
}

/// Lines the list occupies on screen — the distance the redraw moves back up.
///
/// Rows with a sublabel take two. Getting this wrong smears the menu on every
/// keypress, which is why it is a separate, tested function.
fn rendered_height(rows: &[SelectRow]) -> u16 {
    let lines = rows.len() + rows.iter().filter(|r| r.sublabel.is_some()).count();
    u16::try_from(lines).unwrap_or(u16::MAX)
}

/// Width of the label column: the widest label, in characters.
///
/// Counted in `char`s rather than bytes so a non-ASCII label still lines up.
fn label_width(rows: &[SelectRow]) -> usize {
    rows.iter()
        .filter(|r| r.selectable)
        .map(|r| r.label.chars().count())
        .max()
        .unwrap_or(0)
}

/// The next selectable row in direction `delta`, clamped at both ends.
///
/// Returns `from` when there is nowhere to go — including the degenerate case
/// where no row is selectable, which must terminate rather than spin.
fn next_selectable(rows: &[SelectRow], from: usize, delta: isize) -> usize {
    let mut i = from;
    loop {
        let Some(next) = i.checked_add_signed(delta) else {
            return from;
        };
        if next >= rows.len() {
            return from;
        }
        if rows[next].selectable {
            return next;
        }
        i = next;
    }
}

/// The starting cursor: `wanted` if it is selectable, else the nearest
/// selectable row after it, else before it.
///
/// Guards a stale cursor — the row set is rebuilt between menu passes and can
/// shrink, leaving a remembered index past the end or sitting on a separator.
fn normalize_default(rows: &[SelectRow], wanted: usize) -> usize {
    if rows.get(wanted).is_some_and(|r| r.selectable) {
        return wanted;
    }
    let clamped = wanted.min(rows.len().saturating_sub(1));
    let forward = next_selectable(rows, clamped, 1);
    if rows.get(forward).is_some_and(|r| r.selectable) {
        return forward;
    }
    let back = next_selectable(rows, clamped, -1);
    if rows.get(back).is_some_and(|r| r.selectable) {
        return back;
    }
    0
}

fn render_rows(rows: &[SelectRow], selected: usize, confirmed: bool) -> Result<()> {
    let width = label_width(rows);
    for (i, row) in rows.iter().enumerate() {
        let label = format!("{:<width$}", row.label, width = width);
        let text = format!("{label}   {}{}", row.description, row.tag);
        if !row.selectable {
            crossterm::execute!(
                io::stdout(),
                SetForegroundColor(Color::DarkGrey),
                Print(format!("    {}\r\n", row.label)),
                ResetColor,
            )?;
        } else if i == selected && confirmed {
            crossterm::execute!(
                io::stdout(),
                SetForegroundColor(Color::Green),
                Print("  \u{2713} "),
                ResetColor,
                Print(format!("{text}\r\n")),
            )?;
        } else if i == selected {
            print!("  > {text}\r\n");
        } else {
            print!("    {text}\r\n");
        }
        if let Some(sublabel) = &row.sublabel {
            // Indented under the label column so it reads as detail, not a row.
            crossterm::execute!(
                io::stdout(),
                SetForegroundColor(Color::DarkGrey),
                Print(format!(
                    "    {:<width$}   {sublabel}\r\n",
                    "",
                    width = width
                )),
                ResetColor,
            )?;
        }
    }
    Ok(())
}

/// Render a single-select list and return the chosen index.
///
/// `Ok(None)` means the user backed out (Esc, `q`, Ctrl-C) — a looping menu
/// needs that as an answer rather than an error. Labels are padded here, so
/// callers building rows at runtime never have to.
pub(super) fn run_menu(title: &str, rows: &[SelectRow], default: usize) -> Result<Option<usize>> {
    if !rows.iter().any(|r| r.selectable) {
        return Ok(None);
    }

    println!("{title}");
    println!();
    let mut selected = normalize_default(rows, default);
    render_rows(rows, selected, false)?;
    io::stdout().flush()?;

    enable_raw_mode()?;
    let lines = rendered_height(rows);

    // Leaving raw mode on every exit path, including the error one, is what
    // keeps a failure from handing back an unusable terminal.
    let outcome = menu_loop(rows, &mut selected, lines);
    disable_raw_mode()?;
    println!();

    match outcome? {
        MenuExit::Chosen => {
            // Repaint with the green check now that raw mode is off.
            Ok(Some(selected))
        }
        MenuExit::Cancelled => Ok(None),
    }
}

enum MenuExit {
    Chosen,
    Cancelled,
}

fn menu_loop(rows: &[SelectRow], selected: &mut usize, lines: u16) -> Result<MenuExit> {
    loop {
        match crossterm::event::read() {
            Ok(Event::Key(KeyEvent {
                code, modifiers, ..
            })) => {
                let delta = match code {
                    KeyCode::Up | KeyCode::Char('k') => Some(-1),
                    KeyCode::Down | KeyCode::Char('j') => Some(1),
                    _ => None,
                };

                if let Some(delta) = delta {
                    *selected = next_selectable(rows, *selected, delta);
                    crossterm::execute!(
                        io::stdout(),
                        MoveUp(lines),
                        Clear(ClearType::FromCursorDown)
                    )?;
                    render_rows(rows, *selected, false)?;
                    io::stdout().flush()?;
                    continue;
                }

                match code {
                    KeyCode::Enter => {
                        crossterm::execute!(
                            io::stdout(),
                            MoveUp(lines),
                            Clear(ClearType::FromCursorDown)
                        )?;
                        render_rows(rows, *selected, true)?;
                        io::stdout().flush()?;
                        return Ok(MenuExit::Chosen);
                    }
                    KeyCode::Esc | KeyCode::Char('q') => return Ok(MenuExit::Cancelled),
                    KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(MenuExit::Cancelled);
                    }
                    _ => {}
                }
            }
            Ok(_) => {}
            Err(e) => return Err(e.into()),
        }
    }
}

/// Single-select over borrowed parallel slices, for the fixed-size menus.
///
/// A thin adapter over [`run_menu`]: every row is selectable, the `← current`
/// marker folds into the row's tag, and backing out stays an error because
/// these callers are wizard steps with nowhere to go back to.
pub(super) fn run_selection(
    title: &str,
    labels: &[&str],
    descriptions: &[&str],
    tags: &[String],
    default: usize,
    current_idx: Option<usize>,
) -> Result<usize> {
    let rows: Vec<SelectRow> = labels
        .iter()
        .enumerate()
        .map(|(i, label)| {
            let tag = tags.get(i).map_or("", String::as_str);
            let marker = if current_idx == Some(i) {
                "  \u{2190} current"
            } else {
                ""
            };
            SelectRow::new(
                (*label).to_string(),
                descriptions.get(i).copied().unwrap_or("").to_string(),
                format!("{tag}{marker}"),
            )
        })
        .collect();

    run_menu(title, &rows, default)?.ok_or_else(|| Cancelled.into())
}

/// One row of a multi-select list. `parent` links a dependent sub-row to an
/// earlier row: the sub-row only toggles (and only counts as selected) while
/// its parent is checked, and renders greyed out otherwise.
pub(super) struct MultiRow {
    pub label: String,
    pub description: String,
    pub tag: String,
    pub checked: bool,
    pub selectable: bool,
    pub parent: Option<usize>,
}

/// A row can be toggled when it's selectable and its parent (if any) is
/// currently checked.
fn row_enabled(rows: &[MultiRow], checked: &[bool], i: usize) -> bool {
    rows[i].selectable && rows[i].parent.is_none_or(|p| checked[p])
}

/// Checked rows that actually count. Toggling keeps sub-rows of unchecked
/// parents cleared; the parent guard here covers stray preselections.
fn effective_indices(rows: &[MultiRow], checked: &[bool]) -> Vec<usize> {
    (0..rows.len())
        .filter(|&i| checked[i] && rows[i].parent.is_none_or(|p| checked[p]))
        .collect()
}

/// Flip row `i`; unchecking a parent also clears its sub-rows, so a greyed
/// out checkbox is never left ticked — what's shown is what confirms.
fn toggle_row(rows: &[MultiRow], checked: &mut [bool], i: usize) {
    checked[i] = !checked[i];
    if !checked[i] {
        for (j, row) in rows.iter().enumerate() {
            if row.parent == Some(i) {
                checked[j] = false;
            }
        }
    }
}

/// Render a vertical multi-select list and return the effectively checked
/// indices. Space toggles, Enter confirms (at least one top-level selection
/// required unless `allow_empty`), Esc/q cancels. Rows with `selectable ==
/// false` can't be toggled; sub-rows only while their parent is checked.
pub(super) fn run_multi_selection(
    title: &str,
    rows: &[MultiRow],
    allow_empty: bool,
) -> Result<Vec<usize>> {
    let count = rows.len();

    println!("{title}");
    println!();
    let mut checked: Vec<bool> = rows.iter().map(|r| r.checked).collect();
    let mut cursor = 0;
    render_multi_options(rows, &checked, cursor, false)?;
    render_multi_hint()?;
    io::stdout().flush()?;

    enable_raw_mode()?;

    // Options plus the hint line.
    #[allow(clippy::cast_possible_truncation)]
    let lines = count as u16 + 1;

    let redraw = |checked: &[bool], cursor: usize, confirmed: bool| -> Result<()> {
        crossterm::execute!(
            io::stdout(),
            MoveUp(lines),
            Clear(ClearType::FromCursorDown)
        )?;
        render_multi_options(rows, checked, cursor, confirmed)?;
        render_multi_hint()?;
        io::stdout().flush()?;
        Ok(())
    };

    loop {
        match crossterm::event::read() {
            Ok(Event::Key(KeyEvent {
                code, modifiers, ..
            })) => match code {
                KeyCode::Up | KeyCode::Char('k') => {
                    cursor = cursor.saturating_sub(1);
                    redraw(&checked, cursor, false)?;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    cursor = (cursor + 1).min(count - 1);
                    redraw(&checked, cursor, false)?;
                }
                KeyCode::Char(' ') => {
                    if row_enabled(rows, &checked, cursor) {
                        toggle_row(rows, &mut checked, cursor);
                        redraw(&checked, cursor, false)?;
                    }
                }
                KeyCode::Enter => {
                    let effective = effective_indices(rows, &checked);
                    if allow_empty || effective.iter().any(|&i| rows[i].parent.is_none()) {
                        redraw(&checked, cursor, true)?;
                        disable_raw_mode()?;
                        println!();
                        return Ok(effective);
                    }
                    // At least one selection required — ignore Enter.
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    disable_raw_mode()?;
                    println!();
                    return Err(Cancelled.into());
                }
                KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                    disable_raw_mode()?;
                    println!();
                    return Err(Cancelled.into());
                }
                _ => {}
            },
            Ok(_) => {}
            Err(e) => {
                disable_raw_mode()?;
                println!();
                return Err(e.into());
            }
        }
    }
}

fn render_multi_options(
    rows: &[MultiRow],
    checked: &[bool],
    cursor: usize,
    confirmed: bool,
) -> Result<()> {
    for (i, row) in rows.iter().enumerate() {
        let pointer = if i == cursor && !confirmed { ">" } else { " " };
        // Sub-rows shift right as a whole, checkbox included.
        let indent = if row.parent.is_some() { "  " } else { "" };
        let text = format!("{}   {}{}", row.label, row.description, row.tag);
        let effective = checked[i] && row.parent.is_none_or(|p| checked[p]);
        if effective && confirmed {
            crossterm::execute!(
                io::stdout(),
                Print(format!("  {indent}")),
                SetForegroundColor(Color::Green),
                Print("[\u{2713}] "),
                ResetColor,
                Print(format!("{text}\r\n")),
            )?;
        } else if !row.selectable {
            crossterm::execute!(
                io::stdout(),
                Print(format!("{pointer} {indent}")),
                SetForegroundColor(Color::DarkGrey),
                Print(format!("[-] {text}\r\n")),
                ResetColor,
            )?;
        } else if !row_enabled(rows, checked, i) {
            // Sub-row whose parent is unchecked: inert and always unticked
            // (toggling the parent off clears it), so the grey box never
            // suggests a choice that won't apply.
            crossterm::execute!(
                io::stdout(),
                Print(format!("{pointer} {indent}")),
                SetForegroundColor(Color::DarkGrey),
                Print(format!("[ ] {text}\r\n")),
                ResetColor,
            )?;
        } else {
            let mark = if checked[i] { "[x]" } else { "[ ]" };
            print!("{pointer} {indent}{mark} {text}\r\n");
        }
    }
    Ok(())
}

/// Hint line for the multi-select, following the semantic color convention:
/// Blue = available action, Green = confirm, Magenta = back/cancel.
fn render_multi_hint() -> Result<()> {
    crossterm::execute!(
        io::stdout(),
        SetForegroundColor(Color::Blue),
        Print("  space"),
        ResetColor,
        Print(" toggle  "),
        SetForegroundColor(Color::Blue),
        Print("\u{2191}\u{2193}/jk"),
        ResetColor,
        Print(" move  "),
        SetForegroundColor(Color::Green),
        Print("enter"),
        ResetColor,
        Print(" confirm  "),
        SetForegroundColor(Color::Magenta),
        Print("esc"),
        ResetColor,
        Print(" cancel\r\n"),
    )?;
    Ok(())
}

// ── Prompt: auth method ─────────────────────────────────────────────────────

fn prompt_auth_method(current: Option<&AuthMethod>) -> Result<AuthMethod> {
    let current_idx = current.map(|c| match c {
        AuthMethod::PersonalToken => 0,
        AuthMethod::OAuth => 1,
    });
    let default = current_idx.unwrap_or(0);
    let tags = vec![String::new(); AUTH_METHOD_COUNT];

    let idx = run_selection(
        "How would you like to authenticate?",
        &AUTH_METHOD_LABELS,
        &AUTH_METHOD_DESCRIPTIONS,
        &tags,
        default,
        current_idx,
    )?;

    Ok(match idx {
        0 => AuthMethod::PersonalToken,
        _ => AuthMethod::OAuth,
    })
}

// ── Prompt: OAuth storage ───────────────────────────────────────────────────

pub(super) fn prompt_oauth_storage(current: Option<&StorageChoice>) -> Result<StorageChoice> {
    let current_idx = current.and_then(|c| match c {
        StorageChoice::Keyring => Some(0),
        StorageChoice::File => Some(1),
        _ => None,
    });
    let default = current_idx.unwrap_or(0);
    let tags = vec![String::new(); OAUTH_STORAGE_COUNT];

    let idx = run_selection(
        "Where should OAuth tokens be stored?",
        &OAUTH_STORAGE_LABELS,
        &OAUTH_STORAGE_DESCRIPTIONS,
        &tags,
        default,
        current_idx,
    )?;

    Ok(match idx {
        0 => StorageChoice::Keyring,
        _ => StorageChoice::File,
    })
}

// ── Prompt: token storage ───────────────────────────────────────────────────

pub(super) fn prompt_token_storage(
    current: Option<&StorageChoice>,
    status: Option<&CredentialStatus>,
    env_var: &str,
) -> Result<StorageChoice> {
    let current_idx = current.map(|c| match c {
        StorageChoice::Keyring => 0,
        StorageChoice::File => 1,
        StorageChoice::Command => 2,
        StorageChoice::Env => 3,
    });
    let default = current_idx.unwrap_or(0);

    let tags = build_token_storage_tags(status);
    let descriptions = token_storage_descriptions(env_var);
    let descriptions: Vec<&str> = descriptions.iter().map(String::as_str).collect();

    let idx = run_selection(
        "How would you like to store your API token?",
        &TOKEN_STORAGE_LABELS,
        &descriptions,
        &tags,
        default,
        current_idx,
    )?;

    Ok(match idx {
        0 => StorageChoice::Keyring,
        1 => StorageChoice::File,
        2 => StorageChoice::Command,
        _ => StorageChoice::Env,
    })
}

fn build_token_storage_tags(status: Option<&CredentialStatus>) -> Vec<String> {
    vec![
        // Keyring
        status
            .map_or("", |s| {
                if s.keyring_found {
                    "  [entry found]"
                } else {
                    "  [no entry]"
                }
            })
            .to_string(),
        // File
        status
            .map_or("", |s| {
                if s.file_exists {
                    "  [token found]"
                } else {
                    "  [not found]"
                }
            })
            .to_string(),
        // Command
        status
            .map(|s| {
                s.command.as_ref().map_or_else(
                    || "  [not set]".to_string(),
                    |cmd| {
                        let short = if cmd.len() > 25 {
                            format!("{}…", &cmd[..25])
                        } else {
                            cmd.clone()
                        };
                        format!("  [set: {short}]")
                    },
                )
            })
            .unwrap_or_default(),
        // Env
        status
            .map_or("", |s| if s.env_set { "  [set]" } else { "  [not set]" })
            .to_string(),
    ]
}

// ── Config style prompt ─────────────────────────────────────────────────────

fn prompt_config_style() -> Result<ConfigStyle> {
    println!("How would you like to save the config file?");
    println!();
    println!("  [1] Minimal            only the settings you just entered");
    println!("  [2] Annotated template all options as commented-out examples");
    println!();
    print!("Choice [1-2]: ");
    io::stdout().flush()?;

    enable_raw_mode()?;
    loop {
        match crossterm::event::read() {
            Ok(Event::Key(KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                ..
            })) => {
                if c == 'c' && modifiers.contains(KeyModifiers::CONTROL) {
                    disable_raw_mode()?;
                    println!();
                    return Err(Cancelled.into());
                }
                match c {
                    '1' => {
                        disable_raw_mode()?;
                        println!("1");
                        return Ok(ConfigStyle::Minimal);
                    }
                    '2' => {
                        disable_raw_mode()?;
                        println!("2");
                        return Ok(ConfigStyle::Template);
                    }
                    _ => {}
                }
            }
            Ok(_) => {}
            Err(e) => {
                disable_raw_mode()?;
                println!();
                return Err(e.into());
            }
        }
    }
}

// ── Template config ─────────────────────────────────────────────────────────

/// Generate a template user config (jira credentials + team references).
fn template_user_config(
    base_url: &str,
    default_project: &str,
    jira_config: &crate::config::types::AtlassianConfig,
) -> String {
    let email = jira_config.email.as_deref().unwrap_or("you@example.com");

    let cred_line = if jira_config.auth_method.as_deref() == Some("oauth") {
        let id = jira_config.oauth_client_id.as_deref().unwrap_or("");
        let secret = jira_config.oauth_client_secret.as_deref().unwrap_or("");
        format!(
            "    auth_method: \"oauth\",\n\
             \x20   oauth_client_id: \"{id}\",\n\
             \x20   oauth_client_secret: \"{secret}\",\n"
        )
    } else {
        match &jira_config.credential_command {
            Some(cmd) => format!("    credential_command: \"{cmd}\",\n"),
            None if jira_config.credential_store.as_deref() == Some("keyring") => {
                "    credential_store: \"keyring\",\n".to_string()
            }
            None => String::new(),
        }
    };

    let cred_comments = if jira_config.auth_method.as_deref() == Some("oauth") {
        "    // credential_store: \"keyring\",  // also used for OAuth token storage\n".to_string()
    } else if jira_config.credential_command.is_some() {
        "    // credential_store: \"keyring\",\n    // credential_key: \"jira.example.com\",  // optional label\n".to_string()
    } else if jira_config.credential_store.as_deref() == Some("keyring") {
        "    // credential_key: \"jira.example.com\",  // optional label\n    // credential_command: \"pass show jira/do-next\",\n".to_string()
    } else {
        "    // credential_store: \"keyring\",\n    // credential_command: \"pass show atlassian/do-next\",\n    // Env: DO_NEXT_ATLASSIAN_API_TOKEN=<your-api-token>\n".to_string()
    };

    let config_dir = dirs::config_dir()
        .map(|d| d.join("do-next").join("teams").join("personal"))
        .unwrap_or_default();
    let team_path = config_dir.to_string_lossy();

    format!(
        r#"{{
  jira: {{
    base_url: "{base_url}",
    default_project: "{default_project}",
    email: "{email}",

    // Authentication — API token resolution (first found wins):
    //   1. Env:              DO_NEXT_ATLASSIAN_API_TOKEN=<api-token>
    //   2. External command: credential_command: "..."
    //   3. System keyring:   credential_store: "keyring"
    //   4. Credentials file: ~/.config/do-next/credentials.json5
    //   Or use OAuth:        auth_method: "oauth"
    // Email override:        DO_NEXT_ATLASSIAN_EMAIL=<email>
{cred_line}{cred_comments}  }},

  // Teams — each team has its own sources, views, and display config.
  // Add more teams by cloning a shared config repo and adding an entry here.
  teams: [
    {{
      id: "personal",
      path: "{team_path}",
    }},
    // {{
    //   id: "platform",
    //   path: "~/work/platform-do-next-config",
    //   // file: "do-next.json5",  // optional, defaults to "do-next.json5"
    // }},
  ],

  // cache: {{
  //   enabled: true,
  //   max_age_seconds: 300,
  // }},
}}
"#
    )
}

/// Build the default personal team config with a "`my_tasks`" source.
fn default_personal_team_config(default_project: &str) -> TeamConfig {
    use crate::config::types::SourceConfig;
    TeamConfig {
        sources: vec![SourceConfig {
            id: "my_tasks".into(),
            display_name: Some("My tasks".into()),
            jql: format!(
                "assignee = currentUser() AND project = {default_project} AND statusCategory != Done ORDER BY priority DESC, updated DESC"
            ),
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// Generate a template team config (sources, views, etc.).
fn template_team_config(default_project: &str) -> String {
    format!(
        r#"{{
  sources: [
    {{
      id: "my_tasks",
      display_name: "My tasks",
      jql: "assignee = currentUser() AND project = {default_project} AND statusCategory != Done ORDER BY priority DESC, updated DESC",
    }},
  ],
}}
"#
    )
}

// ── Utility functions ───────────────────────────────────────────────────────

fn print_api_token_instructions() {
    println!();
    println!("Atlassian API Token");
    println!("  One token covers this Atlassian site: Jira issues, Confluence");
    println!("  pages and boards alike.");
    println!("  To create one, go to:");
    println!("    https://id.atlassian.com/manage-profile/security/api-tokens");
    println!("  Click \"Create API token\", give it a label, and copy the value.");
    println!();
    println!("  Input masked with *. Backspace on an empty field hides the input.");
    println!();
}

fn check_keyring_available(key: &str) -> Result<()> {
    let entry = keyring::Entry::new("do-next", key)
        .map_err(|e| anyhow::anyhow!("System keyring is not accessible: {e}"))?;
    match entry.get_password() {
        Ok(_) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(anyhow::anyhow!("System keyring is not accessible: {e}")),
    }
}

/// Prompt for a yes/no answer. `default` sets which is chosen on bare Enter.
pub fn prompt_yes_no(message: &str, default: bool) -> Result<bool> {
    print!("{message}");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(match input.trim().to_lowercase().as_str() {
        "y" | "yes" => true,
        "n" | "no" => false,
        _ => default,
    })
}

pub(super) fn prompt(message: &str, default: Option<&str>) -> Result<String> {
    print!("{message}");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let trimmed = input.trim().to_string();
    if trimmed.is_empty()
        && let Some(d) = default
    {
        return Ok(d.to_string());
    }
    Ok(trimmed)
}

pub(super) fn prompt_masked(message: &str) -> Result<String> {
    print!("{message}");
    io::stdout().flush()?;

    enable_raw_mode()?;
    let mut token = String::new();
    let mut echo = true;
    loop {
        match crossterm::event::read() {
            Ok(Event::Key(KeyEvent {
                code: KeyCode::Enter,
                ..
            })) => break,
            Ok(Event::Key(KeyEvent {
                code: KeyCode::Char('c'),
                modifiers,
                ..
            })) if modifiers.contains(KeyModifiers::CONTROL) => {
                disable_raw_mode()?;
                println!();
                return Err(Cancelled.into());
            }
            Ok(Event::Key(KeyEvent {
                code: KeyCode::Char(c),
                ..
            })) => {
                token.push(c);
                if echo {
                    print!("*");
                    io::stdout().flush()?;
                }
            }
            Ok(Event::Key(KeyEvent {
                code: KeyCode::Backspace,
                ..
            })) => {
                if token.pop().is_some() {
                    if echo {
                        print!("\x08 \x08");
                        io::stdout().flush()?;
                    }
                } else if echo {
                    echo = false;
                    print!("[no echo]");
                    io::stdout().flush()?;
                }
            }
            Ok(_) => {}
            Err(e) => {
                disable_raw_mode()?;
                println!();
                return Err(e.into());
            }
        }
    }
    disable_raw_mode()?;
    println!();
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::{SelectRow, label_width, next_selectable, normalize_default, rendered_height};

    fn rows(spec: &[bool]) -> Vec<SelectRow> {
        spec.iter()
            .map(|&selectable| {
                if selectable {
                    SelectRow::new("row", "desc", "")
                } else {
                    SelectRow::separator()
                }
            })
            .collect()
    }

    #[test]
    fn rendered_height_counts_sublabels_as_their_own_line() {
        // This is the redraw's MoveUp distance: undercount and the menu smears.
        let plain = rows(&[true, true, true]);
        assert_eq!(rendered_height(&plain), 3);

        let mut with_sub = rows(&[true, true, true]);
        with_sub[0] = SelectRow::new("a", "", "").with_sublabel("Jira \u{b7} Confluence");
        with_sub[2] = SelectRow::new("c", "", "").with_sublabel("Confluence");
        assert_eq!(rendered_height(&with_sub), 5);

        assert_eq!(rendered_height(&[]), 0);
    }

    #[test]
    fn navigation_steps_over_separators() {
        // rows: 0 pick, 1 sep, 2 sep, 3 pick
        let r = rows(&[true, false, false, true]);
        assert_eq!(next_selectable(&r, 0, 1), 3, "down skips both separators");
        assert_eq!(next_selectable(&r, 3, -1), 0, "up skips them too");
    }

    #[test]
    fn navigation_clamps_at_both_ends_without_wrapping() {
        let r = rows(&[true, true]);
        assert_eq!(next_selectable(&r, 0, -1), 0, "up at the top stays put");
        assert_eq!(next_selectable(&r, 1, 1), 1, "down at the bottom stays put");
    }

    #[test]
    fn navigation_terminates_when_the_tail_is_all_separators() {
        // Walking off the end past a run of separators must return, not spin.
        let r = rows(&[true, false, false]);
        assert_eq!(next_selectable(&r, 0, 1), 0);
    }

    #[test]
    fn navigation_terminates_when_nothing_is_selectable() {
        let r = rows(&[false, false]);
        assert_eq!(next_selectable(&r, 0, 1), 0);
        assert_eq!(next_selectable(&r, 1, -1), 1);
    }

    #[test]
    fn a_default_landing_on_a_separator_moves_to_a_real_row() {
        let r = rows(&[true, false, true]);
        assert_eq!(normalize_default(&r, 1), 2, "forward first");
        assert_eq!(normalize_default(&r, 0), 0, "already selectable: unchanged");
    }

    #[test]
    fn a_stale_default_past_the_end_falls_back_to_a_real_row() {
        // The row set is rebuilt between menu passes and can shrink.
        let r = rows(&[true, true]);
        assert_eq!(normalize_default(&r, 99), 1);

        // Trailing separator: nothing forward, so it walks back.
        let r = rows(&[true, false]);
        assert_eq!(normalize_default(&r, 1), 0);
    }

    #[test]
    fn the_label_column_is_measured_in_characters_not_bytes() {
        // A multi-byte label must not widen the column past what it displays.
        let r = vec![
            SelectRow::new("Grafana", "", ""),
            SelectRow::new("Atlassián", "", ""),
        ];
        assert_eq!(label_width(&r), 9, "9 chars, though 10 bytes");
    }

    #[test]
    fn separators_do_not_widen_the_label_column() {
        let r = vec![SelectRow::new("Jira", "", ""), SelectRow::separator()];
        assert_eq!(label_width(&r), 4);
    }

    #[test]
    fn backing_out_of_a_prompt_is_a_typed_error_not_a_message_match() {
        // The menu tells "went back" from "went wrong" by type; the rendered
        // text stays what users have always seen.
        let e: anyhow::Error = super::Cancelled.into();
        assert!(super::is_cancelled(&e));
        assert_eq!(e.to_string(), "Cancelled");
        assert!(!super::is_cancelled(&anyhow::anyhow!("keyring locked")));
    }

    use super::*;

    fn row(selectable: bool, parent: Option<usize>) -> MultiRow {
        MultiRow {
            label: String::new(),
            description: String::new(),
            tag: String::new(),
            checked: false,
            selectable,
            parent,
        }
    }

    #[test]
    fn sub_row_toggles_only_while_parent_is_checked() {
        let rows = [row(true, None), row(true, Some(0))];
        assert!(!row_enabled(&rows, &[false, false], 1));
        assert!(row_enabled(&rows, &[true, false], 1));
        // The parent itself never depends on anyone.
        assert!(row_enabled(&rows, &[false, false], 0));
        // Unselectable rows stay off-limits even with a checked parent.
        let rows = [row(true, None), row(false, Some(0))];
        assert!(!row_enabled(&rows, &[true, false], 1));
    }

    #[test]
    fn checked_sub_row_of_unchecked_parent_is_inert() {
        let rows = [row(true, None), row(true, Some(0)), row(true, None)];
        assert_eq!(effective_indices(&rows, &[false, true, true]), vec![2]);
        assert_eq!(effective_indices(&rows, &[true, true, false]), vec![0, 1]);
        assert_eq!(effective_indices(&rows, &[true, false, true]), vec![0, 2]);
    }

    #[test]
    fn unchecking_a_parent_clears_its_sub_rows() {
        let rows = [
            row(true, None),
            row(true, Some(0)),
            row(true, None),
            row(true, Some(2)),
        ];
        let mut checked = vec![true, true, true, true];
        toggle_row(&rows, &mut checked, 0);
        // Row 0 and its sub-row cleared; the other pair untouched.
        assert_eq!(checked, vec![false, false, true, true]);
        // Re-checking the parent does not resurrect the sub-row.
        toggle_row(&rows, &mut checked, 0);
        assert_eq!(checked, vec![true, false, true, true]);
    }

    #[test]
    fn toggling_a_sub_row_touches_only_itself() {
        let rows = [row(true, None), row(true, Some(0))];
        let mut checked = vec![true, true];
        toggle_row(&rows, &mut checked, 1);
        assert_eq!(checked, vec![true, false]);
        toggle_row(&rows, &mut checked, 1);
        assert_eq!(checked, vec![true, true]);
    }
}
