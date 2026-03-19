pub mod credentials;
pub mod hidden;
pub mod types;

use anyhow::{Context, Result};
use std::path::PathBuf;

use types::Config;

/// Load and merge configuration.
/// Reads user config, then merges project override (.do-next/config.json5) on top.
pub fn load() -> Result<(Config, bool)> {
    let user_path = user_config_path()?;
    let project_path = PathBuf::from(".do-next/config.json5");

    let mut config = if user_path.exists() {
        load_file(&user_path)?
    } else {
        Config::default()
    };

    let project_override_exists = project_path.exists();
    if project_override_exists {
        let overlay: Config = load_file(&project_path)?;
        merge_config(&mut config, overlay);
    }

    Ok((config, project_override_exists))
}

fn user_config_path() -> Result<PathBuf> {
    Ok(dirs::config_dir()
        .context("Cannot determine config directory")?
        .join("do-next")
        .join("config.json5"))
}

fn load_file(path: &PathBuf) -> Result<Config> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read config file: {}", path.display()))?;
    json5::from_str(&content)
        .with_context(|| format!("Failed to parse config file: {}", path.display()))
}

/// Merge `overlay` into `base`. Non-empty overlay fields win.
fn merge_config(base: &mut Config, overlay: Config) {
    if !overlay.jira.base_url.is_empty() {
        base.jira.base_url = overlay.jira.base_url;
    }
    if !overlay.jira.default_project.is_empty() {
        base.jira.default_project = overlay.jira.default_project;
    }
    if overlay.jira.credential_command.is_some() {
        base.jira.credential_command = overlay.jira.credential_command;
    }
    if overlay.jira.credential_store.is_some() {
        base.jira.credential_store = overlay.jira.credential_store;
    }
    if overlay.jira.credential_key.is_some() {
        base.jira.credential_key = overlay.jira.credential_key;
    }
    // If project override defines sources, replace entirely; otherwise keep base sources.
    if !overlay.sources.is_empty() {
        base.sources = overlay.sources;
    }
    if overlay.list.default_indication.is_some() {
        base.list.default_indication = overlay.list.default_indication;
    }
}
