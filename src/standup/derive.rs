//! Turning source payloads into timeline entries.
//!
//! Everything here is pure: no network, no clock. The collector does the I/O and
//! hands payloads in, which is what makes the attribution rules — "authored by
//! me", "inside the window", "not a duplicate of the creation entry" — testable
//! against fixtures.

use chrono::{DateTime, Utc};

use crate::confluence::types::{ApiPageSummary, ApiPageVersion, Task};
use crate::datetime::parse_dt;
use crate::gitlab::types::MergeRequest;
use crate::jira::types::{Comment, Issue, UserField, Worklog};
use crate::standup::types::{Backend, EntryKind, ItemRef, StandupEntry};
use crate::standup::window::Window;

/// Longest detail string kept on an entry; longer values are elided.
const SNIPPET_LEN: usize = 90;

/// Does this user reference identify the current user?
///
/// Checks both `accountId` and `name` because `JiraClient::current_user()`
/// returns whichever the deployment supplies — an account id on Cloud, a
/// username on Data Center.
pub fn is_me(user: Option<&UserField>, me: &str) -> bool {
    user.is_some_and(|u| u.account_id.as_deref() == Some(me) || u.name.as_deref() == Some(me))
}

/// Same check against a raw JSON user object, for fields that live untyped in
/// `IssueFields::extra` (`creator`).
fn json_is_me(value: Option<&serde_json::Value>, me: &str) -> bool {
    value.is_some_and(|v| {
        v.get("accountId").and_then(serde_json::Value::as_str) == Some(me)
            || v.get("name").and_then(serde_json::Value::as_str) == Some(me)
    })
}

/// First non-blank line, trimmed and elided to [`SNIPPET_LEN`].
fn snippet(text: &str) -> String {
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if line.chars().count() <= SNIPPET_LEN {
        return line.to_owned();
    }
    let kept: String = line.chars().take(SNIPPET_LEN).collect();
    format!("{}…", kept.trim_end())
}

/// Render an ADF (or plain-string) body to a one-line snippet.
fn body_snippet(body: &serde_json::Value) -> String {
    let text = body.as_str().map_or_else(
        || crate::jira::adf::adf_to_markdown(body),
        std::string::ToString::to_string,
    );
    snippet(&text)
}

fn jira_item_ref(issue: &Issue, base_url: &str) -> ItemRef {
    ItemRef {
        key: issue.key.clone(),
        title: issue.fields.summary.clone(),
        url: format!("{}/browse/{}", base_url.trim_end_matches('/'), issue.key),
        backend: Backend::Jira,
    }
}

/// Every entry derivable from one issue payload: creation, changelog and the
/// comments that came inline.
///
/// The changelog is expected to be the object from `expand=changelog` (newest
/// first); entries not authored by `me`, or outside `window`, are dropped.
pub fn entries_from_issue(
    issue: &Issue,
    me: &str,
    window: &Window,
    base_url: &str,
) -> Vec<StandupEntry> {
    let item = jira_item_ref(issue, base_url);
    let mut out = Vec::new();

    // Creation. Jira writes *no changelog entry* for it — initial field values
    // are not a changegroup — so without this an issue you filed inside the
    // window would be discovered and then silently produce nothing.
    if json_is_me(issue.fields.extra.get("creator"), me)
        && let Some(created) = issue
            .fields
            .extra
            .get("created")
            .and_then(serde_json::Value::as_str)
            .and_then(parse_dt)
    {
        let at = created.with_timezone(&Utc);
        if window.contains_instant(at) {
            out.push(StandupEntry {
                at,
                item: item.clone(),
                kind: EntryKind::Created,
                detail: issue.fields.issuetype.name.clone(),
            });
        }
    }

    if let Some(changelog) = &issue.changelog {
        out.extend(entries_from_histories(
            &changelog.histories,
            me,
            window,
            &item,
        ));
    }

    if let Some(list) = &issue.fields.comment {
        out.extend(comment_entries(&list.comments, me, window, &item));
    }

    out
}

/// Entries from a list of changegroups, whichever endpoint supplied them.
///
/// Split out so the changelog *tail* fetch — used when the inline object was
/// truncated — can reuse the attribution rules without fabricating an `Issue`.
pub fn entries_from_histories(
    histories: &[crate::jira::types::ChangelogEntry],
    me: &str,
    window: &Window,
    item: &ItemRef,
) -> Vec<StandupEntry> {
    let mut out = Vec::new();
    for history in histories {
        if !is_me(history.author.as_ref(), me) {
            continue;
        }
        let Some(at) = parse_dt(&history.created).map(|d| d.with_timezone(&Utc)) else {
            continue;
        };
        if !window.contains_instant(at) {
            continue;
        }
        for change in &history.items {
            out.push(entry_from_change(at, item, change));
        }
    }
    out
}

fn entry_from_change(
    at: DateTime<Utc>,
    item: &ItemRef,
    change: &crate::jira::types::ChangelogItem,
) -> StandupEntry {
    let from = change.from_string.clone().filter(|s| !s.is_empty());
    let to = change.to_string_value.clone().filter(|s| !s.is_empty());

    if change.field.eq_ignore_ascii_case("status") {
        let from_s = from.unwrap_or_else(|| "—".to_owned());
        let to_s = to.unwrap_or_else(|| "—".to_owned());
        return StandupEntry {
            at,
            item: item.clone(),
            kind: EntryKind::Transition {
                from: from_s.clone(),
                to: to_s.clone(),
            },
            detail: format!("{from_s} → {to_s}"),
        };
    }

    // For everything else the new value is the interesting part; long bodies
    // (description, custom text fields) are elided rather than dropped.
    let detail = to.as_ref().map_or_else(
        || format!("{} cleared", change.field),
        |v| format!("{}: {}", change.field, snippet(v)),
    );
    StandupEntry {
        at,
        item: item.clone(),
        kind: EntryKind::FieldChange {
            field: change.field.clone(),
            from,
            to,
        },
        detail,
    }
}

/// Comment entries for `me` inside `window`.
///
/// A comment I wrote *and* later edited yields one entry, not two: authorship is
/// checked first, so the edit branch only catches comments written by someone
/// else, or written by me before the window opened.
pub fn comment_entries(
    comments: &[Comment],
    me: &str,
    window: &Window,
    item: &ItemRef,
) -> Vec<StandupEntry> {
    let mut out = Vec::new();
    for comment in comments {
        let created = parse_dt(&comment.created).map(|d| d.with_timezone(&Utc));
        let updated = parse_dt(&comment.updated).map(|d| d.with_timezone(&Utc));

        let wrote_it =
            is_me(Some(&comment.author), me) && created.is_some_and(|c| window.contains_instant(c));
        let edited_it = is_me(comment.update_author.as_ref(), me)
            && updated.is_some_and(|u| window.contains_instant(u));

        let (at, edited) = if wrote_it {
            (created, false)
        } else if edited_it {
            (updated, true)
        } else {
            continue;
        };
        let Some(at) = at else { continue };

        out.push(StandupEntry {
            at,
            item: item.clone(),
            kind: EntryKind::Comment {
                id: comment.id.clone(),
                edited,
            },
            detail: body_snippet(&comment.body),
        });
    }
    out
}

/// Worklog entries for `me`.
///
/// Placed on the day the work *happened* (`started`), not the day it was typed
/// in: a worklog entered this morning for yesterday's work belongs in
/// yesterday's row.
pub fn worklog_entries(
    worklogs: &[Worklog],
    me: &str,
    window: &Window,
    item: &ItemRef,
) -> Vec<StandupEntry> {
    worklogs
        .iter()
        .filter(|w| is_me(w.author.as_ref(), me))
        .filter_map(|w| {
            let started = parse_dt(&w.started)?.with_timezone(&Utc);
            if !window.contains_instant(started) {
                return None;
            }
            Some(StandupEntry {
                at: started,
                item: item.clone(),
                kind: EntryKind::Worklog {
                    seconds: w.time_spent_seconds,
                    started,
                },
                detail: fmt_duration(w.time_spent_seconds),
            })
        })
        .collect()
}

/// Human duration for a worklog: "1h 30m", "45m", "2h".
pub fn fmt_duration(seconds: i64) -> String {
    let mins = (seconds / 60).abs();
    let (h, m) = (mins / 60, mins % 60);
    match (h, m) {
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
    }
}

// ── GitLab ───────────────────────────────────────────────────────────────────

/// Entries for one merge request.
///
/// `created_at`/`merged_at`/`closed_at` are provably yours (you authored it, and
/// those instants only move when the MR was opened, merged or closed). A bare
/// `updated_at` hit is not — a colleague's comment bumps it too — so it becomes
/// a single low-confidence [`EntryKind::MrTouched`], and only when nothing
/// stronger was found.
pub fn entries_from_mr(mr: &MergeRequest, window: &Window) -> Vec<StandupEntry> {
    let item = ItemRef {
        key: mr.key.clone(),
        title: mr.title.clone(),
        url: mr.web_url.clone(),
        backend: Backend::Gitlab,
    };
    let label = mr.short_ref();
    let mut out = Vec::new();

    for (instant, kind) in [
        (mr.created_at, EntryKind::MrOpened),
        (mr.merged_at, EntryKind::MrMerged),
        (mr.closed_at, EntryKind::MrClosed),
    ] {
        if let Some(at) = instant.filter(|at| window.contains_instant(*at)) {
            out.push(StandupEntry {
                at,
                item: item.clone(),
                kind,
                detail: label.clone(),
            });
        }
    }

    if out.is_empty()
        && let Some(at) = mr.updated_at.filter(|at| window.contains_instant(*at))
    {
        out.push(StandupEntry {
            at,
            item,
            kind: EntryKind::MrTouched,
            detail: label,
        });
    }
    out
}

// ── Confluence ───────────────────────────────────────────────────────────────

/// Entry for a task you ticked off. `None` when the completion instant is
/// missing (tasks completed before Atlassian recorded it) or outside the window.
pub fn entry_from_task(task: &Task, window: &Window, site_url: &str) -> Option<StandupEntry> {
    let at = task.completed_at?;
    if !window.contains_instant(at) {
        return None;
    }
    Some(StandupEntry {
        at,
        item: ItemRef {
            key: task.key.clone(),
            title: task.title.clone(),
            url: task.page_url.clone().unwrap_or_else(|| site_url.to_owned()),
            backend: Backend::ConfluenceTask,
        },
        kind: EntryKind::TaskCompleted,
        detail: task.page_title.clone().unwrap_or_default(),
    })
}

/// Entries from a page's version history.
///
/// Version 1 is the creation. Minor edits are skipped so a standup is not padded
/// out with typo fixes.
pub fn entries_from_page_versions(
    versions: &[ApiPageVersion],
    me: &str,
    window: &Window,
    item: &ItemRef,
) -> Vec<StandupEntry> {
    versions
        .iter()
        .filter(|v| v.author_id.as_deref() == Some(me))
        .filter(|v| !v.minor_edit)
        .filter(|v| window.contains_instant(v.created_at))
        .map(|v| StandupEntry {
            at: v.created_at,
            item: item.clone(),
            kind: if v.number <= 1 {
                EntryKind::PageCreated { version: v.number }
            } else {
                EntryKind::PageUpdated { version: v.number }
            },
            detail: v.message.as_deref().map(snippet).unwrap_or_default(),
        })
        .collect()
}

/// Entries from a page summary, for the reduced-accuracy fallback.
///
/// Only "I created it" and "I made the latest edit" are visible this way — a
/// page you edited that someone else edited afterwards is invisible, which is
/// exactly why the caller marks this backend degraded.
pub fn entries_from_page_summary(
    page: &ApiPageSummary,
    me: &str,
    window: &Window,
    site_url: &str,
) -> Vec<StandupEntry> {
    let url = page.links.webui.as_ref().map_or_else(
        || site_url.to_owned(),
        |w| format!("{}/wiki{w}", site_url.trim_end_matches('/')),
    );
    let item = ItemRef {
        key: format!("CONFPAGE:{}", page.id),
        title: page.title.clone(),
        url,
        backend: Backend::ConfluencePage,
    };
    let mut out = Vec::new();

    if page.author_id.as_deref() == Some(me)
        && let Some(at) = page.created_at.filter(|at| window.contains_instant(*at))
    {
        out.push(StandupEntry {
            at,
            item: item.clone(),
            kind: EntryKind::PageCreated { version: 1 },
            detail: String::new(),
        });
    }

    if let Some(version) = &page.version
        && version.author_id.as_deref() == Some(me)
        && version.number > 1
        && window.contains_instant(version.created_at)
    {
        out.push(StandupEntry {
            at: version.created_at,
            item,
            kind: EntryKind::PageUpdated {
                version: version.number,
            },
            detail: version.message.as_deref().map(snippet).unwrap_or_default(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jira::types::{
        Changelog, ChangelogEntry, ChangelogItem, CommentList, IssueFields, IssueTypeField,
        ProjectField, StatusField,
    };
    use chrono::TimeZone;
    use std::assert_matches;
    use std::collections::HashMap;

    const ME: &str = "557058:me";
    const OTHER: &str = "557058:someone-else";

    fn utc(d: u32, h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, d, h, m, 0)
            .single()
            .expect("valid")
    }

    /// Jira's wire format: colon-less offset.
    fn jira_ts(d: u32, h: u32, m: u32) -> String {
        format!("2026-08-{d:02}T{h:02}:{m:02}:00.000+0000")
    }

    fn window() -> Window {
        Window {
            start: utc(3, 0, 0),
            end: utc(4, 0, 0),
        }
    }

    fn user(id: &str) -> UserField {
        UserField {
            name: None,
            display_name: Some("Someone".into()),
            account_id: Some(id.to_owned()),
        }
    }

    fn issue(extra: HashMap<String, serde_json::Value>) -> Issue {
        Issue {
            id: "1".into(),
            key: "PROJ-1".into(),
            fields: IssueFields {
                summary: "Login broken".into(),
                status: StatusField {
                    id: "1".into(),
                    name: "In Progress".into(),
                },
                priority: None,
                assignee: None,
                reporter: None,
                issuetype: IssueTypeField {
                    id: "1".into(),
                    name: "Bug".into(),
                    subtask: false,
                },
                project: ProjectField {
                    id: "1".into(),
                    key: "PROJ".into(),
                    name: "Project".into(),
                },
                description: None,
                comment: None,
                attachment: None,
                extra,
            },
            source_id: None,
            subsource_idx: 0,
            partial: false,
            changelog: None,
        }
    }

    fn creator_extra(account: &str, created: &str) -> HashMap<String, serde_json::Value> {
        let mut extra = HashMap::new();
        extra.insert(
            "creator".to_owned(),
            serde_json::json!({ "accountId": account }),
        );
        extra.insert("created".to_owned(), serde_json::json!(created));
        extra
    }

    fn history(author: &str, created: &str, items: Vec<ChangelogItem>) -> ChangelogEntry {
        ChangelogEntry {
            id: "h1".into(),
            author: Some(user(author)),
            created: created.to_owned(),
            items,
        }
    }

    fn change(field: &str, from: Option<&str>, to: Option<&str>) -> ChangelogItem {
        ChangelogItem {
            field: field.to_owned(),
            field_id: None,
            from_string: from.map(str::to_owned),
            to_string_value: to.map(str::to_owned),
        }
    }

    fn comment(id: &str, author: &str, created: &str, updated: &str) -> Comment {
        Comment {
            id: id.to_owned(),
            author: user(author),
            update_author: None,
            body: serde_json::json!("repro'd on staging"),
            created: created.to_owned(),
            updated: updated.to_owned(),
        }
    }

    // ── Creation ─────────────────────────────────────────────────────────────

    #[test]
    fn issue_created_in_window_yields_one_entry_despite_an_empty_changelog() {
        // The load-bearing case: Jira records no changegroup for creation, so
        // this entry exists only because it is synthesized from fields.
        let mut i = issue(creator_extra(ME, &jira_ts(3, 9, 0)));
        i.changelog = Some(Changelog::default());
        let got = entries_from_issue(&i, ME, &window(), "https://jira.test");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind, EntryKind::Created);
        assert_eq!(got[0].detail, "Bug");
        assert_eq!(got[0].item.url, "https://jira.test/browse/PROJ-1");
    }

    #[test]
    fn issue_created_by_someone_else_yields_nothing() {
        let i = issue(creator_extra(OTHER, &jira_ts(3, 9, 0)));
        assert!(entries_from_issue(&i, ME, &window(), "https://jira.test").is_empty());
    }

    #[test]
    fn issue_created_before_the_window_yields_nothing() {
        let i = issue(creator_extra(ME, &jira_ts(1, 9, 0)));
        assert!(entries_from_issue(&i, ME, &window(), "https://jira.test").is_empty());
    }

    // ── Changelog ────────────────────────────────────────────────────────────

    #[test]
    fn status_change_becomes_a_transition() {
        let mut i = issue(HashMap::new());
        i.changelog = Some(Changelog {
            histories: vec![history(
                ME,
                &jira_ts(3, 11, 2),
                vec![change("status", Some("To Do"), Some("In Progress"))],
            )],
            ..Changelog::default()
        });
        let got = entries_from_issue(&i, ME, &window(), "https://jira.test");
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0].kind,
            EntryKind::Transition {
                from: "To Do".into(),
                to: "In Progress".into()
            }
        );
        assert_eq!(got[0].detail, "To Do → In Progress");
        assert_eq!(got[0].at, utc(3, 11, 2));
    }

    #[test]
    fn a_history_authored_by_someone_else_is_dropped() {
        let mut i = issue(HashMap::new());
        i.changelog = Some(Changelog {
            histories: vec![history(
                OTHER,
                &jira_ts(3, 11, 2),
                vec![change("status", Some("To Do"), Some("Done"))],
            )],
            ..Changelog::default()
        });
        assert!(entries_from_issue(&i, ME, &window(), "https://jira.test").is_empty());
    }

    #[test]
    fn a_history_outside_the_window_is_dropped() {
        let mut i = issue(HashMap::new());
        i.changelog = Some(Changelog {
            histories: vec![history(
                ME,
                &jira_ts(1, 11, 2),
                vec![change("status", Some("To Do"), Some("Done"))],
            )],
            ..Changelog::default()
        });
        assert!(entries_from_issue(&i, ME, &window(), "https://jira.test").is_empty());
    }

    #[test]
    fn non_status_changes_become_field_changes_and_one_group_can_yield_several() {
        let mut i = issue(HashMap::new());
        i.changelog = Some(Changelog {
            histories: vec![history(
                ME,
                &jira_ts(3, 14, 0),
                vec![
                    change("description", None, Some("A much longer body")),
                    change("Story Points", Some("3"), Some("5")),
                ],
            )],
            ..Changelog::default()
        });
        let got = entries_from_issue(&i, ME, &window(), "https://jira.test");
        assert_eq!(got.len(), 2);
        assert_matches!(got[0].kind, EntryKind::FieldChange { .. });
        assert_eq!(got[1].detail, "Story Points: 5");
    }

    #[test]
    fn a_cleared_field_says_so() {
        let mut i = issue(HashMap::new());
        i.changelog = Some(Changelog {
            histories: vec![history(
                ME,
                &jira_ts(3, 14, 0),
                vec![change("assignee", Some("Someone"), None)],
            )],
            ..Changelog::default()
        });
        let got = entries_from_issue(&i, ME, &window(), "https://jira.test");
        assert_eq!(got[0].detail, "assignee cleared");
    }

    // ── Comments ─────────────────────────────────────────────────────────────

    fn item() -> ItemRef {
        ItemRef {
            key: "PROJ-1".into(),
            title: "Login broken".into(),
            url: "https://jira.test/browse/PROJ-1".into(),
            backend: Backend::Jira,
        }
    }

    #[test]
    fn my_comment_in_the_window_is_kept() {
        let c = comment("10", ME, &jira_ts(3, 14, 20), &jira_ts(3, 14, 20));
        let got = comment_entries(&[c], ME, &window(), &item());
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0].kind,
            EntryKind::Comment {
                id: "10".into(),
                edited: false
            }
        );
        assert_eq!(got[0].detail, "repro'd on staging");
    }

    #[test]
    fn a_comment_i_wrote_before_the_window_is_dropped() {
        let c = comment("10", ME, &jira_ts(1, 14, 20), &jira_ts(1, 14, 20));
        assert!(comment_entries(&[c], ME, &window(), &item()).is_empty());
    }

    #[test]
    fn someone_elses_comment_is_dropped() {
        let c = comment("10", OTHER, &jira_ts(3, 14, 20), &jira_ts(3, 14, 20));
        assert!(comment_entries(&[c], ME, &window(), &item()).is_empty());
    }

    #[test]
    fn a_comment_i_edited_in_the_window_is_caught_via_update_author() {
        let mut c = comment("10", OTHER, &jira_ts(1, 9, 0), &jira_ts(3, 15, 0));
        c.update_author = Some(user(ME));
        let got = comment_entries(&[c], ME, &window(), &item());
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0].kind,
            EntryKind::Comment {
                id: "10".into(),
                edited: true
            }
        );
        assert_eq!(got[0].at, utc(3, 15, 0));
    }

    #[test]
    fn a_comment_i_wrote_and_edited_yields_exactly_one_entry() {
        let mut c = comment("10", ME, &jira_ts(3, 14, 0), &jira_ts(3, 16, 0));
        c.update_author = Some(user(ME));
        let got = comment_entries(&[c], ME, &window(), &item());
        assert_eq!(got.len(), 1, "authorship wins over the edit branch");
        assert_eq!(
            got[0].kind,
            EntryKind::Comment {
                id: "10".into(),
                edited: false
            }
        );
    }

    #[test]
    fn inline_comments_flow_through_entries_from_issue() {
        let mut i = issue(HashMap::new());
        i.fields.comment = Some(CommentList {
            comments: vec![comment("10", ME, &jira_ts(3, 14, 20), &jira_ts(3, 14, 20))],
            total: 1,
            max_results: 1,
            start_at: 0,
        });
        let got = entries_from_issue(&i, ME, &window(), "https://jira.test");
        assert_eq!(got.len(), 1);
        assert_matches!(got[0].kind, EntryKind::Comment { .. });
    }

    // ── Worklogs ─────────────────────────────────────────────────────────────

    #[test]
    fn worklogs_are_placed_on_the_day_the_work_happened() {
        let w = Worklog {
            id: "1".into(),
            author: Some(user(ME)),
            // Typed in on the 4th, but for work started on the 3rd.
            started: jira_ts(3, 9, 0),
            created: Some(jira_ts(4, 8, 0)),
            time_spent_seconds: 5400,
            comment: None,
        };
        let got = worklog_entries(&[w], ME, &window(), &item());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].at, utc(3, 9, 0));
        assert_eq!(got[0].detail, "1h 30m");
    }

    #[test]
    fn someone_elses_worklog_is_dropped() {
        let w = Worklog {
            id: "1".into(),
            author: Some(user(OTHER)),
            started: jira_ts(3, 9, 0),
            created: None,
            time_spent_seconds: 60,
            comment: None,
        };
        assert!(worklog_entries(&[w], ME, &window(), &item()).is_empty());
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(fmt_duration(0), "0m");
        assert_eq!(fmt_duration(2700), "45m");
        assert_eq!(fmt_duration(7200), "2h");
        assert_eq!(fmt_duration(5400), "1h 30m");
    }

    // ── Attribution helpers ──────────────────────────────────────────────────

    #[test]
    fn is_me_matches_either_account_id_or_username() {
        assert!(is_me(Some(&user(ME)), ME));
        assert!(!is_me(Some(&user(OTHER)), ME));
        assert!(!is_me(None, ME));

        // Data Center supplies a username rather than an account id.
        let dc = UserField {
            name: Some("jsmith".into()),
            display_name: None,
            account_id: None,
        };
        assert!(is_me(Some(&dc), "jsmith"));
    }

    #[test]
    fn snippets_elide_long_text_on_a_char_boundary() {
        let long = "ы".repeat(200);
        let out = snippet(&long);
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().count(), SNIPPET_LEN + 1);
    }

    #[test]
    fn snippets_take_the_first_non_blank_line() {
        assert_eq!(snippet("\n\n  hello \nworld"), "hello");
        assert_eq!(snippet(""), "");
    }
}
