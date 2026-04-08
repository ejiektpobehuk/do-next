pub mod fetcher;

use tokio::sync::mpsc::UnboundedSender;

use crate::config::types::TeamConfig;
use crate::events::AppEvent;
use crate::jira::JiraClient;
use fetcher::spawn_fetch;

/// Spawn one background fetch task per configured source in a team.
pub fn spawn_fetches(
    client: &JiraClient,
    team_config: &TeamConfig,
    tx: &UnboundedSender<AppEvent>,
) {
    for source_cfg in &team_config.sources {
        if source_cfg.jql.is_empty() && source_cfg.subsources.is_empty() {
            // No JQL configured for this source; skip silently
            let _ = tx.send(AppEvent::SourceLoaded(source_cfg.id.clone(), vec![]));
            continue;
        }
        spawn_fetch(client.clone(), source_cfg.clone(), tx.clone());
    }
}
