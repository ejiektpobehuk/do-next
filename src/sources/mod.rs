pub mod cache;
pub mod fetcher;

use std::collections::HashMap;

use tokio::sync::mpsc::UnboundedSender;

use crate::config::types::{CacheConfig, DetailLoad, SourceKind, TeamConfig};
use crate::confluence::ConfluenceClient;
use crate::events::AppEvent;
use crate::jira::JiraClient;
use fetcher::{spawn_board_fetch, spawn_confluence_fetch, spawn_fetch};

/// Per-backend HTTP clients, keyed by base URL.
pub struct Clients {
    pub jira: HashMap<String, JiraClient>,
    pub confluence: HashMap<String, ConfluenceClient>,
}

/// Spawn one background fetch task per configured source in a team.
///
/// `default_detail_load` is the global board detail-load mode; a board source
/// may override it via its own `detail_load`. `cache` controls whether each
/// fetch persists its result for stale-while-revalidate.
pub fn spawn_fetches(
    jira: &JiraClient,
    confluence: Option<&ConfluenceClient>,
    team_config: &TeamConfig,
    default_detail_load: DetailLoad,
    cache: &CacheConfig,
    tx: &UnboundedSender<AppEvent>,
) {
    for source_cfg in &team_config.sources {
        match source_cfg.kind {
            SourceKind::Jira => {
                if source_cfg.jql.is_empty() && source_cfg.subsources.is_empty() {
                    // No JQL configured for this source; skip silently
                    let _ = tx.send(AppEvent::SourceLoaded(source_cfg.id.clone(), vec![]));
                    continue;
                }
                spawn_fetch(jira.clone(), source_cfg.clone(), cache.clone(), tx.clone());
            }
            SourceKind::Confluence => {
                if let Some(client) = confluence {
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
            SourceKind::Board => {
                let detail_load = source_cfg
                    .board
                    .as_ref()
                    .and_then(|b| b.detail_load)
                    .unwrap_or(default_detail_load);
                spawn_board_fetch(
                    jira.clone(),
                    source_cfg.clone(),
                    detail_load,
                    cache.clone(),
                    tx.clone(),
                );
            }
        }
    }
}
