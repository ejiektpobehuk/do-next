use std::collections::{HashMap, HashSet};

use tokio::sync::mpsc::UnboundedSender;

use crate::config::types::{
    CacheConfig, DetailLoad, QueryLane, SourceConfig, SprintSelector, SwimlaneConfig,
};
use crate::confluence::ConfluenceClient;
use crate::events::{ActionResult, AppEvent};
use crate::items::WorkItem;
use crate::jira::JiraClient;
use crate::jira::types::{BoardSwimlanes, BoardType, Issue};

/// Spawn a background task that fetches issues for one source and sends
/// an `AppEvent::SourceLoaded` or `AppEvent::SourceError` when done.
///
/// If the source has subsources, one Jira search is run per subsource using
/// combined JQL: `(parent jql) AND (subsource jql_filter)`.
/// Issues are deduplicated within the source; first-matching subsource wins.
pub fn spawn_fetch(
    client: JiraClient,
    source_cfg: SourceConfig,
    cache: CacheConfig,
    tx: UnboundedSender<AppEvent>,
) {
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
        crate::sources::cache::write(&cache, &source_id, &items, None, None);
        let _ = tx.send(AppEvent::SourceLoaded(source_id, items));
    });
}

/// Spawn a background task that fetches a Jira Agile board source: the
/// column configuration first (`BoardConfigLoaded`), then the issue set per
/// sprint selector (`SourceLoaded`), then — for query-swimlane strategies —
/// the lane assignment (`BoardLanesLoaded`). Lane failures degrade the board
/// to laneless; they never fail the source.
pub fn spawn_board_fetch(
    client: JiraClient,
    source_cfg: SourceConfig,
    detail_load: DetailLoad,
    cache: CacheConfig,
    tx: UnboundedSender<AppEvent>,
) {
    let source_id = source_cfg.id.clone();
    tokio::spawn(async move {
        // Validation guarantees Some; unwrap_or_default keeps the task panic-free.
        let filters = source_cfg.board.clone().unwrap_or_default();

        // Without columns the kanban view can't place a single card, so a
        // failed configuration fetch fails the whole source (matching how a
        // failed JQL fetch behaves).
        log::debug!(
            "Board '{}': fetching configuration for board_id={}",
            source_id,
            filters.board_id
        );
        let config = match client.get_board_configuration(filters.board_id).await {
            Ok(config) => config,
            Err(e) => {
                log::debug!("Board '{source_id}': configuration fetch failed: {e:#}");
                let _ = tx.send(AppEvent::SourceError(source_id, e));
                return;
            }
        };
        let board_type = config.board_type;
        log::debug!(
            "Board '{}': configuration ok (type={:?}, {} columns); fetching issues (sprint={:?})",
            source_id,
            board_type,
            config.column_config.columns.len(),
            filters.sprint
        );
        let _ = tx.send(AppEvent::BoardConfigLoaded(source_id.clone(), config.clone()));

        // In lazy mode fetch only the fields the board renders and mark the
        // issues partial (full detail is loaded when a card is opened); in
        // full mode fetch everything up front.
        let fields = match detail_load {
            DetailLoad::Full => "*all".to_string(),
            DetailLoad::Lazy => board_fields_for(filters.swimlanes.as_ref()),
        };
        let partial = detail_load == DetailLoad::Lazy;

        let issues = match fetch_issues_for_selector(
            &client,
            filters.board_id,
            filters.sprint,
            board_type,
            &fields,
        )
        .await
        {
            Ok(issues) => issues,
            Err(e) => {
                log::debug!("Board '{source_id}': issue fetch failed: {e:#}");
                let _ = tx.send(AppEvent::SourceError(source_id, e));
                return;
            }
        };

        let keys: Vec<String> = issues.iter().map(|i| i.key.clone()).collect();
        let items: Vec<WorkItem> = issues
            .into_iter()
            .map(|mut issue| {
                issue.partial = partial;
                let mut item = WorkItem::Jira(issue);
                item.set_source(source_id.clone(), 0);
                item
            })
            .collect();
        log::debug!(
            "Board source '{}' fetch complete: {} items",
            source_id,
            items.len()
        );
        // Cache items + column config for an instant board paint on next open.
        // Lanes are omitted (they render laneless from cache and fill in on the
        // background revalidation, matching a cold load's lane-loading window).
        crate::sources::cache::write(&cache, &source_id, &items, Some(&config), None);
        let _ = tx.send(AppEvent::SourceLoaded(source_id.clone(), items));

        // Lane resolution. Field lanes need no fetch (grouped in the TUI);
        // auto/query lanes resolve membership via the public search API.
        let (lanes, everything_else, else_name) = match &filters.swimlanes {
            Some(SwimlaneConfig::Auto) => {
                match client.get_greenhopper_swimlanes(filters.board_id).await {
                    Ok(gh_lanes) => {
                        let else_name = gh_lanes
                            .iter()
                            .find(|l| l.is_default)
                            .map_or_else(|| "Everything Else".to_string(), |l| l.name.clone());
                        let lanes: Vec<QueryLane> = gh_lanes
                            .into_iter()
                            .filter(|l| !l.is_default && !l.query.is_empty())
                            .map(|l| QueryLane {
                                name: l.name,
                                jql: l.query,
                            })
                            .collect();
                        (lanes, true, else_name)
                    }
                    Err(e) => {
                        let _ = tx.send(AppEvent::BoardLanesLoaded(source_id, Err(e)));
                        return;
                    }
                }
            }
            Some(SwimlaneConfig::Queries {
                lanes,
                everything_else,
            }) => (
                lanes.clone(),
                *everything_else,
                "Everything Else".to_string(),
            ),
            Some(SwimlaneConfig::Field { .. }) | None => return,
        };

        let result = resolve_query_lanes(&client, &keys, &lanes, everything_else, &else_name).await;
        let _ = tx.send(AppEvent::BoardLanesLoaded(source_id, result));
    });
}

/// The board only renders a handful of fields; fetching just these (instead
/// of `*all`) shrinks the payload, Jira's serialization, and our parse time.
const BOARD_FIELDS: &str = "summary,status,priority,assignee,issuetype,project";

/// The board-display field list for a lazy fetch, plus the swimlane grouping
/// field when lanes group by an arbitrary field not already in the list.
fn board_fields_for(swimlanes: Option<&SwimlaneConfig>) -> String {
    match swimlanes {
        Some(SwimlaneConfig::Field { field })
            if !["priority", "assignee"].contains(&field.as_str()) =>
        {
            format!("{BOARD_FIELDS},{field}")
        }
        _ => BOARD_FIELDS.to_string(),
    }
}

/// The issue set a board source shows, per its sprint selector.
async fn fetch_issues_for_selector(
    client: &JiraClient,
    board_id: u64,
    sprint: SprintSelector,
    board_type: BoardType,
    fields: &str,
) -> anyhow::Result<Vec<Issue>> {
    // Kanban boards have no sprints; the selector is ignored.
    let selector = if board_type == BoardType::Kanban {
        SprintSelector::All
    } else {
        sprint
    };
    match selector {
        SprintSelector::All => client.fetch_board_issues(board_id, fields).await,
        SprintSelector::Id(sprint_id) => {
            client.fetch_board_sprint_issues(board_id, sprint_id, fields).await
        }
        SprintSelector::Active => {
            match client.get_board_sprints(board_id, "active").await? {
                // HTTP 400: the board doesn't support sprints at all
                // (sprint-less team-managed board) — show all its issues.
                None => {
                    log::debug!("Board {board_id}: no sprint support (400); fetching all issues");
                    client.fetch_board_issues(board_id, fields).await
                }
                // Sprints supported but none active: an empty board, NOT
                // all-issues — that would silently include the backlog.
                Some(sprints) if sprints.is_empty() => {
                    log::debug!("Board {board_id}: sprints supported but none active; empty board");
                    Ok(Vec::new())
                }
                // Parallel active sprints are possible; fetch them
                // concurrently, then concatenate in sprint order, deduped by
                // key like the subsource loop above (`try_join_all` preserves
                // sprint order).
                Some(sprints) => {
                    log::debug!("Board {board_id}: {} active sprint(s)", sprints.len());
                    let per_sprint = futures::future::try_join_all(sprints.iter().map(|sprint| {
                        client.fetch_board_sprint_issues(board_id, sprint.id, fields)
                    }))
                    .await?;
                    let mut all: Vec<Issue> = Vec::new();
                    let mut seen: HashSet<String> = HashSet::new();
                    for issues in per_sprint {
                        for issue in issues {
                            if seen.insert(issue.key.clone()) {
                                all.push(issue);
                            }
                        }
                    }
                    Ok(all)
                }
            }
        }
    }
}

/// Keys are checked against lane JQL in chunks this size; Jira comfortably
/// handles `issueKey in (...)` lists of this length.
const LANE_CHUNK: usize = 100;

/// Evaluate query-lane membership through the public search API: each lane's
/// JQL is intersected with the board's keys via chunked `issueKey in (...)`
/// searches, then folded first-match-wins (Jira query-swimlane semantics).
async fn resolve_query_lanes(
    client: &JiraClient,
    keys: &[String],
    lanes: &[QueryLane],
    everything_else: bool,
    else_name: &str,
) -> anyhow::Result<BoardSwimlanes> {
    // Every (lane, key-chunk) membership query is independent, so run them all
    // concurrently. `try_join_all` preserves input order, which we rely on to
    // fold results back into per-lane match lists in lane order.
    let per_lane_matches: Vec<Vec<String>> = futures::future::try_join_all(lanes.iter().map(
        |lane| async move {
            let chunk_matches = futures::future::try_join_all(keys.chunks(LANE_CHUNK).map(
                |chunk| {
                    let jql = membership_jql(chunk, &lane.jql);
                    async move { client.fetch_jql_keys(&jql).await }
                },
            ))
            .await?;
            anyhow::Ok(chunk_matches.into_iter().flatten().collect::<Vec<String>>())
        },
    ))
    .await?;
    let lane_names: Vec<String> = lanes.iter().map(|l| l.name.clone()).collect();
    Ok(build_lane_assignment(
        keys,
        lane_names,
        &per_lane_matches,
        everything_else.then(|| else_name.to_string()),
    ))
}

/// `issueKey in (K-1, K-2, ...) AND (<lane jql>)`
fn membership_jql(keys: &[String], lane_jql: &str) -> String {
    format!("issueKey in ({}) AND ({lane_jql})", keys.join(", "))
}

/// Pure fold of per-lane match lists into a lane assignment. The first lane
/// whose query matched an issue wins; leftovers go to a trailing
/// everything-else lane when one is given (and only if any exist).
fn build_lane_assignment(
    all_keys: &[String],
    mut lane_names: Vec<String>,
    per_lane_matches: &[Vec<String>],
    everything_else: Option<String>,
) -> BoardSwimlanes {
    let mut assignment: HashMap<String, usize> = HashMap::new();
    for (idx, matches) in per_lane_matches.iter().enumerate() {
        for key in matches {
            assignment.entry(key.clone()).or_insert(idx);
        }
    }
    if let Some(else_name) = everything_else
        && all_keys.iter().any(|k| !assignment.contains_key(k))
    {
        let else_idx = lane_names.len();
        lane_names.push(else_name);
        for key in all_keys {
            assignment.entry(key.clone()).or_insert(else_idx);
        }
    }
    BoardSwimlanes {
        lane_names,
        assignment,
    }
}

/// Spawn a background task that fetches Confluence inline tasks for one
/// source and sends `AppEvent::SourceLoaded` / `SourceError` when done.
pub fn spawn_confluence_fetch(
    client: ConfluenceClient,
    source_cfg: SourceConfig,
    cache: CacheConfig,
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
                crate::sources::cache::write(&cache, &source_id, &items, None, None);
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

/// Max concurrent `get_issue` calls a preload fans out. Keeps the burst from
/// hammering Jira when a large board is preloaded.
const PRELOAD_CONCURRENCY: usize = 8;

/// Spawn one background task that fetches full detail for many issues with
/// bounded concurrency, emitting an `IssueRefreshed` (or `IssueRefreshError`)
/// per issue — the same events a single-issue refresh uses, so the receiver
/// swaps each partial issue for its full version as results arrive.
pub fn spawn_preload_details(
    client: JiraClient,
    requests: Vec<crate::tui::app::RefreshIssueRequest>,
    tx: UnboundedSender<AppEvent>,
) {
    use futures::stream::StreamExt;
    log::debug!("Preloading detail for {} partial issue(s)", requests.len());
    tokio::spawn(async move {
        futures::stream::iter(requests)
            .for_each_concurrent(PRELOAD_CONCURRENCY, |req| {
                let client = client.clone();
                let tx = tx.clone();
                async move {
                    match client.get_issue(&req.key).await {
                        Ok(mut issue) => {
                            issue.source_id = req.source_id;
                            issue.subsource_idx = req.subsource_idx;
                            let _ = tx
                                .send(AppEvent::IssueRefreshed(Box::new(WorkItem::Jira(issue))));
                        }
                        Err(error) => {
                            let _ = tx.send(AppEvent::IssueRefreshError {
                                issue_key: req.key,
                                error,
                            });
                        }
                    }
                }
            })
            .await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn membership_jql_intersects_keys_with_lane_query() {
        let jql = membership_jql(&keys(&["A-1", "A-2"]), "priority = Highest");
        assert_eq!(jql, "issueKey in (A-1, A-2) AND (priority = Highest)");
    }

    #[test]
    fn board_fields_are_the_base_set_without_a_field_swimlane() {
        assert_eq!(board_fields_for(None), BOARD_FIELDS);
        // Auto/query lanes need no extra field.
        assert_eq!(board_fields_for(Some(&SwimlaneConfig::Auto)), BOARD_FIELDS);
    }

    #[test]
    fn field_swimlane_appends_only_fields_not_already_fetched() {
        // A custom grouping field is appended.
        assert_eq!(
            board_fields_for(Some(&SwimlaneConfig::Field {
                field: "customfield_10020".into(),
            })),
            format!("{BOARD_FIELDS},customfield_10020")
        );
        // priority/assignee are already in the base set — not duplicated.
        assert_eq!(
            board_fields_for(Some(&SwimlaneConfig::Field {
                field: "priority".into(),
            })),
            BOARD_FIELDS
        );
    }

    #[test]
    fn lane_assignment_is_first_match_wins() {
        let all = keys(&["A-1", "A-2", "A-3"]);
        // A-1 matches both lanes; the first lane must win.
        let result = build_lane_assignment(
            &all,
            vec!["Expedite".into(), "Bugs".into()],
            &[keys(&["A-1"]), keys(&["A-1", "A-2"])],
            None,
        );
        assert_eq!(result.lane_names, ["Expedite", "Bugs"]);
        assert_eq!(result.assignment["A-1"], 0);
        assert_eq!(result.assignment["A-2"], 1);
        assert!(!result.assignment.contains_key("A-3"));
    }

    #[test]
    fn everything_else_lane_appended_only_when_leftovers_exist() {
        let all = keys(&["A-1", "A-2"]);
        let with_leftovers = build_lane_assignment(
            &all,
            vec!["Expedite".into()],
            &[keys(&["A-1"])],
            Some("Everything Else".into()),
        );
        assert_eq!(with_leftovers.lane_names, ["Expedite", "Everything Else"]);
        assert_eq!(with_leftovers.assignment["A-2"], 1);

        let fully_assigned = build_lane_assignment(
            &all,
            vec!["Expedite".into()],
            &[keys(&["A-1", "A-2"])],
            Some("Everything Else".into()),
        );
        assert_eq!(fully_assigned.lane_names, ["Expedite"]);
    }
}
