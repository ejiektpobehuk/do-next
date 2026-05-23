//! Search feature: local scoring + Jira JQL construction.
//!
//! Local scoring filters issues already loaded; JQL is built for parallel
//! escalation to the Jira search API.

use serde_json::Value;

use crate::jira::types::Issue;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct ChipSet {
    pub mine: bool,
    pub unassigned: bool,
    pub in_review: bool,
    pub active_sprint: bool,
    pub global: bool,
}

impl ChipSet {
    pub const fn any_filter(self) -> bool {
        self.mine || self.unassigned || self.in_review || self.active_sprint
    }
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
    pub key_ranges: Vec<MatchRange>,
    pub summary_ranges: Vec<MatchRange>,
    pub origin: HitOrigin,
}

const SCORE_KEY_EXACT: i32 = 1000;
const SCORE_KEY_PREFIX: i32 = 500;
const SCORE_KEY_SUBSTR: i32 = 250;
const SCORE_SUMMARY_TOKEN_PREFIX: i32 = 200;
const SCORE_SUMMARY_SUBSTR: i32 = 100;

/// Score an issue against a local search query + chip filters.
///
/// Returns `None` if the issue should be hidden. When `query` is empty and no
/// chip filter rejects the issue, returns a hit with `score = 0`.
pub fn score_local(
    query: &str,
    chips: ChipSet,
    issue: &Issue,
    me: Option<&str>,
) -> Option<RankedHit> {
    if !chips_match(chips, issue, me) {
        return None;
    }

    let q = query.trim();
    if q.is_empty() {
        return Some(RankedHit {
            issue_key: issue.key.clone(),
            score: 0,
            key_ranges: Vec::new(),
            summary_ranges: Vec::new(),
            origin: HitOrigin::Local,
        });
    }

    let q_lower = q.to_lowercase();
    let key_lower = issue.key.to_lowercase();
    let summary_lower = issue.fields.summary.to_lowercase();

    let mut score = 0;
    let mut key_ranges = Vec::new();
    let mut summary_ranges = Vec::new();

    // Whole-query key matching.
    if key_lower == q_lower {
        score = score.max(SCORE_KEY_EXACT);
        key_ranges.push(MatchRange {
            start: 0,
            end: issue.key.len(),
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
        issue_key: issue.key.clone(),
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

fn chips_match(chips: ChipSet, issue: &Issue, me: Option<&str>) -> bool {
    if chips.mine {
        let assignee = issue.fields.assignee.as_ref();
        let matched = me.is_some_and(|m| {
            assignee.is_some_and(|a| {
                a.name.as_deref() == Some(m)
                    || a.account_id.as_deref() == Some(m)
                    || a.display_name.as_deref() == Some(m)
            })
        });
        if !matched {
            return false;
        }
    }
    if chips.unassigned && issue.fields.assignee.is_some() {
        return false;
    }
    if chips.in_review && !issue.fields.status.name.to_lowercase().contains("review") {
        return false;
    }
    if chips.active_sprint && !has_active_sprint(issue) {
        return false;
    }
    true
}

fn has_active_sprint(issue: &Issue) -> bool {
    issue.fields.extra.values().any(|v| match v {
        Value::Array(arr) => arr.iter().any(is_active_sprint_value),
        _ => false,
    })
}

fn is_active_sprint_value(v: &Value) -> bool {
    v.as_object()
        .and_then(|o| o.get("state"))
        .and_then(Value::as_str)
        .is_some_and(|s| s.eq_ignore_ascii_case("active"))
}

/// Build the JQL to send to Jira for the current search state.
///
/// Returns an empty string when there is nothing to search (no query, no chip
/// filter, no scope). Callers should skip the request in that case.
pub fn build_jql(query: &str, chips: ChipSet, team_projects: &[String]) -> String {
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

    if chips.mine {
        clauses.push("assignee = currentUser()".into());
    }
    if chips.unassigned {
        clauses.push("assignee is EMPTY".into());
    }
    if chips.in_review {
        clauses.push("status = \"In Review\"".into());
    }
    if chips.active_sprint {
        clauses.push("sprint in openSprints()".into());
    }

    if !chips.global && !team_projects.is_empty() {
        let list = team_projects
            .iter()
            .map(|p| escape_jql_string(p))
            .map(|p| format!("\"{p}\""))
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
    use crate::jira::types::{IssueFields, IssueTypeField, ProjectField, StatusField, UserField};
    use serde_json::json;
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

    fn with_assignee(mut issue: Issue, name: &str) -> Issue {
        issue.fields.assignee = Some(UserField {
            name: Some(name.into()),
            display_name: Some(name.into()),
            account_id: Some(name.into()),
        });
        issue
    }

    fn with_sprint(mut issue: Issue, state: &str) -> Issue {
        issue.fields.extra.insert(
            "customfield_10020".into(),
            json!([{"id": 1, "name": "Sprint 1", "state": state}]),
        );
        issue
    }

    // ── score_local ──────────────────────────────────────────────────────────

    #[test]
    fn empty_query_no_chips_keeps_issue_with_zero_score() {
        let issue = make_issue("PROJ-1", "Fix login bug");
        let hit = score_local("", ChipSet::default(), &issue, None).unwrap();
        assert_eq!(hit.score, 0);
        assert_eq!(hit.issue_key, "PROJ-1");
        assert!(hit.key_ranges.is_empty());
        assert!(hit.summary_ranges.is_empty());
    }

    #[test]
    fn exact_key_match_wins_over_summary() {
        let issue = make_issue("PROJ-12", "Fix login bug");
        let hit = score_local("PROJ-12", ChipSet::default(), &issue, None).unwrap();
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
        let hit = score_local("proj-12", ChipSet::default(), &issue, None).unwrap();
        assert_eq!(hit.score, SCORE_KEY_EXACT);
    }

    #[test]
    fn key_prefix_scores_below_exact() {
        let issue = make_issue("PROJ-12", "Fix login bug");
        let hit = score_local("PROJ", ChipSet::default(), &issue, None).unwrap();
        assert_eq!(hit.score, SCORE_KEY_PREFIX);
    }

    #[test]
    fn summary_token_prefix_outranks_substring() {
        let issue = make_issue("PROJ-12", "Fix login bug");
        let hit = score_local("log", ChipSet::default(), &issue, None).unwrap();
        assert_eq!(hit.score, SCORE_SUMMARY_TOKEN_PREFIX);
        assert_eq!(hit.summary_ranges.len(), 1);
    }

    #[test]
    fn summary_substring_falls_back_to_lower_score() {
        let issue = make_issue("PROJ-12", "Refactor authentication");
        let hit = score_local("entic", ChipSet::default(), &issue, None).unwrap();
        assert_eq!(hit.score, SCORE_SUMMARY_SUBSTR);
    }

    #[test]
    fn multi_token_query_requires_all_tokens_to_match() {
        let issue = make_issue("PROJ-12", "Fix login bug");
        assert!(score_local("fix bug", ChipSet::default(), &issue, None).is_some());
        assert!(score_local("fix banana", ChipSet::default(), &issue, None).is_none());
    }

    #[test]
    fn no_match_returns_none() {
        let issue = make_issue("PROJ-12", "Fix login bug");
        assert!(score_local("banana", ChipSet::default(), &issue, None).is_none());
    }

    #[test]
    fn unicode_summary_matches() {
        let issue = make_issue("PROJ-12", "Починить вход тёмной темы");
        let hit = score_local("тёмной", ChipSet::default(), &issue, None).unwrap();
        assert!(hit.score >= SCORE_SUMMARY_TOKEN_PREFIX);
        let r = &hit.summary_ranges[0];
        assert_eq!(&issue.fields.summary.to_lowercase()[r.start..r.end], "тёмной");
    }

    // ── chip filters ─────────────────────────────────────────────────────────

    #[test]
    fn chip_mine_matches_when_assignee_equals_me() {
        let issue = with_assignee(make_issue("PROJ-1", "x"), "alice");
        let chips = ChipSet {
            mine: true,
            ..ChipSet::default()
        };
        assert!(score_local("", chips, &issue, Some("alice")).is_some());
        assert!(score_local("", chips, &issue, Some("bob")).is_none());
    }

    #[test]
    fn chip_mine_filters_out_unassigned() {
        let issue = make_issue("PROJ-1", "x");
        let chips = ChipSet {
            mine: true,
            ..ChipSet::default()
        };
        assert!(score_local("", chips, &issue, Some("alice")).is_none());
    }

    #[test]
    fn chip_unassigned_matches_only_when_no_assignee() {
        let chips = ChipSet {
            unassigned: true,
            ..ChipSet::default()
        };
        let unassigned = make_issue("PROJ-1", "x");
        let assigned = with_assignee(make_issue("PROJ-2", "x"), "alice");
        assert!(score_local("", chips, &unassigned, None).is_some());
        assert!(score_local("", chips, &assigned, None).is_none());
    }

    #[test]
    fn chip_in_review_matches_status_containing_review() {
        let chips = ChipSet {
            in_review: true,
            ..ChipSet::default()
        };
        let in_review = with_status(make_issue("PROJ-1", "x"), "In Review");
        let code_review = with_status(make_issue("PROJ-2", "x"), "Code Review");
        let done = with_status(make_issue("PROJ-3", "x"), "Done");
        assert!(score_local("", chips, &in_review, None).is_some());
        assert!(score_local("", chips, &code_review, None).is_some());
        assert!(score_local("", chips, &done, None).is_none());
    }

    #[test]
    fn chip_active_sprint_finds_open_sprint_in_extra() {
        let chips = ChipSet {
            active_sprint: true,
            ..ChipSet::default()
        };
        let active = with_sprint(make_issue("PROJ-1", "x"), "active");
        let closed = with_sprint(make_issue("PROJ-2", "x"), "closed");
        let none = make_issue("PROJ-3", "x");
        assert!(score_local("", chips, &active, None).is_some());
        assert!(score_local("", chips, &closed, None).is_none());
        assert!(score_local("", chips, &none, None).is_none());
    }

    #[test]
    fn chips_combine_with_query() {
        let chips = ChipSet {
            mine: true,
            ..ChipSet::default()
        };
        let mine_match = with_assignee(make_issue("PROJ-1", "Fix login"), "alice");
        let mine_no_query = with_assignee(make_issue("PROJ-2", "Refactor"), "alice");
        assert!(score_local("login", chips, &mine_match, Some("alice")).is_some());
        assert!(score_local("login", chips, &mine_no_query, Some("alice")).is_none());
    }

    // ── build_jql ────────────────────────────────────────────────────────────

    #[test]
    fn jql_text_only_with_project_scope() {
        let jql = build_jql("login", ChipSet::default(), &["PROJ".into()]);
        assert_eq!(
            jql,
            "(summary ~ \"login*\" OR key = \"LOGIN\") AND project in (\"PROJ\")"
        );
    }

    #[test]
    fn jql_phrase_query_does_not_emit_key_clause() {
        let jql = build_jql("login bug", ChipSet::default(), &["PROJ".into()]);
        assert_eq!(
            jql,
            "summary ~ \"login bug*\" AND project in (\"PROJ\")"
        );
    }

    #[test]
    fn jql_each_chip_alone() {
        let mine = ChipSet {
            mine: true,
            ..ChipSet::default()
        };
        assert_eq!(
            build_jql("", mine, &["P".into()]),
            "assignee = currentUser() AND project in (\"P\")"
        );

        let unassigned = ChipSet {
            unassigned: true,
            ..ChipSet::default()
        };
        assert_eq!(
            build_jql("", unassigned, &["P".into()]),
            "assignee is EMPTY AND project in (\"P\")"
        );

        let review = ChipSet {
            in_review: true,
            ..ChipSet::default()
        };
        assert_eq!(
            build_jql("", review, &["P".into()]),
            "status = \"In Review\" AND project in (\"P\")"
        );

        let sprint = ChipSet {
            active_sprint: true,
            ..ChipSet::default()
        };
        assert_eq!(
            build_jql("", sprint, &["P".into()]),
            "sprint in openSprints() AND project in (\"P\")"
        );
    }

    #[test]
    fn jql_global_chip_drops_project_scope() {
        let chips = ChipSet {
            global: true,
            mine: true,
            ..ChipSet::default()
        };
        assert_eq!(build_jql("", chips, &["P".into()]), "assignee = currentUser()");
    }

    #[test]
    fn jql_combines_query_and_chips_and_multi_project() {
        let chips = ChipSet {
            mine: true,
            in_review: true,
            ..ChipSet::default()
        };
        let jql = build_jql("login", chips, &["P1".into(), "P2".into()]);
        assert_eq!(
            jql,
            "(summary ~ \"login*\" OR key = \"LOGIN\") AND assignee = currentUser() AND status = \"In Review\" AND project in (\"P1\", \"P2\")"
        );
    }

    #[test]
    fn jql_empty_when_nothing_to_search() {
        assert_eq!(build_jql("", ChipSet::default(), &[]), "");
        assert_eq!(build_jql("   ", ChipSet::default(), &[]), "");
    }

    #[test]
    fn jql_escapes_quotes_and_backslashes_in_query() {
        let jql = build_jql("he said \"hi\\bye\"", ChipSet::default(), &[]);
        assert_eq!(jql, "summary ~ \"he said \\\"hi\\\\bye\\\"*\"");
    }

    #[test]
    fn escape_jql_string_handles_quotes_and_backslash() {
        assert_eq!(escape_jql_string("plain"), "plain");
        assert_eq!(escape_jql_string("a\"b"), "a\\\"b");
        assert_eq!(escape_jql_string("a\\b"), "a\\\\b");
        assert_eq!(escape_jql_string("\"\\"), "\\\"\\\\");
    }
}
