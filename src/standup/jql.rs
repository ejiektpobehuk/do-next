//! JQL for standup discovery.
//!
//! Discovery is a deliberately loose *superset*: every JQL date filter is
//! day-granular, and absolute datetime literals are interpreted in the Jira
//! account's own profile timezone rather than ours, so a query can never be
//! trusted to pin the window exactly. Precision comes afterwards, from
//! timestamps we filter ourselves.
//!
//! Two consequences shape everything here:
//! - the lookback is widened by [`SLACK_DAYS`], because extra candidates are
//!   free (verification discards them) while a missed one is invisible;
//! - only *relative* date forms are emitted (`-6d`), which are documented as
//!   relative to the current instant and so dodge the timezone problem entirely.

use crate::jira::jql::escape_string;

/// Extra days added to every discovery lookback, absorbing both the
/// day-rounding of `updatedBy` and any server/client timezone disagreement.
pub const SLACK_DAYS: i64 = 2;

/// Lookback in days for a window spanning `window_days`.
pub const fn lookback_days(window_days: i64) -> i64 {
    window_days + SLACK_DAYS
}

/// The candidate query: issues you plausibly touched in the window.
///
/// `user` is the literal for `updatedBy`, which is the broadest single term
/// available — it matches issue creation, any field update, and comment
/// create/delete/edit. It also refuses `currentUser()`, hence passing the
/// literal; when the caller's probe found it unusable, `updated_by` is `false`
/// and the remaining terms carry the query alone.
///
/// `creator` rather than `reporter`: reporter is mutable and routinely set to
/// someone else for on-behalf-of creation.
///
/// `status CHANGED BY currentUser()` is belt-and-braces, not a replacement —
/// `WAS`/`CHANGED` only work on assignee, fixVersion, priority, reporter,
/// resolution and status, so it can never satisfy "any field change".
pub fn discovery(user: &str, window_days: i64, extra: Option<&str>, updated_by: bool) -> String {
    let days = lookback_days(window_days);
    let mut terms: Vec<String> = Vec::new();

    if updated_by {
        terms.push(format!(
            "issuekey IN updatedBy(\"{}\", \"-{days}d\")",
            escape_string(user)
        ));
    }
    terms.push(format!("(creator = currentUser() AND created >= -{days}d)"));
    terms.push(format!("(status CHANGED BY currentUser() AFTER -{days}d)"));

    let mut jql = format!("({})", terms.join(" OR "));
    if let Some(extra) = extra.map(str::trim).filter(|e| !e.is_empty()) {
        jql = format!("{jql} AND ({extra})");
    }
    jql
}

/// Discovery for logged work.
///
/// Kept separate from [`discovery`] on purpose: `worklogAuthor` and
/// `worklogDate` are *uncorrelated* clauses — an issue where you logged work
/// last month and a colleague logged some yesterday satisfies both — so folded
/// into the main OR they would give no way to tell which candidates are worth a
/// worklog lookup. Alone, this narrows to the handful that are.
pub fn worklog_discovery(window_days: i64, extra: Option<&str>) -> String {
    let days = lookback_days(window_days);
    let mut jql = format!("(worklogAuthor = currentUser() AND worklogDate >= -{days}d)");
    if let Some(extra) = extra.map(str::trim).filter(|e| !e.is_empty()) {
        jql = format!("{jql} AND ({extra})");
    }
    jql
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookback_adds_slack() {
        assert_eq!(lookback_days(1), 3);
        assert_eq!(lookback_days(3), 5);
    }

    #[test]
    fn discovery_includes_every_term() {
        let jql = discovery("557058:abc", 3, None, true);
        assert!(
            jql.contains(r#"issuekey IN updatedBy("557058:abc", "-5d")"#),
            "{jql}"
        );
        assert!(
            jql.contains("creator = currentUser() AND created >= -5d"),
            "{jql}"
        );
        assert!(
            jql.contains("status CHANGED BY currentUser() AFTER -5d"),
            "{jql}"
        );
    }

    #[test]
    fn discovery_uses_creator_not_reporter() {
        // reporter is mutable, so it would miss issues filed on someone's behalf.
        let jql = discovery("me", 1, None, true);
        assert!(jql.contains("creator = currentUser()"));
        assert!(!jql.contains("reporter"));
    }

    #[test]
    fn discovery_emits_no_absolute_datetime_literal() {
        // Absolute literals resolve in the *server's* account timezone, which is
        // the bug this rule exists to prevent.
        let jql = discovery("me", 7, Some("project = PROJ"), true);
        assert!(!jql.contains("20"), "no year literal expected in {jql}");
        assert!(!jql.contains(':'), "no clock time expected in {jql}");
    }

    #[test]
    fn discovery_without_updated_by_still_has_the_other_terms() {
        let jql = discovery("me", 2, None, false);
        assert!(!jql.contains("updatedBy"), "{jql}");
        assert!(jql.contains("creator = currentUser()"), "{jql}");
        assert!(jql.contains("status CHANGED BY currentUser()"), "{jql}");
        // Still a well-formed disjunction.
        assert!(jql.starts_with('(') && jql.ends_with(')'), "{jql}");
    }

    #[test]
    fn extra_jql_is_anded_and_parenthesised() {
        let jql = discovery("me", 1, Some("project in (A, B)"), true);
        assert!(jql.ends_with("AND (project in (A, B))"), "{jql}");
        // Blank extra must not produce a dangling AND.
        assert!(!discovery("me", 1, Some("   "), true).contains("AND ("));
    }

    #[test]
    fn user_literal_is_escaped() {
        let jql = discovery(r#"od"d"#, 1, None, true);
        assert!(jql.contains(r#"updatedBy("od\"d", "-3d")"#), "{jql}");
    }

    #[test]
    fn worklog_discovery_is_separate_and_relative() {
        let jql = worklog_discovery(3, None);
        assert!(jql.contains("worklogAuthor = currentUser()"), "{jql}");
        assert!(jql.contains("worklogDate >= -5d"), "{jql}");
        assert!(!jql.contains("updatedBy"), "{jql}");

        let scoped = worklog_discovery(3, Some("project = PROJ"));
        assert!(scoped.ends_with("AND (project = PROJ)"), "{scoped}");
    }
}
