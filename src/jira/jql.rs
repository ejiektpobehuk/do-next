//! JQL string helpers.

/// Escape a value for use inside a double-quoted JQL string literal.
///
/// Mostly defensive: Cloud account ids are alphanumeric. It matters on Data
/// Center, where the user literal is a username that may contain a quote or
/// backslash, and where an unescaped one turns a query into a syntax error
/// rather than a wrong answer.
pub fn escape_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// JQL for the create form's epic picker: open epics in `project`,
/// most-recently-touched first, narrowed by `query` when there is one.
///
/// Epics whose status category is Done are left out — a closed epic is not
/// somewhere new work goes, and on a long-lived project they would otherwise
/// crowd out the ones being worked on.
///
/// `query` is matched against the summary, and additionally taken as an issue
/// key when it looks like one, so pasting `PROJ-12` finds that epic directly.
pub fn epic_search_jql(project: &str, query: &str) -> String {
    let mut jql = format!(
        r#"project = "{}" AND issuetype = Epic AND statusCategory != Done"#,
        escape_string(project)
    );
    let mut clauses: Vec<String> = Vec::new();
    if let Some(text) = text_query(query) {
        clauses.push(format!(r#"summary ~ "{text}*""#));
    }
    if looks_like_issue_key(query.trim()) {
        clauses.push(format!(r#"key = "{}""#, escape_string(query.trim())));
    }
    if !clauses.is_empty() {
        jql.push_str(" AND (");
        jql.push_str(&clauses.join(" OR "));
        jql.push(')');
    }
    jql.push_str(" ORDER BY updated DESC");
    jql
}

/// JQL for the create form's linked-issue picker, most-recently-touched first.
///
/// Unlike the epic picker this is not confined to one project: a link commonly
/// crosses projects, and the issue on the other end is usually one the user can
/// name. So an empty query lists recent issues in `project` — the useful
/// default when there is nothing to go on — and a typed query searches the
/// whole site.
///
/// `query` is matched against the summary, and additionally taken as an issue
/// key when it looks like one, so pasting `OPS-42` finds that issue directly.
pub fn link_search_jql(project: &str, query: &str) -> String {
    let mut clauses: Vec<String> = Vec::new();
    if let Some(text) = text_query(query) {
        clauses.push(format!(r#"summary ~ "{text}*""#));
    }
    if looks_like_issue_key(query.trim()) {
        clauses.push(format!(r#"key = "{}""#, escape_string(query.trim())));
    }
    let mut jql = if clauses.is_empty() {
        format!(r#"project = "{}""#, escape_string(project))
    } else {
        format!("({})", clauses.join(" OR "))
    };
    jql.push_str(" ORDER BY updated DESC");
    jql
}

/// Reduce a picker query to something safe on the right of `~`.
///
/// The value reaches Lucene, not just JQL, so its operators (`*?~^:` and
/// friends) would be read as syntax and a half-typed one is a 400 rather than
/// no matches. Dropping them costs nothing here: a picker query is a name
/// fragment. `None` when nothing searchable is left.
fn text_query(query: &str) -> Option<String> {
    const LUCENE_SPECIAL: [char; 19] = [
        '+', '-', '&', '|', '!', '(', ')', '{', '}', '[', ']', '^', '"', '~', '*', '?', ':', '\\',
        '/',
    ];
    let cleaned = query
        .chars()
        .map(|c| if LUCENE_SPECIAL.contains(&c) { ' ' } else { c })
        .collect::<String>();
    let words: Vec<&str> = cleaned.split_whitespace().collect();
    (!words.is_empty()).then(|| words.join(" "))
}

/// Whether `value` has the shape of an issue key (`ABC-123`). Asking Jira for
/// `key = "not a key"` is a syntax error, so the clause is only added when the
/// query could actually be one.
fn looks_like_issue_key(value: &str) -> bool {
    let Some((project, number)) = value.split_once('-') else {
        return false;
    };
    !project.is_empty()
        && project.starts_with(|c: char| c.is_ascii_alphabetic())
        && project
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !number.is_empty()
        && number.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_plain_values_alone() {
        assert_eq!(escape_string("557058:abc-123"), "557058:abc-123");
        assert_eq!(escape_string("jsmith"), "jsmith");
    }

    #[test]
    fn escapes_quotes_and_backslashes() {
        assert_eq!(escape_string(r#"a"b"#), r#"a\"b"#);
        assert_eq!(escape_string(r"a\b"), r"a\\b");
        // Backslash first, so an escaped quote is not double-escaped.
        assert_eq!(escape_string(r#"a\"b"#), r#"a\\\"b"#);
    }

    #[test]
    fn epic_jql_without_a_query_lists_open_epics() {
        assert_eq!(
            epic_search_jql("PROJ", ""),
            r#"project = "PROJ" AND issuetype = Epic AND statusCategory != Done ORDER BY updated DESC"#
        );
    }

    #[test]
    fn epic_jql_matches_the_summary() {
        assert_eq!(
            epic_search_jql("PROJ", "pay"),
            r#"project = "PROJ" AND issuetype = Epic AND statusCategory != Done AND (summary ~ "pay*") ORDER BY updated DESC"#
        );
    }

    #[test]
    fn epic_jql_also_tries_a_pasted_key() {
        let jql = epic_search_jql("PROJ", "PROJ-12");
        assert!(jql.contains(r#"summary ~ "PROJ 12*""#), "{jql}");
        assert!(jql.contains(r#"key = "PROJ-12""#), "{jql}");
    }

    #[test]
    fn epic_jql_drops_lucene_operators_from_the_query() {
        // Half-typed operators would be a 400, not an empty result.
        let jql = epic_search_jql("PROJ", "pay~ (");
        assert!(jql.contains(r#"summary ~ "pay*""#), "{jql}");
    }

    #[test]
    fn epic_jql_omits_a_query_with_nothing_searchable_left() {
        assert_eq!(
            epic_search_jql("PROJ", "  ?? "),
            epic_search_jql("PROJ", "")
        );
    }

    #[test]
    fn link_jql_without_a_query_lists_the_project() {
        assert_eq!(
            link_search_jql("PROJ", ""),
            r#"project = "PROJ" ORDER BY updated DESC"#
        );
    }

    #[test]
    fn link_jql_with_a_query_searches_every_project() {
        let jql = link_search_jql("PROJ", "payment");
        assert_eq!(
            jql, r#"(summary ~ "payment*") ORDER BY updated DESC"#,
            "a typed query must not stay inside the form's project"
        );
    }

    #[test]
    fn link_jql_also_tries_a_pasted_key() {
        let jql = link_search_jql("PROJ", "OPS-42");
        assert!(jql.contains(r#"summary ~ "OPS 42*""#), "{jql}");
        assert!(jql.contains(r#"key = "OPS-42""#), "{jql}");
    }

    #[test]
    fn link_jql_falls_back_to_the_project_when_nothing_is_searchable() {
        assert_eq!(link_search_jql("PROJ", " ?? "), link_search_jql("PROJ", ""));
    }

    #[test]
    fn issue_key_shape_is_project_dash_number() {
        assert!(looks_like_issue_key("PROJ-12"));
        assert!(looks_like_issue_key("a1_b-7"));
        assert!(!looks_like_issue_key("PROJ"));
        assert!(!looks_like_issue_key("PROJ-"));
        assert!(!looks_like_issue_key("-12"));
        assert!(!looks_like_issue_key("1PROJ-12"));
        assert!(!looks_like_issue_key("PROJ-12a"));
        assert!(!looks_like_issue_key("some epic-1"));
    }
}
