//! GitLab integration: merge requests waiting on the user (review requested,
//! assigned, authored, or any) listed alongside Jira issues and Confluence
//! tasks in the same prioritised list.
//!
//! Read-only by design — `o` opens a merge request in the browser and `i`
//! hides it for a day; nothing here approves, merges or comments.

mod client;
pub mod types;

use anyhow::{Context, Result};

use crate::config::credentials::resolve_gitlab_token;
use crate::config::types::ResolvedTeam;
pub use client::GitlabClient;
use types::ApiUser;

/// Check a token against the API and return the user it belongs to. Used by
/// the interactive token setup to catch typos before storing.
pub async fn validate_token(base_url: &str, token: String) -> Result<ApiUser> {
    GitlabClient::new(base_url, token)?
        .current_user()
        .await
        .context("token check against the GitLab API failed")
}

/// One token-setup target: the teams behind a single GitLab instance (one
/// token covers every team on the same instance).
pub struct TokenSetupTarget {
    pub base_url: String,
    pub team_ids: Vec<String>,
}

/// Teams with GitLab sources, grouped by instance base URL. Order follows the
/// team list.
pub fn gitlab_api_urls(teams: &[ResolvedTeam]) -> Vec<TokenSetupTarget> {
    let mut groups: Vec<TokenSetupTarget> = Vec::new();
    for team in teams {
        if !team.uses_gitlab() {
            continue;
        }
        match groups
            .iter_mut()
            .find(|g| g.base_url == team.gitlab.base_url)
        {
            Some(group) => group.team_ids.push(team.id.clone()),
            None => groups.push(TokenSetupTarget {
                base_url: team.gitlab.base_url.clone(),
                team_ids: vec![team.id.clone()],
            }),
        }
    }
    groups
}

/// Like [`gitlab_api_urls`], but only teams whose token resolves to "not
/// configured anywhere" — the ones the startup setup offer targets. Hard
/// resolution failures (locked keyring, failing command) are excluded; they
/// surface as source errors instead.
pub fn teams_missing_token(teams: &[ResolvedTeam]) -> Vec<TokenSetupTarget> {
    let missing: Vec<ResolvedTeam> = teams
        .iter()
        .filter(|t| t.uses_gitlab() && matches!(resolve_gitlab_token(&t.gitlab), Ok(None)))
        .cloned()
        .collect();
    gitlab_api_urls(&missing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::{JiraConfig, ResolvedGitlab, SourceConfig, SourceKind, TeamConfig};

    fn team(id: &str, base_url: &str, kinds: &[SourceKind]) -> ResolvedTeam {
        let sources: Vec<SourceConfig> = kinds
            .iter()
            .enumerate()
            .map(|(i, &kind)| SourceConfig {
                id: format!("s{i}"),
                kind,
                ..Default::default()
            })
            .collect();
        ResolvedTeam {
            id: id.into(),
            path: "/tmp".into(),
            config: TeamConfig {
                sources: sources.clone(),
                ..Default::default()
            },
            jira: JiraConfig::default(),
            confluence: JiraConfig::default(),
            open_slack_in_app: true,
            slack_team_id: None,
            grafana: None,
            gitlab: ResolvedGitlab {
                base_url: base_url.into(),
                ..Default::default()
            },
            normal_sources: sources,
            on_duty: false,
        }
    }

    #[test]
    fn setup_targets_group_teams_by_instance() {
        let teams = [
            team("a", "https://gitlab.com", &[SourceKind::Gitlab]),
            team("b", "https://gitlab.example.com", &[SourceKind::Gitlab]),
            team("c", "https://gitlab.com", &[SourceKind::Gitlab]),
        ];
        let targets = gitlab_api_urls(&teams);
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].base_url, "https://gitlab.com");
        assert_eq!(targets[0].team_ids, vec!["a", "c"]);
        assert_eq!(targets[1].team_ids, vec!["b"]);
    }

    #[test]
    fn teams_without_gitlab_sources_are_not_setup_targets() {
        let teams = [
            team("jira-only", "https://gitlab.com", &[SourceKind::Jira]),
            team("empty", "https://gitlab.com", &[]),
        ];
        assert!(gitlab_api_urls(&teams).is_empty());
    }
}
