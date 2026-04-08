use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HiddenState {
    #[serde(default)]
    pub issues: HashMap<String, DateTime<Utc>>,
}

impl HiddenState {
    pub fn load(path: &PathBuf) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        json5::from_str(&content).context("Failed to parse hidden.json5")
    }

    pub fn save(&self, path: &PathBuf) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory {}", parent.display()))?;
        }
        let content = json5::to_string(self).context("Failed to serialize hidden state")?;
        std::fs::write(path, content)
            .with_context(|| format!("Failed to write {}", path.display()))?;
        Ok(())
    }

    /// Returns true if the issue is currently hidden (hide period has not expired).
    #[allow(dead_code)]
    pub fn is_hidden(&self, issue_key: &str) -> bool {
        self.issues
            .get(issue_key)
            .is_some_and(|until| Utc::now() < *until)
    }

    /// Hide an issue until `now + duration_hours`.
    pub fn hide_for(&mut self, issue_key: &str, duration_hours: u32) {
        let until = Utc::now() + chrono::Duration::hours(i64::from(duration_hours));
        self.issues.insert(issue_key.to_string(), until);
    }

    /// Remove expired entries (cleanup).
    #[allow(dead_code)]
    pub fn prune(&mut self) {
        let now = Utc::now();
        self.issues.retain(|_, until| *until > now);
    }
}

/// Resolve the hidden.json5 path for a given team.
pub fn hidden_path(team_id: &str) -> Result<PathBuf> {
    Ok(dirs::config_dir()
        .context("Cannot determine config directory")?
        .join("do-next")
        .join("hidden")
        .join(format!("{team_id}.json5")))
}
