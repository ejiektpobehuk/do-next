pub mod credentials;
pub mod hidden;
pub mod types;
pub mod updates;

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use types::{Config, JiraConfig, ResolvedTeam, TeamConfig, TeamJiraOverride, TeamRef};

/// Result of loading user config + all team configs.
pub struct LoadedConfig {
    pub config: Config,
    pub teams: Vec<ResolvedTeam>,
    /// Non-fatal errors from team configs that failed to load.
    pub load_errors: Vec<String>,
}

/// Load user configuration and resolve all team configs.
pub fn load() -> Result<LoadedConfig> {
    let user_path = user_config_path()?;

    let config: Config = if user_path.exists() {
        load_file(&user_path)?
    } else {
        Config::default()
    };

    let mut teams = Vec::new();
    let mut load_errors = Vec::new();
    for team_ref in &config.teams {
        match load_team_config(team_ref) {
            Ok(team_config) => {
                let jira = resolve_team_jira(&config.jira, &team_config);
                let open_slack_in_app = team_config
                    .open_slack_in_app
                    .or(config.open_slack_in_app)
                    .unwrap_or(true);
                let slack_team_id = team_config
                    .slack_team_id
                    .clone()
                    .or_else(|| config.slack_team_id.clone());
                teams.push(ResolvedTeam {
                    id: team_ref.id.clone(),
                    path: team_ref.path.clone(),
                    config: team_config,
                    jira,
                    open_slack_in_app,
                    slack_team_id,
                });
            }
            Err(e) => {
                load_errors.push(format!("team '{}': {e:#}", team_ref.id));
            }
        }
    }

    Ok(LoadedConfig {
        config,
        teams,
        load_errors,
    })
}

pub fn user_config_path() -> Result<PathBuf> {
    Ok(dirs::config_dir()
        .context("Cannot determine config directory")?
        .join("do-next")
        .join("config.json5"))
}

fn load_file<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read config file: {}", path.display()))?;
    json5::from_str(&content)
        .with_context(|| format!("Failed to parse config file: {}", path.display()))
}

/// Load a single team config from disk.
fn load_team_config(team_ref: &TeamRef) -> Result<TeamConfig> {
    let dir = expand_tilde(&team_ref.path);
    let file_name = team_ref.file.as_deref().unwrap_or("do-next.json5");
    let path = dir.join(file_name);
    load_file(&path).with_context(|| format!("Failed to load team '{}' config", team_ref.id))
}

/// Merge team Jira override on top of user default.
fn resolve_team_jira(default: &JiraConfig, team: &TeamConfig) -> JiraConfig {
    let Some(ref overlay) = team.jira else {
        return default.clone();
    };
    let mut jira = default.clone();
    apply_team_jira_override(&mut jira, overlay);
    jira
}

/// Apply a partial team Jira override onto a full `JiraConfig`.
/// Only `Some` fields override the base.
pub fn apply_team_jira_override(base: &mut JiraConfig, overlay: &TeamJiraOverride) {
    if let Some(ref v) = overlay.base_url {
        base.base_url.clone_from(v);
    }
    if let Some(ref v) = overlay.default_project {
        base.default_project.clone_from(v);
    }
    if overlay.email.is_some() {
        base.email.clone_from(&overlay.email);
    }
    if overlay.credential_command.is_some() {
        base.credential_command
            .clone_from(&overlay.credential_command);
    }
    if overlay.credential_store.is_some() {
        base.credential_store.clone_from(&overlay.credential_store);
    }
    if overlay.credential_key.is_some() {
        base.credential_key.clone_from(&overlay.credential_key);
    }
    if overlay.auth_method.is_some() {
        base.auth_method.clone_from(&overlay.auth_method);
    }
    if overlay.oauth_client_id.is_some() {
        base.oauth_client_id.clone_from(&overlay.oauth_client_id);
    }
    if overlay.oauth_client_secret.is_some() {
        base.oauth_client_secret
            .clone_from(&overlay.oauth_client_secret);
    }
}

/// Expand `~` prefix to the user's home directory.
pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(path)
}
