//! GitLab wire types, the display-ready [`MergeRequest`] handed to the TUI as
//! a `WorkItem`, and the pure helpers that map between them (query building,
//! status digest, list labels, field ids).

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::types::{DraftFilter, GitlabFilters, GitlabLabel, GitlabRole};

// ── Wire types ────────────────────────────────────────────────────────────────

/// The user a personal access token belongs to (`GET /api/v4/user`).
#[derive(Debug, Clone, Deserialize)]
pub struct ApiUser {
    pub id: u64,
    pub username: String,
    #[serde(default)]
    pub name: Option<String>,
}

impl ApiUser {
    /// Display name, falling back to the handle.
    pub fn display(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.username)
    }
}

/// Wire shape of one merge request from any of the three list endpoints.
///
/// The list endpoints omit approval state and the head pipeline entirely; both
/// are filled in afterwards (see `GitlabClient::enrich`).
#[derive(Debug, Clone, Deserialize)]
pub struct ApiMergeRequest {
    pub iid: u64,
    pub project_id: u64,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    pub state: String,
    pub web_url: String,
    #[serde(default)]
    pub source_branch: Option<String>,
    #[serde(default)]
    pub target_branch: Option<String>,
    /// Present on modern instances; older ones only send `work_in_progress`.
    #[serde(default)]
    pub draft: Option<bool>,
    #[serde(default)]
    pub work_in_progress: Option<bool>,
    #[serde(default)]
    pub author: Option<ApiUser>,
    #[serde(default)]
    pub assignees: Vec<ApiUser>,
    #[serde(default)]
    pub reviewers: Vec<ApiUser>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
    /// Set only on merged/closed merge requests. These are what make a standup
    /// entry high-confidence: `updated_at` moves when anyone touches the MR,
    /// but these two only move when it was merged or closed.
    #[serde(default)]
    pub merged_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub closed_at: Option<DateTime<Utc>>,
    /// `null` on endpoints/instances that don't compute it — treated as "no
    /// known conflict", never as a conflict.
    #[serde(default)]
    pub has_conflicts: Option<bool>,
    #[serde(default)]
    pub user_notes_count: Option<u64>,
    #[serde(default)]
    pub blocking_discussions_resolved: Option<bool>,
    /// Reference forms; `full` is `group/project!iid`, the only place the list
    /// payload carries the project path.
    #[serde(default)]
    pub references: Option<ApiReferences>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiReferences {
    #[serde(default)]
    pub full: Option<String>,
}

/// One entry of `GET /events` (the authenticated user's contribution feed).
///
/// Only read when a standup source opts into `gitlab.events`. It is the sole way
/// to see a merge request you closed but did not author — there is no
/// `closed_by` filter on the merge-requests endpoint — at the cost of
/// day-granular bounds and a broader token scope.
#[derive(Debug, Clone, Deserialize)]
pub struct ApiEvent {
    #[serde(default)]
    pub action_name: Option<String>,
    #[serde(default)]
    pub target_type: Option<String>,
    #[serde(default)]
    pub target_iid: Option<u64>,
    #[serde(default)]
    pub target_title: Option<String>,
    #[serde(default)]
    pub project_id: Option<u64>,
    pub created_at: DateTime<Utc>,
}

/// Subset of `GET /projects` used for "repositories I created".
#[derive(Debug, Clone, Deserialize)]
pub struct ApiProject {
    pub id: u64,
    #[serde(default)]
    pub path_with_namespace: Option<String>,
    pub name: String,
    pub web_url: String,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
}

/// `GET /projects/{id}/merge_requests/{iid}/approvals`.
#[derive(Debug, Clone, Deserialize)]
pub struct ApiApprovals {
    #[serde(default)]
    pub approvals_required: Option<u32>,
    #[serde(default)]
    pub approvals_left: Option<u32>,
    #[serde(default)]
    pub approved_by: Vec<ApiApprover>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiApprover {
    pub user: ApiUser,
}

/// A merge request's head pipeline, only present on the single-MR endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct ApiPipeline {
    #[serde(default)]
    pub status: Option<String>,
}

/// The single-MR endpoint response — fetched purely for `head_pipeline`.
#[derive(Debug, Clone, Deserialize)]
pub struct ApiMergeRequestDetail {
    #[serde(default)]
    pub head_pipeline: Option<ApiPipeline>,
}

// ── Display type ──────────────────────────────────────────────────────────────

/// Display-ready GitLab merge request handed to the TUI as a `WorkItem`.
///
/// `Serialize`/`Deserialize` are required by the on-disk source cache.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MergeRequest {
    /// Work-item key (`MR:{project_path}!{iid}`) — cannot collide with Jira
    /// issue keys (`[A-Z]+-[0-9]+`) or Confluence's `CONF:` prefix, which also
    /// namespaces hidden-for-a-day entries.
    pub key: String,
    pub project_id: u64,
    pub iid: u64,
    /// Full path, e.g. "backend/api". Absent when `references.full` was.
    pub project_path: Option<String>,
    pub title: String,
    /// GitLab state string: "opened" | "merged" | "closed" | "locked".
    pub state: String,
    /// The ≤16-char list-row status, derived from everything below via
    /// [`status_digest`]. Stored so the shared `status_name()` accessor can
    /// borrow it; recomputed whenever enrichment lands.
    pub status_label: String,
    pub web_url: String,
    pub draft: bool,
    pub has_conflicts: bool,
    pub source_branch: Option<String>,
    pub target_branch: Option<String>,
    pub author: Option<String>,
    pub assignees: Vec<String>,
    pub reviewers: Vec<String>,
    pub labels: Vec<String>,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    /// Merge/close instants. `default` because the on-disk source cache holds
    /// payloads written before these were read.
    #[serde(default)]
    pub merged_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub closed_at: Option<DateTime<Utc>>,
    pub user_notes_count: u64,
    pub threads_resolved: Option<bool>,
    /// Approval state, filled in by enrichment (`None` when it failed).
    pub approvals_required: Option<u32>,
    pub approvals_left: Option<u32>,
    pub approved_by: Vec<String>,
    /// Head-pipeline status, filled in by enrichment.
    pub ci_status: Option<String>,
    /// Which source this merge request was fetched from (set after fetch).
    pub source_id: Option<String>,
    /// Field map rendered by the default/custom views, keyed by `gl.*` ids.
    pub extra: HashMap<String, serde_json::Value>,
}

impl MergeRequest {
    /// The list-row label per the source's `label` option. Falls back to the
    /// title when the project path is unavailable.
    pub fn list_label(&self, mode: GitlabLabel) -> String {
        match mode {
            GitlabLabel::Title => self.title.clone(),
            GitlabLabel::Project => self
                .project_path
                .clone()
                .unwrap_or_else(|| self.title.clone()),
            GitlabLabel::Both => self.project_path.as_ref().map_or_else(
                || self.title.clone(),
                |project| format!("{} · {project}", self.title),
            ),
        }
    }

    /// `!123` — the short reference shown ahead of the title in the list.
    pub fn short_ref(&self) -> String {
        format!("!{}", self.iid)
    }

    /// Human-readable "2 of 3 approvals" style summary for the detail view.
    pub fn approvals_summary(&self) -> String {
        match (self.approvals_required, self.approvals_left) {
            (Some(required), Some(left)) => {
                let given = required.saturating_sub(left);
                let names = if self.approved_by.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", self.approved_by.join(", "))
                };
                format!("{given}/{required}{names}")
            }
            _ if !self.approved_by.is_empty() => {
                format!("approved by {}", self.approved_by.join(", "))
            }
            _ => "—".to_owned(),
        }
    }
}

// ── Field ids ─────────────────────────────────────────────────────────────────

/// Stable field ids exposed to the view layer for merge requests.
pub const FIELD_APPROVALS: &str = "gl.approvals";
pub const FIELD_AUTHOR: &str = "gl.author";
pub const FIELD_BRANCHES: &str = "gl.branches";
pub const FIELD_CI: &str = "gl.ci";
pub const FIELD_DESCRIPTION: &str = "gl.description";
pub const FIELD_LABELS: &str = "gl.labels";
pub const FIELD_PROJECT: &str = "gl.project";
pub const FIELD_REVIEWERS: &str = "gl.reviewers";
pub const FIELD_STATE: &str = "gl.state";
pub const FIELD_THREADS: &str = "gl.threads";
pub const FIELD_UPDATED: &str = "gl.updated";
pub const FIELD_URL: &str = "gl.url";

/// Builtin display names for the `gl.*` field ids (no editmeta exists).
pub fn field_name(field_id: &str) -> Option<&'static str> {
    match field_id {
        FIELD_APPROVALS => Some("Approvals"),
        FIELD_AUTHOR => Some("Author"),
        FIELD_BRANCHES => Some("Branches"),
        FIELD_CI => Some("CI"),
        FIELD_DESCRIPTION => Some("Description"),
        FIELD_LABELS => Some("Labels"),
        FIELD_PROJECT => Some("Project"),
        FIELD_REVIEWERS => Some("Reviewers"),
        FIELD_STATE => Some("State"),
        FIELD_THREADS => Some("Threads"),
        FIELD_UPDATED => Some("Updated"),
        FIELD_URL => Some("Link"),
        _ => None,
    }
}

// ── Wire → display ────────────────────────────────────────────────────────────

/// Build the display merge request from a wire one. Approval and CI fields
/// stay `None` until enrichment fills them in.
pub fn to_display(mr: ApiMergeRequest) -> MergeRequest {
    let project_path = mr
        .references
        .as_ref()
        .and_then(|r| r.full.as_deref())
        .and_then(|full| full.split_once('!'))
        .map(|(path, _)| path.to_owned());
    // Prefer the human-readable path; the numeric id keeps the key unique when
    // `references` is missing.
    let key = project_path.as_ref().map_or_else(
        || format!("MR:{}!{}", mr.project_id, mr.iid),
        |path| format!("MR:{path}!{}", mr.iid),
    );

    let draft = mr.draft.or(mr.work_in_progress).unwrap_or(false);
    let author = mr.author.as_ref().map(|u| u.display().to_owned());
    let assignees: Vec<String> = mr
        .assignees
        .iter()
        .map(|u| u.display().to_owned())
        .collect();
    let reviewers: Vec<String> = mr
        .reviewers
        .iter()
        .map(|u| u.display().to_owned())
        .collect();

    let mut out = MergeRequest {
        key,
        project_id: mr.project_id,
        iid: mr.iid,
        project_path,
        title: mr.title,
        state: mr.state,
        status_label: String::new(),
        web_url: mr.web_url,
        draft,
        has_conflicts: mr.has_conflicts.unwrap_or(false),
        source_branch: mr.source_branch,
        target_branch: mr.target_branch,
        author,
        assignees,
        reviewers,
        labels: mr.labels,
        created_at: mr.created_at,
        updated_at: mr.updated_at,
        merged_at: mr.merged_at,
        closed_at: mr.closed_at,
        user_notes_count: mr.user_notes_count.unwrap_or(0),
        threads_resolved: mr.blocking_discussions_resolved,
        approvals_required: None,
        approvals_left: None,
        approved_by: Vec::new(),
        ci_status: None,
        source_id: None,
        extra: HashMap::new(),
    };
    out.status_label = status_digest(&out);
    out.extra = base_fields(mr.description.as_deref(), &out);
    out
}

/// The `gl.*` field map for everything known before enrichment.
fn base_fields(description: Option<&str>, mr: &MergeRequest) -> HashMap<String, serde_json::Value> {
    let mut extra: HashMap<String, serde_json::Value> = HashMap::new();
    extra.insert(FIELD_STATE.into(), mr.status_label.clone().into());
    extra.insert(FIELD_URL.into(), mr.web_url.clone().into());
    if let Some(desc) = description.filter(|d| !d.trim().is_empty()) {
        extra.insert(FIELD_DESCRIPTION.into(), desc.replace('\r', "").into());
    }
    if let Some(path) = &mr.project_path {
        extra.insert(FIELD_PROJECT.into(), path.clone().into());
    }
    if let Some(author) = &mr.author {
        extra.insert(FIELD_AUTHOR.into(), author.clone().into());
    }
    if !mr.reviewers.is_empty() {
        extra.insert(FIELD_REVIEWERS.into(), mr.reviewers.join(", ").into());
    }
    if !mr.labels.is_empty() {
        extra.insert(FIELD_LABELS.into(), mr.labels.join(", ").into());
    }
    if let (Some(source), Some(target)) = (&mr.source_branch, &mr.target_branch) {
        extra.insert(FIELD_BRANCHES.into(), format!("{source} → {target}").into());
    }
    if let Some(updated) = mr.updated_at {
        extra.insert(
            FIELD_UPDATED.into(),
            updated.format("%Y-%m-%d %H:%M").to_string().into(),
        );
    }
    extra.insert(FIELD_THREADS.into(), threads_summary(mr).into());
    extra
}

/// Refresh the fields that depend on enriched state. Called after approval and
/// pipeline lookups land so the views see the final values.
pub fn apply_enriched_fields(mr: &mut MergeRequest) {
    mr.status_label = status_digest(mr);
    let approvals = mr.approvals_summary();
    mr.extra
        .insert(FIELD_STATE.into(), mr.status_label.clone().into());
    mr.extra.insert(FIELD_APPROVALS.into(), approvals.into());
    if let Some(ci) = &mr.ci_status {
        mr.extra.insert(FIELD_CI.into(), ci.clone().into());
    }
}

fn threads_summary(mr: &MergeRequest) -> String {
    match mr.threads_resolved {
        Some(false) => format!("{} (unresolved)", mr.user_notes_count),
        _ => mr.user_notes_count.to_string(),
    }
}

/// The ≤16-char right-hand column of a merge-request list row.
///
/// Precedence is "what stops this from merging, most blocking first": draft →
/// a terminal state → conflicts → failed CI → approval progress → plain open.
pub fn status_digest(mr: &MergeRequest) -> String {
    if mr.draft {
        return "Draft".to_owned();
    }
    match mr.state.as_str() {
        "merged" => return "Merged".to_owned(),
        "closed" => return "Closed".to_owned(),
        "locked" => return "Locked".to_owned(),
        _ => {}
    }
    if mr.has_conflicts {
        return "Conflicts".to_owned();
    }
    if mr.ci_status.as_deref() == Some("failed") {
        return "CI failed".to_owned();
    }
    let has_rules = mr.approvals_required.is_some_and(|r| r > 0);
    if mr.approvals_left == Some(0) && (has_rules || !mr.approved_by.is_empty()) {
        return "Approved".to_owned();
    }
    match (mr.approvals_required, mr.approvals_left) {
        (Some(required), Some(left)) if required > 0 => {
            format!("Appr {}/{}", required.saturating_sub(left), required)
        }
        _ => "Open".to_owned(),
    }
}

// ── Query building ────────────────────────────────────────────────────────────

/// Map per-source filters to merge-request list query parameters.
///
/// `me` is the token's own username, used when the filter names no explicit
/// one. `scope=all` is always sent: the instance-wide endpoint otherwise
/// defaults to `created_by_me`.
pub fn build_mr_query(filters: &GitlabFilters, me: Option<&str>) -> Vec<(String, String)> {
    let mut q: Vec<(String, String)> = vec![
        ("scope".into(), "all".into()),
        ("state".into(), filters.state.as_str().into()),
        ("order_by".into(), filters.order_by.as_str().into()),
        ("sort".into(), filters.sort.as_str().into()),
    ];

    let role_param = match filters.role {
        GitlabRole::Reviewer => Some("reviewer_username"),
        GitlabRole::Assignee => Some("assignee_username"),
        GitlabRole::Author => Some("author_username"),
        GitlabRole::Any => None,
    };
    if let Some(param) = role_param
        && let Some(username) = filters.username.as_deref().or(me)
    {
        q.push((param.into(), username.into()));
    }

    if !filters.labels.is_empty() {
        q.push(("labels".into(), filters.labels.join(",")));
    }

    match filters.draft {
        DraftFilter::Include => {}
        DraftFilter::Exclude => q.push(("draft".into(), "false".into())),
        DraftFilter::Only => q.push(("draft".into(), "true".into())),
    }

    q
}

/// Query for the standup's merge-request discovery.
///
/// `updated_after` takes a full ISO-8601 instant, so unlike every other date
/// filter in a standup this one is precise to the second and needs no
/// day-widening. `state=all` is deliberate: a merge request merged inside the
/// window is the most interesting thing a standup can report.
pub fn build_standup_mr_query(me: &str, since: DateTime<Utc>) -> Vec<(String, String)> {
    vec![
        // The unscoped endpoint defaults to `created_by_me`, which would drop
        // merge requests in projects you only contribute to.
        ("scope".into(), "all".into()),
        ("state".into(), "all".into()),
        ("author_username".into(), me.into()),
        // `Z` rather than `+00:00`: this is the form GitLab documents, and the
        // offset form has to survive query-string escaping of `+`.
        (
            "updated_after".into(),
            since.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        ),
        ("order_by".into(), "updated_at".into()),
        ("sort".into(), "desc".into()),
    ]
}

/// True when the source needs the token's own username resolved before it can
/// query (a user-scoped role with no explicit `username`).
pub const fn needs_current_user(filters: &GitlabFilters) -> bool {
    !matches!(filters.role, GitlabRole::Any) && filters.username.is_none()
}

/// Percent-encode a namespace path for use as a single URL path segment
/// (`backend/api` → `backend%2Fapi`), which is how GitLab addresses projects
/// and groups by full path.
pub fn encode_path(path: &str) -> String {
    use std::fmt::Write;

    let mut out = String::with_capacity(path.len());
    for byte in path.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            // Writing to a String is infallible.
            other => {
                let _ = write!(out, "%{other:02X}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::{GitlabOrderBy, GitlabState, SortDirection};

    fn q(query: &[(String, String)], key: &str) -> Vec<String> {
        query
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .collect()
    }

    #[test]
    fn default_filters_mean_my_open_mrs_as_reviewer() {
        let query = build_mr_query(&GitlabFilters::default(), Some("me-handle"));
        assert_eq!(q(&query, "reviewer_username"), vec!["me-handle"]);
        assert_eq!(q(&query, "state"), vec!["opened"]);
        assert_eq!(q(&query, "order_by"), vec!["updated_at"]);
        assert_eq!(q(&query, "sort"), vec!["desc"]);
        // Always sent: the unscoped endpoint defaults to created_by_me.
        assert_eq!(q(&query, "scope"), vec!["all"]);
        assert!(q(&query, "draft").is_empty());
        assert!(q(&query, "labels").is_empty());
        assert!(needs_current_user(&GitlabFilters::default()));
    }

    #[test]
    fn roles_pick_the_matching_user_param() {
        for (role, param) in [
            (GitlabRole::Reviewer, "reviewer_username"),
            (GitlabRole::Assignee, "assignee_username"),
            (GitlabRole::Author, "author_username"),
        ] {
            let filters = GitlabFilters {
                role,
                ..Default::default()
            };
            let query = build_mr_query(&filters, Some("someone"));
            assert_eq!(q(&query, param), vec!["someone"], "role: {role:?}");
        }
    }

    #[test]
    fn any_role_omits_every_user_param() {
        let filters = GitlabFilters {
            role: GitlabRole::Any,
            username: Some("ignored".into()),
            ..Default::default()
        };
        let query = build_mr_query(&filters, Some("me-handle"));
        for param in ["reviewer_username", "assignee_username", "author_username"] {
            assert!(q(&query, param).is_empty(), "{param} must be absent");
        }
        assert!(!needs_current_user(&filters));
    }

    #[test]
    fn explicit_username_wins_over_the_token_user() {
        let filters = GitlabFilters {
            username: Some("someone-else".into()),
            ..Default::default()
        };
        let query = build_mr_query(&filters, Some("me-handle"));
        assert_eq!(q(&query, "reviewer_username"), vec!["someone-else"]);
        assert!(!needs_current_user(&filters));
    }

    #[test]
    fn draft_filter_maps_to_the_boolean_param() {
        for (draft, expected) in [
            (DraftFilter::Include, vec![]),
            (DraftFilter::Exclude, vec!["false"]),
            (DraftFilter::Only, vec!["true"]),
        ] {
            let filters = GitlabFilters {
                draft,
                ..Default::default()
            };
            let query = build_mr_query(&filters, Some("me"));
            assert_eq!(q(&query, "draft"), expected, "draft: {draft:?}");
        }
    }

    #[test]
    fn labels_are_comma_joined_and_options_pass_through() {
        let filters = GitlabFilters {
            labels: vec!["needs-review".into(), "backend".into()],
            state: GitlabState::All,
            order_by: GitlabOrderBy::Title,
            sort: SortDirection::Asc,
            ..Default::default()
        };
        let query = build_mr_query(&filters, Some("me"));
        assert_eq!(q(&query, "labels"), vec!["needs-review,backend"]);
        assert_eq!(q(&query, "state"), vec!["all"]);
        assert_eq!(q(&query, "order_by"), vec!["title"]);
        assert_eq!(q(&query, "sort"), vec!["asc"]);
    }

    const LIST_PAYLOAD: &str = r#"{
        "id": 9001,
        "iid": 12,
        "project_id": 42,
        "title": "Add retry to the ingest worker",
        "description": "Fixes the flaky pipeline.\r\nSee the runbook.",
        "state": "opened",
        "web_url": "https://gitlab.example.com/backend/api/-/merge_requests/12",
        "source_branch": "fix/ingest-retry",
        "target_branch": "main",
        "draft": false,
        "author": { "id": 5, "username": "hedy", "name": "Hedy L" },
        "assignees": [{ "id": 6, "username": "ada", "name": "Ada L" }],
        "reviewers": [{ "id": 7, "username": "grace" }],
        "labels": ["needs-review", "backend"],
        "created_at": "2026-07-01T10:00:00.000Z",
        "updated_at": "2026-07-20T12:30:00.000Z",
        "has_conflicts": false,
        "user_notes_count": 3,
        "blocking_discussions_resolved": false,
        "references": { "short": "!12", "full": "backend/api!12" }
    }"#;

    #[test]
    fn to_display_maps_a_realistic_list_payload() {
        let api: ApiMergeRequest = serde_json::from_str(LIST_PAYLOAD).expect("payload parses");
        let mr = to_display(api);

        assert_eq!(mr.key, "MR:backend/api!12");
        assert_eq!(mr.project_path.as_deref(), Some("backend/api"));
        assert_eq!(mr.project_id, 42);
        assert_eq!(mr.iid, 12);
        assert_eq!(mr.short_ref(), "!12");
        assert_eq!(mr.title, "Add retry to the ingest worker");
        assert_eq!(mr.author.as_deref(), Some("Hedy L"));
        // No `name` on the wire → the handle is the display name.
        assert_eq!(mr.reviewers, vec!["grace"]);
        assert_eq!(mr.assignees, vec!["Ada L"]);
        assert_eq!(mr.user_notes_count, 3);
        assert_eq!(mr.threads_resolved, Some(false));
        assert!(!mr.draft);

        // Field map: pre-enrichment values only.
        let field = |id: &str| mr.extra.get(id).and_then(|v| v.as_str()).map(str::to_owned);
        assert_eq!(field(FIELD_PROJECT).as_deref(), Some("backend/api"));
        assert_eq!(field(FIELD_AUTHOR).as_deref(), Some("Hedy L"));
        assert_eq!(field(FIELD_REVIEWERS).as_deref(), Some("grace"));
        assert_eq!(
            field(FIELD_LABELS).as_deref(),
            Some("needs-review, backend")
        );
        assert_eq!(
            field(FIELD_BRANCHES).as_deref(),
            Some("fix/ingest-retry → main")
        );
        assert_eq!(field(FIELD_UPDATED).as_deref(), Some("2026-07-20 12:30"));
        assert_eq!(field(FIELD_THREADS).as_deref(), Some("3 (unresolved)"));
        assert_eq!(field(FIELD_STATE).as_deref(), Some("Open"));
        assert_eq!(mr.status_label, "Open", "the field mirrors the digest");
        assert_eq!(
            field(FIELD_URL).as_deref(),
            Some("https://gitlab.example.com/backend/api/-/merge_requests/12")
        );
        // Carriage returns are stripped so the description renders cleanly.
        assert_eq!(
            field(FIELD_DESCRIPTION).as_deref(),
            Some("Fixes the flaky pipeline.\nSee the runbook.")
        );
        // Enrichment hasn't run.
        assert!(mr.extra.get(FIELD_APPROVALS).is_none());
        assert!(mr.extra.get(FIELD_CI).is_none());
    }

    #[test]
    fn minimal_payload_falls_back_to_numeric_ids_and_legacy_draft_flag() {
        let raw = r#"{
            "iid": 7,
            "project_id": 42,
            "title": "WIP thing",
            "state": "opened",
            "web_url": "https://gitlab.com/x/y/-/merge_requests/7",
            "work_in_progress": true
        }"#;
        let mr = to_display(serde_json::from_str(raw).expect("payload parses"));
        // No `references` → the numeric project id keeps the key unique.
        assert_eq!(mr.key, "MR:42!7");
        assert!(mr.project_path.is_none());
        // Older instances only send `work_in_progress`.
        assert!(mr.draft);
        assert!(!mr.has_conflicts, "absent has_conflicts is not a conflict");
        assert_eq!(mr.user_notes_count, 0);
        assert!(mr.extra.get(FIELD_DESCRIPTION).is_none());
        assert!(mr.extra.get(FIELD_BRANCHES).is_none());
    }

    #[test]
    fn keys_cannot_collide_with_jira_or_confluence_keys() {
        let mr = to_display(serde_json::from_str(LIST_PAYLOAD).expect("payload parses"));
        assert!(mr.key.starts_with("MR:"));
        // A Jira key is `[A-Z]+-[0-9]+`; a Confluence one is `CONF:{id}`.
        assert_ne!(mr.key, "PROJ-1");
        assert_ne!(mr.key, "CONF:1");
        assert!(!mr.key.starts_with("CONF:"));
        assert!(
            !mr.key.contains('-') || mr.key.contains('/'),
            "the MR: prefix keeps it out of Jira key shape"
        );
    }

    fn open_mr() -> MergeRequest {
        to_display(serde_json::from_str(LIST_PAYLOAD).expect("payload parses"))
    }

    #[test]
    fn status_digest_precedence_table() {
        // Plain open MR, no approval rules.
        assert_eq!(status_digest(&open_mr()), "Open");

        // Approval progress, then fully approved.
        let mut mr = open_mr();
        mr.approvals_required = Some(2);
        mr.approvals_left = Some(1);
        assert_eq!(status_digest(&mr), "Appr 1/2");
        mr.approvals_left = Some(0);
        assert_eq!(status_digest(&mr), "Approved");

        // Failed CI outranks approval state.
        mr.ci_status = Some("failed".into());
        assert_eq!(status_digest(&mr), "CI failed");
        // A passing pipeline doesn't hide approval state.
        mr.ci_status = Some("success".into());
        assert_eq!(status_digest(&mr), "Approved");
        mr.ci_status = Some("failed".into());

        // Conflicts outrank CI.
        mr.has_conflicts = true;
        assert_eq!(status_digest(&mr), "Conflicts");

        // A terminal state outranks conflicts.
        for (state, expected) in [
            ("merged", "Merged"),
            ("closed", "Closed"),
            ("locked", "Locked"),
        ] {
            mr.state = state.into();
            assert_eq!(status_digest(&mr), expected);
        }

        // Draft outranks everything.
        mr.draft = true;
        assert_eq!(status_digest(&mr), "Draft");

        // Every digest fits the 16-char column.
        for digest in [
            "Draft",
            "Merged",
            "Closed",
            "Locked",
            "Conflicts",
            "CI failed",
            "Approved",
            "Appr 1/2",
            "Open",
        ] {
            assert!(digest.chars().count() <= 16, "{digest} is too wide");
        }
    }

    #[test]
    fn approvals_with_no_rules_but_an_approver_reads_as_approved() {
        let mut mr = open_mr();
        mr.approvals_required = Some(0);
        mr.approvals_left = Some(0);
        // No rules and nobody approved → nothing to report.
        assert_eq!(status_digest(&mr), "Open");
        mr.approved_by = vec!["Ada L".into()];
        assert_eq!(status_digest(&mr), "Approved");
    }

    #[test]
    fn apply_enriched_fields_refreshes_state_approvals_and_ci() {
        let mut mr = open_mr();
        mr.approvals_required = Some(2);
        mr.approvals_left = Some(1);
        mr.approved_by = vec!["Ada L".into()];
        mr.ci_status = Some("running".into());
        apply_enriched_fields(&mut mr);

        let field = |id: &str| mr.extra.get(id).and_then(|v| v.as_str()).map(str::to_owned);
        assert_eq!(mr.status_label, "Appr 1/2");
        assert_eq!(field(FIELD_STATE).as_deref(), Some("Appr 1/2"));
        assert_eq!(field(FIELD_APPROVALS).as_deref(), Some("1/2 (Ada L)"));
        assert_eq!(field(FIELD_CI).as_deref(), Some("running"));
    }

    #[test]
    fn list_label_modes() {
        let mr = open_mr();
        assert_eq!(
            mr.list_label(GitlabLabel::Title),
            "Add retry to the ingest worker"
        );
        assert_eq!(mr.list_label(GitlabLabel::Project), "backend/api");
        assert_eq!(
            mr.list_label(GitlabLabel::Both),
            "Add retry to the ingest worker · backend/api"
        );

        // No project path resolved → all modes fall back to the title.
        let mut no_path = mr;
        no_path.project_path = None;
        assert_eq!(no_path.list_label(GitlabLabel::Project), no_path.title);
        assert_eq!(no_path.list_label(GitlabLabel::Both), no_path.title);
    }

    #[test]
    fn namespace_paths_are_percent_encoded_as_one_segment() {
        assert_eq!(encode_path("backend/api"), "backend%2Fapi");
        assert_eq!(encode_path("acme/backend/api"), "acme%2Fbackend%2Fapi");
        // Unreserved characters pass through untouched.
        assert_eq!(encode_path("my-group_1.x~y"), "my-group_1.x~y");
        assert_eq!(encode_path("a b"), "a%20b");
    }

    #[test]
    fn approvals_and_pipeline_payloads_parse() {
        let approvals: ApiApprovals = serde_json::from_str(
            r#"{ "approvals_required": 2, "approvals_left": 1,
                 "approved_by": [{ "user": { "id": 6, "username": "ada", "name": "Ada L" } }] }"#,
        )
        .expect("approvals parse");
        assert_eq!(approvals.approvals_required, Some(2));
        assert_eq!(approvals.approvals_left, Some(1));
        assert_eq!(approvals.approved_by[0].user.display(), "Ada L");

        let detail: ApiMergeRequestDetail =
            serde_json::from_str(r#"{ "head_pipeline": { "status": "success" } }"#)
                .expect("detail parses");
        assert_eq!(
            detail.head_pipeline.and_then(|p| p.status).as_deref(),
            Some("success")
        );

        // An MR with no pipeline at all.
        let detail: ApiMergeRequestDetail =
            serde_json::from_str(r#"{ "head_pipeline": null }"#).expect("detail parses");
        assert!(detail.head_pipeline.is_none());
    }
}
