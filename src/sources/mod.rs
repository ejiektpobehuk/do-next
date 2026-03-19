pub mod fetcher;

use tokio::sync::mpsc::UnboundedSender;

use crate::config::types::Config;
use crate::events::AppEvent;
use crate::jira::JiraClient;
use fetcher::spawn_fetch;

/// Spawn one background fetch task per configured source.
pub fn spawn_fetches(client: &JiraClient, config: &Config, tx: &UnboundedSender<AppEvent>) {
    for source_cfg in &config.sources {
        if source_cfg.jql.is_empty() && source_cfg.subsources.is_empty() {
            // No JQL configured for this source; skip silently
            let _ = tx.send(AppEvent::SourceLoaded(source_cfg.id.clone(), vec![]));
            continue;
        }
        spawn_fetch(client.clone(), source_cfg.clone(), tx.clone());
    }
}
