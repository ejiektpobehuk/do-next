pub mod cache;
pub mod fetcher;

use std::collections::HashMap;

use tokio::sync::mpsc::UnboundedSender;

use crate::config::types::{CacheConfig, DetailLoad, SourceConfig, SourceKind, TeamConfig};
use crate::confluence::ConfluenceClient;
use crate::events::AppEvent;
use crate::gitlab::GitlabClient;
use crate::jira::JiraClient;
use fetcher::{
    spawn_backlog_fetch, spawn_board_fetch, spawn_confluence_fetch, spawn_fetch,
    spawn_gitlab_fetch, spawn_standup_fetch,
};

/// Per-backend HTTP clients, keyed by base URL.
pub struct Clients {
    pub jira: HashMap<String, JiraClient>,
    pub confluence: HashMap<String, ConfluenceClient>,
    pub gitlab: HashMap<String, GitlabClient>,
}

/// The clients one team's sources fetch through, resolved out of [`Clients`].
/// Only `jira` is guaranteed: the other backends are configured per team and
/// a missing client surfaces as a source error, not a panic.
#[derive(Clone, Copy)]
pub struct TeamClients<'a> {
    pub jira: &'a JiraClient,
    pub confluence: Option<&'a ConfluenceClient>,
    pub gitlab: Option<&'a GitlabClient>,
    /// The team's Jira *site* URL, for building `/browse/KEY` links. Distinct
    /// from `JiraClient::base_url()`, which under OAuth points at
    /// `api.atlassian.com` and so is useless to a human.
    pub jira_site_url: &'a str,
}

/// Spawn one background fetch task per configured source in a team.
///
/// `default_detail_load` is the global board detail-load mode; a board source
/// may override it via its own `detail_load`. `cache` controls whether each
/// fetch persists its result for stale-while-revalidate.
pub fn spawn_fetches(
    clients: TeamClients<'_>,
    team_config: &TeamConfig,
    default_detail_load: DetailLoad,
    cache: &CacheConfig,
    tx: &UnboundedSender<AppEvent>,
) {
    for source_cfg in &team_config.sources {
        spawn_source_fetch(clients, source_cfg, default_detail_load, cache, tx);
    }
}

/// Spawn the background fetch task for a single source. Used for partial
/// refetches (e.g. the on-duty toggle fetching only the duty sources).
pub fn spawn_source_fetch(
    clients: TeamClients<'_>,
    source_cfg: &SourceConfig,
    default_detail_load: DetailLoad,
    cache: &CacheConfig,
    tx: &UnboundedSender<AppEvent>,
) {
    match source_cfg.kind {
        SourceKind::Jira => {
            if source_cfg.jql.is_empty() && source_cfg.subsources.is_empty() {
                // No JQL configured for this source; skip silently
                let _ = tx.send(AppEvent::SourceLoaded(source_cfg.id.clone(), vec![]));
                return;
            }
            spawn_fetch(
                clients.jira.clone(),
                source_cfg.clone(),
                cache.clone(),
                tx.clone(),
            );
        }
        SourceKind::Confluence => {
            if let Some(client) = clients.confluence {
                spawn_confluence_fetch(
                    client.clone(),
                    source_cfg.clone(),
                    cache.clone(),
                    tx.clone(),
                );
            } else {
                let _ = tx.send(AppEvent::SourceError(
                    source_cfg.id.clone(),
                    anyhow::anyhow!("Confluence is not configured for this team"),
                ));
            }
        }
        SourceKind::Gitlab => {
            if let Some(client) = clients.gitlab {
                spawn_gitlab_fetch(
                    client.clone(),
                    source_cfg.clone(),
                    cache.clone(),
                    tx.clone(),
                );
            } else {
                let _ = tx.send(AppEvent::SourceError(
                    source_cfg.id.clone(),
                    anyhow::anyhow!(
                        "GitLab is not configured for this team \
                         (run `do-next auth` to sign in or store a token)"
                    ),
                ));
            }
        }
        SourceKind::Board => {
            let detail_load = source_cfg
                .board
                .as_ref()
                .and_then(|b| b.detail_load)
                .unwrap_or(default_detail_load);
            spawn_board_fetch(
                clients.jira.clone(),
                source_cfg.clone(),
                detail_load,
                cache.clone(),
                tx.clone(),
            );
        }
        SourceKind::Backlog => {
            let detail_load = source_cfg
                .board
                .as_ref()
                .and_then(|b| b.detail_load)
                .unwrap_or(default_detail_load);
            spawn_backlog_fetch(
                clients.jira.clone(),
                source_cfg.clone(),
                detail_load,
                cache.clone(),
                tx.clone(),
            );
        }
        // The only kind that reaches every backend at once: it collects your
        // activity wherever it happened, so it takes all three clients and
        // reports each backend's failure independently.
        SourceKind::Standup => {
            spawn_standup_fetch(
                fetcher::StandupFetch {
                    jira: clients.jira.clone(),
                    confluence: clients.confluence.cloned(),
                    gitlab: clients.gitlab.cloned(),
                    jira_site_url: clients.jira_site_url.to_owned(),
                    source_cfg: source_cfg.clone(),
                    cache: cache.clone(),
                    // Default coverage; the screen refetches with a wider floor
                    // once the user steps past it.
                    coverage_floor: None,
                },
                tx.clone(),
            );
        }
    }
}
