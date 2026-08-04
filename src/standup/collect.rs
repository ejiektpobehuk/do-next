//! Per-backend activity collection.
//!
//! Each backend is independent and returns its own [`Outcome`]: the caller turns
//! a failure into a subsource error for that backend alone, so a missing GitLab
//! token or an unpermitted Confluence search never costs you the rest of your
//! standup.
//!
//! The shape throughout is *cheap day-granular discovery, then precise local
//! verification*. Discovery over-fetches on purpose (see [`super::jql`]);
//! nothing is reported that we have not attributed ourselves from a timestamp.

use anyhow::Result;
use futures::stream::StreamExt;

use crate::config::types::StandupFilters;
use crate::confluence::ConfluenceClient;
use crate::gitlab::GitlabClient;
use crate::items::WorkItem;
use crate::jira::JiraClient;
use crate::standup::derive;
use crate::standup::types::{Backend, EntryKind, ItemRef, StandupEntry};
use crate::standup::window::Window;

/// Most candidates carried into verification. A standup covering a normal
/// weekend is nowhere near this; the cap stops a mis-scoped `extra_jql` from
/// turning one screen into thousands of requests.
const MAX_CANDIDATES: usize = 200;

/// Most per-page version lookups for Confluence page activity.
const MAX_VERSION_LOOKUPS: usize = 50;

/// Page cap for the reduced-accuracy Confluence walk. Same value as the GitLab
/// client's paging guard.
const MAX_PAGE_WALK_PAGES: u32 = 20;

/// Concurrent per-item lookups. Matches `gitlab::client::ENRICH_CONCURRENCY` so
/// no one backend can burst harder than the others.
const CONCURRENCY: usize = 8;

/// How many comments to pull when an issue's inline block was truncated.
const COMMENT_PAGE_SIZE: u32 = 100;

/// An issue whose inline payload was truncated, and what still needs fetching.
struct Gap {
    item: ItemRef,
    needs_comments: bool,
    /// `(start_at, max_results)` for the oldest-first changelog endpoint, aimed
    /// at the newest changegroups.
    changelog_tail: Option<(u32, u32)>,
}

/// What one backend contributed.
#[derive(Debug, Default)]
pub struct Outcome {
    pub entries: Vec<StandupEntry>,
    /// Real payloads, so the screen's Enter can open the existing detail view.
    pub items: Vec<WorkItem>,
    /// True when this backend fell back to a less precise path.
    pub degraded: bool,
}

// ── Jira ─────────────────────────────────────────────────────────────────────

/// Jira activity: transitions, field edits, comments, worklogs and issues filed.
///
/// One search does discovery *and* carries the content, because `expand=changelog`
/// works on `/search/jql` and returns changegroups newest-first. Extra requests
/// happen only where a payload proved insufficient — a truncated comment block,
/// a truncated changelog, or an issue that turned up in worklog discovery.
pub async fn collect_jira(
    jira: &JiraClient,
    filters: &StandupFilters,
    window: &Window,
    me: &str,
    base_url: &str,
    updated_by_usable: bool,
) -> Result<Outcome> {
    let extra = filters.jira.extra_jql.as_deref();
    let jql = super::jql::discovery(me, window.days(), extra, updated_by_usable);
    log::debug!("standup jira discovery: {jql}");

    // One search does discovery *and* carries the content: `expand=changelog` is
    // supported here and returns changegroups newest-first.
    let fields =
        "summary,status,issuetype,project,priority,assignee,creator,created,comment,parent";
    let issues = cap_candidates(jira.fetch_jql_with(&jql, fields, Some("changelog")).await?);

    let mut out = Outcome::default();
    for issue in &issues {
        out.entries
            .extend(derive::entries_from_issue(issue, me, window, base_url));
    }
    out.entries
        .extend(fill_jira_gaps(jira, &issues, me, window, base_url).await);

    if filters.jira.worklogs {
        match collect_jira_worklogs(jira, filters, window, me, base_url).await {
            Ok(entries) => out.entries.extend(entries),
            // Worklogs are an extra, not the point; losing them beats losing the
            // whole Jira section.
            Err(e) => log::warn!("standup: worklog collection failed: {e:#}"),
        }
    }

    // Keep only the issues that actually contributed, so the screen's Enter never
    // lands on an item with nothing to show.
    let touched: std::collections::HashSet<&str> =
        out.entries.iter().map(|e| e.item.key.as_str()).collect();
    out.items = issues
        .into_iter()
        .filter(|i| touched.contains(i.key.as_str()))
        .map(|mut issue| {
            // The changelog is not serialized, but dropping it keeps the
            // in-memory copy small too.
            issue.changelog = None;
            WorkItem::Jira(issue)
        })
        .collect();

    Ok(out)
}

/// Enforce [`MAX_CANDIDATES`], keeping the most recently updated issues.
fn cap_candidates(mut issues: Vec<crate::jira::types::Issue>) -> Vec<crate::jira::types::Issue> {
    if issues.len() <= MAX_CANDIDATES {
        return issues;
    }
    log::warn!(
        "standup: {} Jira candidates exceeded the {MAX_CANDIDATES} cap; \
         dropping the oldest — narrow `standup.jira.extra_jql`",
        issues.len()
    );
    issues.sort_by(|a, b| {
        let updated = |i: &crate::jira::types::Issue| {
            i.fields
                .extra
                .get("updated")
                .and_then(serde_json::Value::as_str)
                .and_then(crate::datetime::parse_dt)
        };
        updated(b).cmp(&updated(a))
    });
    issues.truncate(MAX_CANDIDATES);
    issues
}

/// Second-pass lookups for issues whose inline payload was truncated.
///
/// Failures degrade one issue's detail, never the backend — the GitLab
/// enrichment precedent.
async fn fill_jira_gaps(
    jira: &JiraClient,
    issues: &[crate::jira::types::Issue],
    me: &str,
    window: &Window,
    base_url: &str,
) -> Vec<StandupEntry> {
    // Owned values first: borrowing `&Issue` into the async closure below trips
    // the higher-ranked-lifetime inference in `buffer_unordered`.
    let gaps: Vec<Gap> = issues
        .iter()
        .filter_map(|issue| {
            let needs_comments = issue
                .fields
                .comment
                .as_ref()
                .is_some_and(crate::jira::types::CommentList::is_truncated);
            // `expand=changelog` is newest-first, so truncation only bites an
            // issue with more changegroups than a single page holds.
            let changelog_tail = issue
                .changelog
                .as_ref()
                .filter(|c| c.is_truncated())
                .map(|c| (c.total.saturating_sub(c.max_results), c.max_results));
            if !needs_comments && changelog_tail.is_none() {
                return None;
            }
            Some(Gap {
                item: ItemRef {
                    key: issue.key.clone(),
                    title: issue.fields.summary.clone(),
                    url: format!("{}/browse/{}", base_url.trim_end_matches('/'), issue.key),
                    backend: Backend::Jira,
                },
                needs_comments,
                changelog_tail,
            })
        })
        .collect();

    if gaps.is_empty() {
        return Vec::new();
    }
    log::debug!("standup: {} issue(s) need a second lookup", gaps.len());

    let filled: Vec<Vec<StandupEntry>> = futures::stream::iter(gaps)
        .map(|gap| async move {
            let mut entries = Vec::new();
            if gap.needs_comments {
                match jira
                    .get_recent_comments(&gap.item.key, COMMENT_PAGE_SIZE)
                    .await
                {
                    Ok(comments) => {
                        entries.extend(derive::comment_entries(&comments, me, window, &gap.item));
                    }
                    Err(e) => {
                        log::warn!("standup: comment page for {} failed: {e:#}", gap.item.key);
                    }
                }
            }
            if let Some((start_at, max_results)) = gap.changelog_tail {
                match jira
                    .get_changelog_tail(&gap.item.key, start_at, max_results)
                    .await
                {
                    Ok(histories) => {
                        entries.extend(derive::entries_from_histories(
                            &histories, me, window, &gap.item,
                        ));
                    }
                    Err(e) => {
                        log::warn!("standup: changelog tail for {} failed: {e:#}", gap.item.key);
                    }
                }
            }
            entries
        })
        .buffer_unordered(CONCURRENCY)
        .collect()
        .await;
    filled.into_iter().flatten().collect()
}

/// Worklog entries, via a discovery query of their own.
async fn collect_jira_worklogs(
    jira: &JiraClient,
    filters: &StandupFilters,
    window: &Window,
    me: &str,
    base_url: &str,
) -> Result<Vec<StandupEntry>> {
    let jql = super::jql::worklog_discovery(window.days(), filters.jira.extra_jql.as_deref());
    log::debug!("standup worklog discovery: {jql}");
    let keys = jira
        .fetch_jql_page_keys(&jql, u32::try_from(MAX_CANDIDATES).unwrap_or(100))
        .await?;

    let start_ms = window.start.timestamp_millis();
    let end_ms = window.end.timestamp_millis();

    let per_issue: Vec<Vec<StandupEntry>> = futures::stream::iter(keys)
        .map(|key| async move {
            let item = ItemRef {
                key: key.clone(),
                // Titles come from the main discovery pass when the issue also
                // appeared there; a worklog-only issue shows its key.
                title: key.clone(),
                url: format!("{}/browse/{key}", base_url.trim_end_matches('/')),
                backend: Backend::Jira,
            };
            match jira.get_worklogs_between(&key, start_ms, end_ms).await {
                Ok(worklogs) => derive::worklog_entries(&worklogs, me, window, &item),
                Err(e) => {
                    log::warn!("standup: worklogs for {key} failed: {e:#}");
                    Vec::new()
                }
            }
        })
        .buffer_unordered(CONCURRENCY)
        .collect()
        .await;

    Ok(per_issue.into_iter().flatten().collect())
}

// ── GitLab ───────────────────────────────────────────────────────────────────

/// Merge requests you opened, merged or closed, plus optional event and
/// project-creation extras.
pub async fn collect_gitlab(
    gitlab: &GitlabClient,
    filters: &StandupFilters,
    window: &Window,
) -> Result<Outcome> {
    let mut out = Outcome::default();

    let mrs = gitlab
        .fetch_my_merge_requests_since(window.start, &[], &[])
        .await?;
    for mr in &mrs {
        out.entries.extend(derive::entries_from_mr(mr, window));
    }
    let touched: std::collections::HashSet<String> =
        out.entries.iter().map(|e| e.item.key.clone()).collect();
    out.items = mrs
        .into_iter()
        .filter(|mr| touched.contains(&mr.key))
        .map(WorkItem::Gitlab)
        .collect();

    if filters.gitlab.projects_created {
        match gitlab.fetch_owned_projects().await {
            Ok(projects) => {
                for project in projects {
                    let Some(at) = project.created_at.filter(|at| window.contains_instant(*at))
                    else {
                        continue;
                    };
                    let path = project
                        .path_with_namespace
                        .clone()
                        .unwrap_or_else(|| project.name.clone());
                    out.entries.push(StandupEntry {
                        at,
                        item: ItemRef {
                            key: format!("GLPROJ:{}", project.id),
                            title: project.name.clone(),
                            url: project.web_url.clone(),
                            backend: Backend::Gitlab,
                        },
                        kind: EntryKind::ProjectCreated,
                        detail: path,
                    });
                }
            }
            Err(e) => log::warn!("standup: owned-project lookup failed: {e:#}"),
        }
    }

    if filters.gitlab.events {
        // Bounds are whole dates and exclusive, so widen by a day each way and
        // let `created_at` decide.
        let after = (window.start - chrono::Duration::days(1)).date_naive();
        let before = (window.end + chrono::Duration::days(1)).date_naive();
        match gitlab.fetch_events_between(after, before).await {
            Ok(events) => out.entries.extend(entries_from_events(&events, window)),
            // Most likely a token without `read_user`. Say so once and move on.
            Err(e) => log::warn!(
                "standup: GitLab events feed unavailable (needs a token with \
                 `read_user` or `api`): {e:#}"
            ),
        }
    }

    Ok(out)
}

/// Merge-request events the merge-requests endpoint cannot show — chiefly one
/// you closed but did not author, since there is no `closed_by` filter.
fn entries_from_events(
    events: &[crate::gitlab::types::ApiEvent],
    window: &Window,
) -> Vec<StandupEntry> {
    events
        .iter()
        .filter(|e| window.contains_instant(e.created_at))
        .filter(|e| e.target_type.as_deref() == Some("MergeRequest"))
        .filter_map(|e| {
            let action = e.action_name.as_deref()?;
            let kind = match action {
                "closed" => EntryKind::MrClosed,
                "merged" | "accepted" => EntryKind::MrMerged,
                "opened" | "created" => EntryKind::MrOpened,
                // Comments and pushes have no MR-level entry kind; the
                // merge-requests pass already covers "this MR moved".
                _ => return None,
            };
            let iid = e.target_iid?;
            let project = e.project_id?;
            let title = e.target_title.clone().unwrap_or_else(|| format!("!{iid}"));
            Some(StandupEntry {
                at: e.created_at,
                item: ItemRef {
                    key: format!("MR:{project}!{iid}"),
                    title,
                    url: String::new(),
                    backend: Backend::Gitlab,
                },
                kind,
                detail: format!("!{iid}"),
            })
        })
        .collect()
}

// ── Confluence ───────────────────────────────────────────────────────────────

/// Inline tasks you ticked off. Filtered entirely server-side.
pub async fn collect_confluence_tasks(
    confluence: &ConfluenceClient,
    window: &Window,
    site_url: &str,
) -> Result<Outcome> {
    let tasks = confluence
        .fetch_completed_tasks(window.start, window.end)
        .await?;
    let mut out = Outcome::default();
    for task in &tasks {
        if let Some(entry) = derive::entry_from_task(task, window, site_url) {
            out.entries.push(entry);
        }
    }
    let touched: std::collections::HashSet<String> =
        out.entries.iter().map(|e| e.item.key.clone()).collect();
    out.items = tasks
        .into_iter()
        .filter(|t| touched.contains(&t.key))
        .map(WorkItem::Confluence)
        .collect();
    Ok(out)
}

/// Pages you created or edited.
///
/// Primary path: CQL search for pages you contributed to (day-granular and
/// timezone-untrusted), then per-page version history for precise attribution.
/// If the search is not permitted, falls back to walking recently-modified pages
/// in the configured spaces and marks the outcome degraded — that path cannot see
/// a page you edited which someone else edited afterwards.
pub async fn collect_confluence_pages(
    confluence: &ConfluenceClient,
    filters: &StandupFilters,
    window: &Window,
    site_url: &str,
) -> Result<Outcome> {
    let me = confluence.account_id().await?;
    // Widen a day: `lastmodified` ignores the time component and resolves its day
    // boundary in the Confluence account's timezone, not ours.
    let from = (window.start - chrono::Duration::days(1)).date_naive();
    let spaces = &filters.confluence.spaces;

    let found = confluence
        .search_contributed_pages(
            from,
            spaces,
            u32::try_from(MAX_VERSION_LOOKUPS).unwrap_or(50),
        )
        .await;

    match found {
        Ok(pages) => {
            let mut out = Outcome::default();
            let per_page: Vec<Vec<StandupEntry>> = futures::stream::iter(pages)
                .map(|(id, title, url)| {
                    let me = me.clone();
                    async move {
                        let item = ItemRef {
                            key: format!("CONFPAGE:{id}"),
                            title,
                            url: url.unwrap_or_else(|| site_url.to_owned()),
                            backend: Backend::ConfluencePage,
                        };
                        match confluence.fetch_page_versions(&id).await {
                            Ok(versions) => {
                                derive::entries_from_page_versions(&versions, &me, window, &item)
                            }
                            Err(e) => {
                                log::warn!("standup: page versions for {id} failed: {e:#}");
                                Vec::new()
                            }
                        }
                    }
                })
                .buffer_unordered(CONCURRENCY)
                .collect()
                .await;
            out.entries = per_page.into_iter().flatten().collect();
            Ok(out)
        }
        Err(search_err) => {
            log::warn!(
                "standup: Confluence CQL search unavailable, falling back to a \
                 space-scoped page walk: {search_err:#}"
            );
            let pages = confluence
                .walk_recent_pages(spaces, window.start, MAX_PAGE_WALK_PAGES)
                .await
                .map_err(|walk_err| {
                    // Surface both: the walk's error alone would hide *why* the
                    // fallback was needed.
                    anyhow::anyhow!(
                        "Confluence page activity unavailable. \
                         Search failed: {search_err:#}. Fallback failed: {walk_err:#}"
                    )
                })?;
            let entries = pages
                .iter()
                .flat_map(|p| derive::entries_from_page_summary(p, &me, window, site_url))
                .collect();
            Ok(Outcome {
                entries,
                items: Vec::new(),
                degraded: true,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn window() -> Window {
        Window {
            start: Utc.with_ymd_and_hms(2026, 8, 3, 0, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 8, 4, 0, 0, 0).unwrap(),
        }
    }

    fn event(action: &str, target: Option<&str>, day: u32) -> crate::gitlab::types::ApiEvent {
        crate::gitlab::types::ApiEvent {
            action_name: Some(action.to_owned()),
            target_type: target.map(str::to_owned),
            target_iid: Some(7),
            target_title: Some("Fix flaky test".to_owned()),
            project_id: Some(42),
            created_at: Utc.with_ymd_and_hms(2026, 8, day, 12, 0, 0).unwrap(),
        }
    }

    #[test]
    fn closed_merge_request_events_become_entries() {
        let got = entries_from_events(&[event("closed", Some("MergeRequest"), 3)], &window());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind, EntryKind::MrClosed);
        assert_eq!(got[0].item.key, "MR:42!7");
        assert_eq!(got[0].item.title, "Fix flaky test");
    }

    #[test]
    fn non_merge_request_events_are_ignored() {
        let got = entries_from_events(&[event("pushed to", Some("Note"), 3)], &window());
        assert!(got.is_empty());
        let got = entries_from_events(&[event("closed", None, 3)], &window());
        assert!(got.is_empty());
    }

    #[test]
    fn events_outside_the_window_are_dropped() {
        // The feed is queried a day wide on each side, so this filter is what
        // actually enforces the window.
        let got = entries_from_events(&[event("closed", Some("MergeRequest"), 1)], &window());
        assert!(got.is_empty());
    }

    #[test]
    fn unmapped_event_actions_are_skipped_rather_than_guessed() {
        for action in ["commented on", "pushed to", "joined", "updated"] {
            let got = entries_from_events(&[event(action, Some("MergeRequest"), 3)], &window());
            assert!(got.is_empty(), "{action} should not map to an entry kind");
        }
    }
}
