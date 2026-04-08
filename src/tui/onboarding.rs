use anyhow::Result;
use crossterm::cursor::MoveUp;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::style::{Color, Print, ResetColor, SetForegroundColor};
use crossterm::terminal::{Clear, ClearType, disable_raw_mode, enable_raw_mode};
use std::io;
use std::io::Write;

use crate::config::LoadedConfig;
use crate::config::types::{Config, JiraConfig, ResolvedTeam, TeamConfig, TeamRef};
use crate::jira::auth::OAuthStore;

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

// ── Step 2: storage ─────────────────────────────────────────────────────────

#[derive(PartialEq, Clone)]
enum StorageChoice {
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

const TOKEN_STORAGE_DESCRIPTIONS: [&str; TOKEN_STORAGE_COUNT] = [
    KEYRING_DESCRIPTION,
    "~/.config/do-next/credentials.json5 (chmod 600)",
    "fetch via shell command (pass, bitwarden CLI, …)",
    "set DO_NEXT_JIRA_API_TOKEN env manually",
];

const KEYRING_DESCRIPTION: &str = if cfg!(target_os = "macos") {
    "macOS Keychain (recommended)"
} else if cfg!(target_os = "windows") {
    "Windows Credential Manager (recommended)"
} else {
    "Linux Secret Service (recommended)"
};

// ── Status probing ──────────────────────────────────────────────────────────

struct CredentialStatus {
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

/// Run the interactive first-run wizard.
/// Returns a fully configured `LoadedConfig` (credentials stored per user's choice).
#[allow(clippy::too_many_lines)]
pub fn run_onboarding() -> Result<LoadedConfig> {
    println!("Welcome to do-next! Let's set up your configuration.\n");

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
        AuthMethod::PersonalToken => prompt_token_storage(None, None)?,
    };

    // Step 3: email (only for personal token).
    let email = if auth_method == AuthMethod::PersonalToken {
        Some(prompt("Jira account email: ", None)?)
    } else {
        None
    };

    let mut jira_config = JiraConfig {
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
            crate::jira::oauth::run_oauth_flow(&client_id, &client_secret, store)?;
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
    };

    let config = Config {
        jira: jira_config.clone(),
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
        config: team_config,
        jira: jira_config,
    };

    Ok(LoadedConfig {
        config,
        teams: vec![resolved],
        load_errors: Vec::new(),
    })
}

// ── Auth reset ──────────────────────────────────────────────────────────────

/// Reconfigure authentication for an existing install without overwriting other config.
#[allow(clippy::too_many_lines)]
pub fn run_auth_reset(config: &mut Config) -> Result<()> {
    if config.jira.base_url.is_empty() {
        return Err(anyhow::anyhow!(
            "No configuration found. Run do-next first to complete initial setup."
        ));
    }

    println!(
        "Reconfiguring Jira authentication for {}",
        config.jira.base_url
    );
    println!();

    let current_auth = detect_auth_method(&config.jira);
    let auth_method = prompt_auth_method(Some(&current_auth))?;

    println!();
    let status = probe_credential_status(&config.jira);
    let current_storage = detect_storage_method(&config.jira);
    let storage = match auth_method {
        AuthMethod::OAuth => prompt_oauth_storage(Some(&current_storage))?,
        AuthMethod::PersonalToken => prompt_token_storage(Some(&current_storage), Some(&status))?,
    };

    // Clear existing auth fields; each branch sets only what it needs.
    config.jira.credential_command = None;
    config.jira.credential_store = None;
    config.jira.auth_method = None;

    let config_dir = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine config directory"))?
        .join("do-next");

    match auth_method {
        AuthMethod::OAuth => {
            let (client_id, client_secret) = resolve_oauth_client_credentials(&config.jira)?;
            let store = match storage {
                StorageChoice::Keyring => OAuthStore::Keyring,
                _ => OAuthStore::File,
            };
            crate::jira::oauth::run_oauth_flow(&client_id, &client_secret, store)?;
            config.jira.auth_method = Some("oauth".into());
            config.jira.oauth_client_id = Some(client_id);
            config.jira.oauth_client_secret = Some(client_secret);
            config.jira.email = None;
            if matches!(storage, StorageChoice::Keyring) {
                config.jira.credential_store = Some("keyring".into());
            }
        }
        AuthMethod::PersonalToken => {
            let current_email = config.jira.email.as_deref().unwrap_or("");
            let email_prompt = if current_email.is_empty() {
                "Jira account email: ".to_string()
            } else {
                format!("Jira account email [{current_email}]: ")
            };
            let email = prompt(&email_prompt, Some(current_email))?;
            config.jira.email = Some(email);
            println!();

            apply_token_storage(&storage, &mut config.jira, &config_dir)?;
        }
    }

    // Write updated config back.
    let config_path = config_dir.join("config.json5");
    if config_path.exists() {
        println!("Note: config file will be rewritten in minimal format (comments removed).");
    }
    std::fs::create_dir_all(&config_dir)?;
    let json5_content = json5::to_string(&config)?;
    std::fs::write(&config_path, json5_content)?;
    println!("Config updated at {}", config_path.display());

    Ok(())
}

// ── Team setup (no teams configured) ────────────────────────────────────────

const TEAM_SETUP_COUNT: usize = 2;

const TEAM_SETUP_LABELS: [&str; TEAM_SETUP_COUNT] =
    ["Create personal space", "Use existing config "];

const TEAM_SETUP_DESCRIPTIONS: [&str; TEAM_SETUP_COUNT] = [
    "create a local team config for your personal sources",
    "provide a path to an existing team config (e.g. a cloned git repo)",
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

    let (team_ref, team_config, team_jira) = if choice == 0 {
        // Create personal space
        let team_dir = config_dir.join("teams").join("personal");
        std::fs::create_dir_all(&team_dir)?;

        let default_project = &config.jira.default_project;
        let team_config_path = team_dir.join("do-next.json5");
        let tc = if team_config_path.exists() {
            // Reuse existing team config (e.g. from a prior migration)
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
        };
        (tr, tc, config.jira.clone())
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

        let jira = if let Some(ref overlay) = tc.jira {
            let mut j = config.jira.clone();
            crate::config::apply_team_jira_override(&mut j, overlay);
            j
        } else {
            config.jira.clone()
        };

        let tr = TeamRef {
            id: id.clone(),
            path: path.clone(),
            file: None,
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
        config: team_config,
        jira: team_jira,
    };

    Ok(LoadedConfig {
        config: config.clone(),
        teams: vec![resolved],
        load_errors: Vec::new(),
    })
}

// ── Token storage application ───────────────────────────────────────────────

fn apply_token_storage(
    storage: &StorageChoice,
    jira_config: &mut JiraConfig,
    config_dir: &std::path::Path,
) -> Result<()> {
    match storage {
        StorageChoice::Keyring => {
            let key = jira_config
                .credential_key
                .as_deref()
                .unwrap_or(&jira_config.base_url)
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

            jira_config.credential_store = Some("keyring".into());
        }

        StorageChoice::File => {
            print_api_token_instructions();
            let token = prompt_masked("API token: ")?;

            let creds_path = config_dir.join("credentials.json5");
            let creds_content = format!("{{ jira: {{ api_token: \"{token}\" }} }}\n");
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
            println!("Enter the shell command whose stdout is your Jira API token.");
            println!("Examples:  pass show jira/do-next");
            println!("           op read 'op://Private/Jira/credential'");
            println!();
            let cmd = prompt("Credential command: ", None)?;
            jira_config.credential_command = Some(cmd);
        }

        StorageChoice::Env => {
            println!();
            println!("Set the following environment variables before running do-next:");
            println!("  DO_NEXT_JIRA_EMAIL=<your-email>");
            println!("  DO_NEXT_JIRA_API_TOKEN=<your-api-token>");
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
fn resolve_oauth_client_credentials(jira: &JiraConfig) -> Result<(String, String)> {
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

fn detect_auth_method(jira: &JiraConfig) -> AuthMethod {
    if jira.auth_method.as_deref() == Some("oauth") {
        AuthMethod::OAuth
    } else {
        AuthMethod::PersonalToken
    }
}

fn detect_storage_method(jira: &JiraConfig) -> StorageChoice {
    if jira.credential_command.is_some() {
        StorageChoice::Command
    } else if jira.credential_store.as_deref() == Some("keyring") {
        StorageChoice::Keyring
    } else {
        StorageChoice::File
    }
}

fn probe_credential_status(jira: &JiraConfig) -> CredentialStatus {
    let env_set = std::env::var("DO_NEXT_JIRA_API_TOKEN").is_ok();

    let file_exists =
        dirs::config_dir().is_some_and(|d| d.join("do-next").join("credentials.json5").exists());

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
fn run_selection(
    title: &str,
    labels: &[&str],
    descriptions: &[&str],
    tags: &[String],
    default: usize,
    current_idx: Option<usize>,
) -> Result<usize> {
    let count = labels.len();

    println!("{title}");
    println!();
    render_options(labels, descriptions, tags, default, current_idx, false)?;
    io::stdout().flush()?;

    enable_raw_mode()?;

    let mut selected = default;
    #[allow(clippy::cast_possible_truncation)]
    let lines = count as u16;

    loop {
        match crossterm::event::read() {
            Ok(Event::Key(KeyEvent {
                code, modifiers, ..
            })) => {
                let nav = match code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        selected = selected.saturating_sub(1);
                        true
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        selected = (selected + 1).min(count - 1);
                        true
                    }
                    _ => false,
                };

                if nav {
                    crossterm::execute!(
                        io::stdout(),
                        MoveUp(lines),
                        Clear(ClearType::FromCursorDown)
                    )?;
                    render_options(labels, descriptions, tags, selected, current_idx, false)?;
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
                        render_options(labels, descriptions, tags, selected, current_idx, true)?;
                        io::stdout().flush()?;
                        disable_raw_mode()?;
                        println!();
                        return Ok(selected);
                    }
                    KeyCode::Esc | KeyCode::Char('q') => {
                        disable_raw_mode()?;
                        println!();
                        return Err(anyhow::anyhow!("Cancelled"));
                    }
                    KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                        disable_raw_mode()?;
                        println!();
                        return Err(anyhow::anyhow!("Cancelled"));
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

fn render_options(
    labels: &[&str],
    descriptions: &[&str],
    tags: &[String],
    selected: usize,
    current_idx: Option<usize>,
    confirmed: bool,
) -> Result<()> {
    for i in 0..labels.len() {
        let marker = if current_idx == Some(i) {
            "  \u{2190} current"
        } else {
            ""
        };
        let tag = tags.get(i).map_or("", String::as_str);
        if i == selected && confirmed {
            crossterm::execute!(
                io::stdout(),
                SetForegroundColor(Color::Green),
                Print("  \u{2713} "),
                ResetColor,
                Print(format!(
                    "{}   {}{}{}\r\n",
                    labels[i], descriptions[i], tag, marker
                )),
            )?;
        } else if i == selected {
            print!(
                "  > {}   {}{}{}\r\n",
                labels[i], descriptions[i], tag, marker
            );
        } else {
            print!(
                "    {}   {}{}{}\r\n",
                labels[i], descriptions[i], tag, marker
            );
        }
    }
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

fn prompt_oauth_storage(current: Option<&StorageChoice>) -> Result<StorageChoice> {
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

fn prompt_token_storage(
    current: Option<&StorageChoice>,
    status: Option<&CredentialStatus>,
) -> Result<StorageChoice> {
    let current_idx = current.map(|c| match c {
        StorageChoice::Keyring => 0,
        StorageChoice::File => 1,
        StorageChoice::Command => 2,
        StorageChoice::Env => 3,
    });
    let default = current_idx.unwrap_or(0);

    let tags = build_token_storage_tags(status);

    let idx = run_selection(
        "How would you like to store your API token?",
        &TOKEN_STORAGE_LABELS,
        &TOKEN_STORAGE_DESCRIPTIONS,
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
                    return Err(anyhow::anyhow!("Cancelled"));
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
    jira_config: &crate::config::types::JiraConfig,
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
        "    // credential_store: \"keyring\",\n    // credential_command: \"pass show jira/do-next\",\n    // Env: DO_NEXT_JIRA_API_TOKEN=<your-api-token>\n".to_string()
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
    //   1. Env:              DO_NEXT_JIRA_API_TOKEN=<api-token>
    //   2. External command: credential_command: "..."
    //   3. System keyring:   credential_store: "keyring"
    //   4. Credentials file: ~/.config/do-next/credentials.json5
    //   Or use OAuth:        auth_method: "oauth"
    // Email override:        DO_NEXT_JIRA_EMAIL=<email>
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
    println!("Jira API Token");
    println!("  An API token lets do-next read and act on Jira issues on your behalf.");
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
fn prompt_yes_no(message: &str, default: bool) -> Result<bool> {
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

fn prompt(message: &str, default: Option<&str>) -> Result<String> {
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

fn prompt_masked(message: &str) -> Result<String> {
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
                return Err(anyhow::anyhow!("Cancelled"));
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
