use std::collections::HashMap;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::types::{
    CacheConfig, CustomViewConfig, HideForADayConfig, JiraConfig, ListConfig, SourceConfig,
    TeamConfig, TeamRef,
};

/// Old single-file config format (pre-team split).
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
struct OldConfig {
    #[serde(default)]
    jira: JiraConfig,
    #[serde(default)]
    sources: Vec<SourceConfig>,
    #[serde(default)]
    list: ListConfig,
    #[serde(default)]
    hide_for_a_day: HideForADayConfig,
    #[serde(default)]
    views: HashMap<String, CustomViewConfig>,
    #[serde(default)]
    cache: CacheConfig,
}

/// New user config (personal settings + team references).
#[derive(Debug, Serialize)]
struct NewUserConfig {
    jira: JiraConfig,
    cache: CacheConfig,
    teams: Vec<TeamRef>,
}

pub fn run() -> Result<()> {
    let config_path = crate::config::user_config_path()?;
    if !config_path.exists() {
        return Err(anyhow::anyhow!(
            "No config file found at {}",
            config_path.display()
        ));
    }

    // Check if already migrated (has `teams` key)
    let raw = std::fs::read_to_string(&config_path)
        .with_context(|| format!("Failed to read {}", config_path.display()))?;
    if raw.contains("\"teams\"") || raw.contains("teams:") || raw.contains("teams :") {
        println!("Config already appears to use the team format. Nothing to migrate.");
        return Ok(());
    }

    // Parse as old format
    let old: OldConfig = json5::from_str(&raw)
        .with_context(|| format!("Failed to parse old config at {}", config_path.display()))?;

    let has_team_fields = !old.sources.is_empty()
        || !old.views.is_empty()
        || old.hide_for_a_day.duration_hours.is_some();

    if !has_team_fields {
        println!("Config has no team-specific fields (sources, views, hide_for_a_day).");
        println!("Creating a minimal personal team config.");
    }

    // Create personal team directory
    let config_dir = config_path
        .parent()
        .context("Cannot determine config directory")?;
    let team_dir = config_dir.join("teams").join("personal");
    std::fs::create_dir_all(&team_dir)
        .with_context(|| format!("Failed to create {}", team_dir.display()))?;

    // Write team config
    let team_config = TeamConfig {
        jira: None,
        sources: old.sources,
        list: old.list,
        hide_for_a_day: old.hide_for_a_day,
        views: old.views,
    };
    let team_config_path = team_dir.join("do-next.json5");
    let team_json = json5::to_string(&team_config).context("Failed to serialize team config")?;
    std::fs::write(&team_config_path, &team_json)
        .with_context(|| format!("Failed to write {}", team_config_path.display()))?;
    println!("Team config written to {}", team_config_path.display());

    // Write new user config
    let new_config = NewUserConfig {
        jira: old.jira,
        cache: old.cache,
        teams: vec![TeamRef {
            id: "personal".into(),
            path: team_dir.to_string_lossy().into_owned(),
            file: None,
        }],
    };

    // Back up old config
    let backup_path = config_path.with_extension("json5.bak");
    std::fs::copy(&config_path, &backup_path)
        .with_context(|| format!("Failed to create backup at {}", backup_path.display()))?;
    println!("Old config backed up to {}", backup_path.display());

    let user_json = json5::to_string(&new_config).context("Failed to serialize user config")?;
    std::fs::write(&config_path, &user_json)
        .with_context(|| format!("Failed to write {}", config_path.display()))?;
    println!("User config updated at {}", config_path.display());

    println!("\nMigration complete!");
    Ok(())
}
