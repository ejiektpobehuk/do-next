//! Search feature: local scoring + Jira JQL construction.
//!
//! Local scoring filters issues already loaded; JQL is built for parallel
//! escalation to the Jira search API.

use crate::items::WorkItem;
use crate::jira::types::Issue;

/// User-selected filters in the search overlay.
///
/// An empty list means "no constraint" for that field. `statuses` lists
/// statuses an issue must match; `statuses_exclude` lists statuses that
/// reject an issue. Matching is case-insensitive on the stored values
/// (status name, project key).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchFilters {
    pub statuses: Vec<String>,
    pub statuses_exclude: Vec<String>,
    pub projects: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitOrigin {
    Local,
    Jira,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone)]
pub struct RankedHit {
    pub issue_key: String,
    pub score: i32,
    /// Match offsets in the issue key — reserved for future highlight rendering.
    #[allow(dead_code)]
    pub key_ranges: Vec<MatchRange>,
    /// Match offsets in the summary — reserved for future highlight rendering.
    #[allow(dead_code)]
    pub summary_ranges: Vec<MatchRange>,
    pub origin: HitOrigin,
}

const SCORE_KEY_EXACT: i32 = 1000;
const SCORE_KEY_PREFIX: i32 = 500;
const SCORE_KEY_SUBSTR: i32 = 250;
const SCORE_SUMMARY_TOKEN_PREFIX: i32 = 200;
const SCORE_SUMMARY_SUBSTR: i32 = 100;

/// Score a work item against a local search query + filters.
///
/// Returns `None` if the item should be hidden. When `query` is empty and no
/// filter rejects the item, returns a hit with `score = 0`.
pub fn score_local(query: &str, filters: &SearchFilters, item: &WorkItem) -> Option<RankedHit> {
    score_parts(
        query,
        filters,
        item.key(),
        item.title(),
        item.status_name(),
        item.project_key(),
    )
}

/// Score a raw Jira issue (used for Jira-side search results, which arrive
/// as bare issues before being wrapped into `WorkItem`s).
pub fn score_local_issue(query: &str, filters: &SearchFilters, issue: &Issue) -> Option<RankedHit> {
    score_parts(
        query,
        filters,
        &issue.key,
        &issue.fields.summary,
        &issue.fields.status.name,
        Some(&issue.fields.project.key),
    )
}

fn score_parts(
    query: &str,
    filters: &SearchFilters,
    key: &str,
    summary: &str,
    status: &str,
    project: Option<&str>,
) -> Option<RankedHit> {
    if !filters_match(filters, status, project) {
        return None;
    }

    let q = query.trim();
    if q.is_empty() {
        return Some(RankedHit {
            issue_key: key.to_owned(),
            score: 0,
            key_ranges: Vec::new(),
            summary_ranges: Vec::new(),
            origin: HitOrigin::Local,
        });
    }

    let q_lower = q.to_lowercase();
    let key_lower = key.to_lowercase();
    let summary_lower = summary.to_lowercase();

    let mut score = 0;
    let mut key_ranges = Vec::new();
    let mut summary_ranges = Vec::new();

    // Whole-query key matching.
    if key_lower == q_lower {
        score = score.max(SCORE_KEY_EXACT);
        key_ranges.push(MatchRange {
            start: 0,
            end: key.len(),
        });
    } else if key_lower.starts_with(&q_lower) {
        score = score.max(SCORE_KEY_PREFIX);
        key_ranges.push(MatchRange {
            start: 0,
            end: q_lower.len(),
        });
    } else if let Some(idx) = key_lower.find(&q_lower) {
        score = score.max(SCORE_KEY_SUBSTR);
        key_ranges.push(MatchRange {
            start: idx,
            end: idx + q_lower.len(),
        });
    }

    // Per-token summary matching: every token must appear somewhere in the
    // summary; word-prefix matches outrank mid-word substring matches.
    let tokens: Vec<&str> = q_lower.split_whitespace().collect();
    let mut summary_score = 0;
    let mut all_tokens_matched = !tokens.is_empty();
    for token in &tokens {
        if let Some(hit) = match_summary_token(&summary_lower, token) {
            summary_score += hit.score;
            summary_ranges.push(hit.range);
        } else {
            all_tokens_matched = false;
            break;
        }
    }
    if all_tokens_matched {
        score = score.max(summary_score);
    } else {
        summary_ranges.clear();
    }

    if score == 0 {
        return None;
    }

    Some(RankedHit {
        issue_key: key.to_owned(),
        score,
        key_ranges,
        summary_ranges,
        origin: HitOrigin::Local,
    })
}

struct TokenHit {
    score: i32,
    range: MatchRange,
}

fn match_summary_token(summary_lower: &str, token: &str) -> Option<TokenHit> {
    if token.is_empty() {
        return None;
    }
    // Word-prefix: token starts at the beginning of a word boundary.
    let mut byte_idx = 0;
    for word in summary_lower.split_whitespace() {
        if let Some(off) = summary_lower[byte_idx..].find(word) {
            byte_idx += off;
        }
        if word.starts_with(token) {
            return Some(TokenHit {
                score: SCORE_SUMMARY_TOKEN_PREFIX,
                range: MatchRange {
                    start: byte_idx,
                    end: byte_idx + token.len(),
                },
            });
        }
        byte_idx += word.len();
    }
    // Mid-word substring fallback.
    summary_lower.find(token).map(|idx| TokenHit {
        score: SCORE_SUMMARY_SUBSTR,
        range: MatchRange {
            start: idx,
            end: idx + token.len(),
        },
    })
}

fn filters_match(filters: &SearchFilters, status: &str, project: Option<&str>) -> bool {
    if !filters.statuses.is_empty() {
        let matched = filters
            .statuses
            .iter()
            .any(|s| s.eq_ignore_ascii_case(status));
        if !matched {
            return false;
        }
    }
    if filters
        .statuses_exclude
        .iter()
        .any(|s| s.eq_ignore_ascii_case(status))
    {
        return false;
    }
    if !filters.projects.is_empty() {
        // Items without a project (non-Jira sources) never match a project filter.
        let Some(item_project) = project else {
            return false;
        };
        let matched = filters
            .projects
            .iter()
            .any(|p| p.eq_ignore_ascii_case(item_project));
        if !matched {
            return false;
        }
    }
    true
}

/// Build the JQL to send to Jira for the current search state.
///
/// Returns an empty string when there is nothing to search (no query, no
/// filter). Callers should skip the request in that case.
pub fn build_jql(query: &str, filters: &SearchFilters) -> String {
    let mut clauses: Vec<String> = Vec::new();

    let q = query.trim();
    if !q.is_empty() {
        let escaped = escape_jql_string(q);
        if looks_like_key_fragment(q) {
            clauses.push(format!(
                "(summary ~ \"{escaped}*\" OR key = \"{}\")",
                escape_jql_string(&q.to_uppercase())
            ));
        } else {
            clauses.push(format!("summary ~ \"{escaped}*\""));
        }
    }

    if !filters.statuses.is_empty() {
        let list = filters
            .statuses
            .iter()
            .map(|s| format!("\"{}\"", escape_jql_string(s)))
            .collect::<Vec<_>>()
            .join(", ");
        clauses.push(format!("status in ({list})"));
    }

    if !filters.statuses_exclude.is_empty() {
        let list = filters
            .statuses_exclude
            .iter()
            .map(|s| format!("\"{}\"", escape_jql_string(s)))
            .collect::<Vec<_>>()
            .join(", ");
        clauses.push(format!("status not in ({list})"));
    }

    if !filters.projects.is_empty() {
        let list = filters
            .projects
            .iter()
            .map(|p| format!("\"{}\"", escape_jql_string(p)))
            .collect::<Vec<_>>()
            .join(", ");
        clauses.push(format!("project in ({list})"));
    }

    clauses.join(" AND ")
}

fn looks_like_key_fragment(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Escape a string for embedding inside a JQL double-quoted literal.
pub fn escape_jql_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jira::types::{IssueFields, IssueTypeField, ProjectField, StatusField};
    use std::collections::HashMap;

    fn make_issue(key: &str, summary: &str) -> Issue {
        Issue {
            id: key.into(),
            key: key.into(),
            fields: IssueFields {
                summary: summary.into(),
                status: StatusField {
                    id: "1".into(),
                    name: "To Do".into(),
                },
                priority: None,
                assignee: None,
                reporter: None,
                issuetype: IssueTypeField {
                    id: "1".into(),
                    name: "Task".into(),
                },
                project: ProjectField {
                    id: "10".into(),
                    key: key.split('-').next().unwrap_or("PROJ").into(),
                    name: "Project".into(),
                },
                description: None,
                comment: None,
                attachment: None,
                extra: HashMap::new(),
            },
            source_id: None,
            subsource_idx: 0,
        }
    }

    fn with_status(mut issue: Issue, status: &str) -> Issue {
        issue.fields.status.name = status.into();
        issue
    }

    fn with_project(mut issue: Issue, key: &str) -> Issue {
        issue.fields.project.key = key.into();
        issue
    }

    // ── score_local ──────────────────────────────────────────────────────────

    #[test]
    fn empty_query_no_filters_keeps_issue_with_zero_score() {
        let issue = make_issue("PROJ-1", "Fix login bug");
        let hit = score_local_issue("", &SearchFilters::default(), &issue).unwrap();
        assert_eq!(hit.score, 0);
        assert_eq!(hit.issue_key, "PROJ-1");
        assert!(hit.key_ranges.is_empty());
        assert!(hit.summary_ranges.is_empty());
    }

    #[test]
    fn exact_key_match_wins_over_summary() {
        let issue = make_issue("PROJ-12", "Fix login bug");
        let hit = score_local_issue("PROJ-12", &SearchFilters::default(), &issue).unwrap();
        assert_eq!(hit.score, SCORE_KEY_EXACT);
        assert_eq!(
            hit.key_ranges,
            vec![MatchRange {
                start: 0,
                end: "PROJ-12".len(),
            }]
        );
    }

    #[test]
    fn key_match_is_case_insensitive() {
        let issue = make_issue("PROJ-12", "Whatever");
        let hit = score_local_issue("proj-12", &SearchFilters::default(), &issue).unwrap();
        assert_eq!(hit.score, SCORE_KEY_EXACT);
    }

    #[test]
    fn key_prefix_scores_below_exact() {
        let issue = make_issue("PROJ-12", "Fix login bug");
        let hit = score_local_issue("PROJ", &SearchFilters::default(), &issue).unwrap();
        assert_eq!(hit.score, SCORE_KEY_PREFIX);
    }

    #[test]
    fn summary_token_prefix_outranks_substring() {
        let issue = make_issue("PROJ-12", "Fix login bug");
        let hit = score_local_issue("log", &SearchFilters::default(), &issue).unwrap();
        assert_eq!(hit.score, SCORE_SUMMARY_TOKEN_PREFIX);
        assert_eq!(hit.summary_ranges.len(), 1);
    }

    #[test]
    fn summary_substring_falls_back_to_lower_score() {
        let issue = make_issue("PROJ-12", "Refactor authentication");
        let hit = score_local_issue("entic", &SearchFilters::default(), &issue).unwrap();
        assert_eq!(hit.score, SCORE_SUMMARY_SUBSTR);
    }

    #[test]
    fn multi_token_query_requires_all_tokens_to_match() {
        let issue = make_issue("PROJ-12", "Fix login bug");
        assert!(score_local_issue("fix bug", &SearchFilters::default(), &issue).is_some());
        assert!(score_local_issue("fix banana", &SearchFilters::default(), &issue).is_none());
    }

    #[test]
    fn no_match_returns_none() {
        let issue = make_issue("PROJ-12", "Fix login bug");
        assert!(score_local_issue("banana", &SearchFilters::default(), &issue).is_none());
    }

    #[test]
    fn unicode_summary_matches() {
        let issue = make_issue("PROJ-12", "Починить вход тёмной темы");
        let hit = score_local_issue("тёмной", &SearchFilters::default(), &issue).unwrap();
        assert!(hit.score >= SCORE_SUMMARY_TOKEN_PREFIX);
        let r = &hit.summary_ranges[0];
        assert_eq!(
            &issue.fields.summary.to_lowercase()[r.start..r.end],
            "тёмной"
        );
    }

    #[test]
    fn digit_only_query_matches_key_substring() {
        let issue = make_issue("PROJ-123", "Anything");
        let hit = score_local_issue("123", &SearchFilters::default(), &issue).unwrap();
        assert!(hit.score >= SCORE_KEY_SUBSTR);
    }

    // ── filters_match ────────────────────────────────────────────────────────

    #[test]
    fn empty_filters_accept_everything() {
        let issue = make_issue("PROJ-1", "x");
        assert!(score_local_issue("", &SearchFilters::default(), &issue).is_some());
    }

    #[test]
    fn status_filter_matches_case_insensitive() {
        let issue = with_status(make_issue("PROJ-1", "x"), "In Progress");
        let filters = SearchFilters {
            statuses: vec!["in progress".into()],
            ..SearchFilters::default()
        };
        assert!(score_local_issue("", &filters, &issue).is_some());
    }

    #[test]
    fn status_filter_rejects_non_listed() {
        let issue = with_status(make_issue("PROJ-1", "x"), "Done");
        let filters = SearchFilters {
            statuses: vec!["In Progress".into(), "In Review".into()],
            ..SearchFilters::default()
        };
        assert!(score_local_issue("", &filters, &issue).is_none());
    }

    #[test]
    fn project_filter_matches_case_insensitive() {
        let issue = with_project(make_issue("PROJ-1", "x"), "PLAT");
        let filters = SearchFilters {
            projects: vec!["plat".into()],
            ..SearchFilters::default()
        };
        assert!(score_local_issue("", &filters, &issue).is_some());
    }

    #[test]
    fn project_filter_rejects_non_listed() {
        let issue = with_project(make_issue("PROJ-1", "x"), "OPS");
        let filters = SearchFilters {
            projects: vec!["PLAT".into(), "PROJ".into()],
            ..SearchFilters::default()
        };
        assert!(score_local_issue("", &filters, &issue).is_none());
    }

    #[test]
    fn filters_combine_with_query() {
        let issue = with_status(make_issue("PROJ-1", "Fix login"), "In Progress");
        let filters = SearchFilters {
            statuses: vec!["In Progress".into()],
            ..SearchFilters::default()
        };
        assert!(score_local_issue("login", &filters, &issue).is_some());
        assert!(score_local_issue("banana", &filters, &issue).is_none());
    }

    // ── build_jql ────────────────────────────────────────────────────────────

    #[test]
    fn jql_text_only_emits_key_clause_for_key_fragment() {
        let jql = build_jql("login", &SearchFilters::default());
        assert_eq!(jql, "(summary ~ \"login*\" OR key = \"LOGIN\")");
    }

    #[test]
    fn jql_phrase_query_does_not_emit_key_clause() {
        let jql = build_jql("login bug", &SearchFilters::default());
        assert_eq!(jql, "summary ~ \"login bug*\"");
    }

    #[test]
    fn jql_status_filter_alone() {
        let filters = SearchFilters {
            statuses: vec!["In Progress".into(), "In Review".into()],
            ..SearchFilters::default()
        };
        assert_eq!(
            build_jql("", &filters),
            "status in (\"In Progress\", \"In Review\")"
        );
    }

    #[test]
    fn jql_project_filter_alone() {
        let filters = SearchFilters {
            projects: vec!["PLAT".into(), "PROJ".into()],
            ..SearchFilters::default()
        };
        assert_eq!(build_jql("", &filters), "project in (\"PLAT\", \"PROJ\")");
    }

    #[test]
    fn jql_combines_query_and_filters() {
        let filters = SearchFilters {
            statuses: vec!["In Review".into()],
            statuses_exclude: Vec::new(),
            projects: vec!["PLAT".into()],
        };
        let jql = build_jql("login", &filters);
        assert_eq!(
            jql,
            "(summary ~ \"login*\" OR key = \"LOGIN\") AND status in (\"In Review\") AND project in (\"PLAT\")"
        );
    }

    #[test]
    fn status_exclude_filter_rejects_listed() {
        let issue = with_status(make_issue("PROJ-1", "x"), "Done");
        let filters = SearchFilters {
            statuses_exclude: vec!["done".into()],
            ..SearchFilters::default()
        };
        assert!(score_local_issue("", &filters, &issue).is_none());
    }

    #[test]
    fn status_include_and_exclude_combine() {
        // Include accepts In Progress + In Review, but In Review is excluded.
        let in_progress = with_status(make_issue("PROJ-1", "x"), "In Progress");
        let in_review = with_status(make_issue("PROJ-2", "y"), "In Review");
        let filters = SearchFilters {
            statuses: vec!["In Progress".into(), "In Review".into()],
            statuses_exclude: vec!["In Review".into()],
            ..SearchFilters::default()
        };
        assert!(score_local_issue("", &filters, &in_progress).is_some());
        assert!(score_local_issue("", &filters, &in_review).is_none());
    }

    #[test]
    fn jql_status_exclude_alone() {
        let filters = SearchFilters {
            statuses_exclude: vec!["Done".into(), "Cancelled".into()],
            ..SearchFilters::default()
        };
        assert_eq!(
            build_jql("", &filters),
            "status not in (\"Done\", \"Cancelled\")"
        );
    }

    #[test]
    fn jql_status_include_and_exclude_together() {
        let filters = SearchFilters {
            statuses: vec!["In Progress".into()],
            statuses_exclude: vec!["Blocked".into()],
            ..SearchFilters::default()
        };
        assert_eq!(
            build_jql("", &filters),
            "status in (\"In Progress\") AND status not in (\"Blocked\")"
        );
    }

    #[test]
    fn jql_empty_when_nothing_to_search() {
        assert_eq!(build_jql("", &SearchFilters::default()), "");
        assert_eq!(build_jql("   ", &SearchFilters::default()), "");
    }

    #[test]
    fn jql_escapes_quotes_and_backslashes_in_query() {
        let jql = build_jql("he said \"hi\\bye\"", &SearchFilters::default());
        assert_eq!(jql, "summary ~ \"he said \\\"hi\\\\bye\\\"*\"");
    }

    #[test]
    fn jql_escapes_quotes_in_status_filter() {
        let filters = SearchFilters {
            statuses: vec!["Has \"quotes\"".into()],
            ..SearchFilters::default()
        };
        assert_eq!(
            build_jql("", &filters),
            "status in (\"Has \\\"quotes\\\"\")"
        );
    }

    #[test]
    fn escape_jql_string_handles_quotes_and_backslash() {
        assert_eq!(escape_jql_string("plain"), "plain");
        assert_eq!(escape_jql_string("a\"b"), "a\\\"b");
        assert_eq!(escape_jql_string("a\\b"), "a\\\\b");
        assert_eq!(escape_jql_string("\"\\"), "\\\"\\\\");
    }
}
