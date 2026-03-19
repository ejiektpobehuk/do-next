use std::collections::HashSet;

use tokio::sync::mpsc::UnboundedSender;

use crate::config::types::SourceConfig;
use crate::events::AppEvent;
use crate::jira::JiraClient;

/// Spawn a background task that fetches issues for one source and sends
/// an `AppEvent::SourceLoaded` or `AppEvent::SourceError` when done.
///
/// If the source has subsources, one Jira search is run per subsource using
/// combined JQL: `(parent jql) AND (subsource jql_filter)`.
/// Issues are deduplicated within the source; first-matching subsource wins.
pub fn spawn_fetch(client: JiraClient, source_cfg: SourceConfig, tx: UnboundedSender<AppEvent>) {
    let source_id = source_cfg.id.clone();
    tokio::spawn(async move {
        let issues = if source_cfg.subsources.is_empty() {
            match client.fetch_jql(&source_cfg.jql).await {
                Ok(mut issues) => {
                    for issue in &mut issues {
                        issue.source_id = Some(source_id.clone());
                    }
                    issues
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::SourceError(source_id, e));
                    return;
                }
            }
        } else {
            let mut all_issues = Vec::new();
            let mut seen: HashSet<String> = HashSet::new();

            for (subsource_idx, subsource) in source_cfg.subsources.iter().enumerate() {
                let combined_jql = format!("({}) AND ({})", source_cfg.jql, subsource.jql_filter);
                match client.fetch_jql(&combined_jql).await {
                    Ok(mut issues) => {
                        for issue in &mut issues {
                            if seen.insert(issue.key.clone()) {
                                issue.source_id = Some(source_id.clone());
                                issue.subsource_idx = subsource_idx;
                                all_issues.push(issue.clone());
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(AppEvent::SubsourceError(
                            source_id.clone(),
                            subsource_idx,
                            e,
                        ));
                        // Continue — other subsources may still succeed.
                    }
                }
            }
            all_issues
        };

        let _ = tx.send(AppEvent::SourceLoaded(source_id, issues));
    });
}
