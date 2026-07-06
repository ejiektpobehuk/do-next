use std::collections::HashSet;

use tokio::sync::mpsc::UnboundedSender;

use crate::config::types::SourceConfig;
use crate::confluence::ConfluenceClient;
use crate::events::{ActionResult, AppEvent};
use crate::items::WorkItem;
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
        let items = if source_cfg.subsources.is_empty() {
            match client.fetch_jql(&source_cfg.jql).await {
                Ok(issues) => issues
                    .into_iter()
                    .map(|issue| {
                        let mut item = WorkItem::Jira(issue);
                        item.set_source(source_id.clone(), 0);
                        item
                    })
                    .collect(),
                Err(e) => {
                    let _ = tx.send(AppEvent::SourceError(source_id, e));
                    return;
                }
            }
        } else {
            let mut all_items: Vec<WorkItem> = Vec::new();
            let mut seen: HashSet<String> = HashSet::new();

            for (subsource_idx, subsource) in source_cfg.subsources.iter().enumerate() {
                let combined_jql = format!("({}) AND ({})", source_cfg.jql, subsource.jql_filter);
                match client.fetch_jql(&combined_jql).await {
                    Ok(issues) => {
                        for issue in issues {
                            if seen.insert(issue.key.clone()) {
                                let mut item = WorkItem::Jira(issue);
                                item.set_source(source_id.clone(), subsource_idx);
                                all_items.push(item);
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
            all_items
        };

        log::debug!(
            "Source '{}' fetch complete: {} items",
            source_id,
            items.len()
        );
        let _ = tx.send(AppEvent::SourceLoaded(source_id, items));
    });
}

/// Spawn a background task that fetches Confluence inline tasks for one
/// source and sends `AppEvent::SourceLoaded` / `SourceError` when done.
pub fn spawn_confluence_fetch(
    client: ConfluenceClient,
    source_cfg: SourceConfig,
    tx: UnboundedSender<AppEvent>,
) {
    let source_id = source_cfg.id.clone();
    tokio::spawn(async move {
        let filters = source_cfg.confluence.clone().unwrap_or_default();
        match client.fetch_tasks(&filters).await {
            Ok(tasks) => {
                let items: Vec<WorkItem> = tasks
                    .into_iter()
                    .map(|task| {
                        let mut item = WorkItem::Confluence(task);
                        item.set_source(source_id.clone(), 0);
                        item
                    })
                    .collect();
                log::debug!(
                    "Source '{}' fetch complete: {} tasks",
                    source_id,
                    items.len()
                );
                let _ = tx.send(AppEvent::SourceLoaded(source_id, items));
            }
            Err(e) => {
                let _ = tx.send(AppEvent::SourceError(source_id, e));
            }
        }
    });
}

/// Spawn a background task that marks a Confluence inline task complete.
pub fn spawn_complete_task(
    client: ConfluenceClient,
    item_key: String,
    task_id: String,
    tx: UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        match client.set_task_status(&task_id, true).await {
            Ok(()) => {
                let _ = tx.send(AppEvent::ActionDone(ActionResult::TaskCompleted {
                    item_key,
                }));
            }
            Err(e) => {
                let _ = tx.send(AppEvent::ActionDone(ActionResult::Error(e)));
            }
        }
    });
}

/// Spawn a background task running a one-off JQL query for the search popup.
/// The result carries the `token` of the search request that triggered it so
/// the receiver can drop stale responses.
pub fn spawn_jira_search(
    client: JiraClient,
    jql: String,
    token: u64,
    tx: UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        let result = client.fetch_jql(&jql).await;
        let _ = tx.send(AppEvent::SearchJiraResult { token, result });
    });
}

/// Spawn a background task that fetches the distinct status names available
/// across the given project keys, in parallel. Statuses are deduped by name,
/// preserving first-seen order.
pub fn spawn_team_statuses_fetch(
    client: JiraClient,
    project_keys: Vec<String>,
    team_idx: usize,
    tx: UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        let result = fetch_team_statuses(client, project_keys).await;
        let _ = tx.send(AppEvent::TeamStatusesLoaded { team_idx, result });
    });
}

async fn fetch_team_statuses(
    client: JiraClient,
    project_keys: Vec<String>,
) -> anyhow::Result<Vec<String>> {
    let mut handles = Vec::with_capacity(project_keys.len());
    for key in project_keys {
        let c = client.clone();
        handles.push(tokio::spawn(
            async move { c.get_project_statuses(&key).await },
        ));
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for h in handles {
        match h.await {
            Ok(Ok(statuses)) => {
                for s in statuses {
                    if seen.insert(s.clone()) {
                        out.push(s);
                    }
                }
            }
            Ok(Err(e)) => {
                log::warn!("project statuses fetch failed: {e}");
            }
            Err(e) => {
                log::warn!("project statuses task join failed: {e}");
            }
        }
    }
    Ok(out)
}

/// Spawn a background task that fetches up to one page of visible Jira projects.
pub fn spawn_projects_fetch(client: JiraClient, team_idx: usize, tx: UnboundedSender<AppEvent>) {
    tokio::spawn(async move {
        let result = client.search_projects().await;
        let _ = tx.send(AppEvent::AllProjectsLoaded { team_idx, result });
    });
}

/// Spawn a background task that fetches every status configured on the Jira
/// instance. Used to populate the status picker's "Other" section.
pub fn spawn_all_statuses_fetch(
    client: JiraClient,
    team_idx: usize,
    tx: UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        let result = client.get_all_statuses().await;
        let _ = tx.send(AppEvent::AllStatusesLoaded { team_idx, result });
    });
}

/// Spawn a background task that refreshes a single issue and sends an
/// `AppEvent::IssueRefreshed` (or `IssueRefreshError`) when done.
///
/// `source_id` and `subsource_idx` are preserved on the refreshed issue so it
/// keeps its grouping in the list.
pub fn spawn_refresh_issue(
    client: JiraClient,
    key: String,
    source_id: Option<String>,
    subsource_idx: usize,
    tx: UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        match client.get_issue(&key).await {
            Ok(mut issue) => {
                issue.source_id = source_id;
                issue.subsource_idx = subsource_idx;
                let _ = tx.send(AppEvent::IssueRefreshed(Box::new(WorkItem::Jira(issue))));
            }
            Err(error) => {
                let _ = tx.send(AppEvent::IssueRefreshError {
                    issue_key: key,
                    error,
                });
            }
        }
    });
}
