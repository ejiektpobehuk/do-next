use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Issue {
    pub id: String,
    pub key: String,
    pub fields: IssueFields,
    /// Which source this issue was fetched from (set after fetch). Absent from
    /// Jira API responses (`default`); persisted in the on-disk source cache.
    #[serde(default)]
    pub source_id: Option<String>,
    /// Within-source subsource index for ordering (set after fetch). Absent
    /// from Jira API responses (`default`); persisted in the source cache.
    #[serde(default)]
    pub subsource_idx: usize,
    /// True when only board-display fields were fetched, so the issue's full
    /// detail (description, comments, custom fields) must be lazy-loaded when
    /// it is opened. Set after a trimmed board fetch; false everywhere else.
    /// Absent from Jira API responses (`default`); persisted in the cache.
    #[serde(default)]
    pub partial: bool,
    /// Present only when the fetch asked for `expand=changelog` (the standup
    /// collector). Skipped when serializing so the source cache does not grow a
    /// copy of every issue's history.
    #[serde(default, skip_serializing)]
    pub changelog: Option<Changelog>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IssueFields {
    pub summary: String,
    pub status: StatusField,
    pub priority: Option<PriorityField>,
    pub assignee: Option<UserField>,
    pub reporter: Option<UserField>,
    pub issuetype: IssueTypeField,
    pub project: ProjectField,
    pub description: Option<serde_json::Value>,
    pub comment: Option<CommentList>,
    pub attachment: Option<Vec<Attachment>>,
    /// All custom fields, keyed by field ID.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StatusField {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PriorityField {
    pub id: String,
    pub name: String,
}

impl PriorityField {
    /// Single-char symbol for the priority level.
    pub fn symbol(&self) -> &'static str {
        match self.name.to_lowercase().as_str() {
            "highest" | "blocker" => "↑",
            "high" | "critical" => "↗",
            "medium" | "normal" => "→",
            "low" | "minor" => "↘",
            "lowest" | "trivial" => "↓",
            _ => "·",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UserField {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(rename = "displayName")]
    pub display_name: Option<String>,
    #[serde(rename = "accountId")]
    pub account_id: Option<String>,
}

impl UserField {
    pub fn display(&self) -> &str {
        self.display_name
            .as_deref()
            .or(self.name.as_deref())
            .or(self.account_id.as_deref())
            .unwrap_or("Unknown")
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IssueTypeField {
    pub id: String,
    pub name: String,
    /// Whether this type is a sub-task. Its `parent` is a standard issue, not
    /// an epic, so the create form must not offer the epic picker for it.
    /// Absent from most payloads we deserialize; defaults to a standard type.
    #[serde(default)]
    pub subtask: bool,
}

/// An issue as the create form's pickers need it: the key is what goes into the
/// payload, the summary is what identifies it on screen. Backs both the epic
/// chooser and the linked-issue chooser — an epic is just an issue there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueRef {
    pub key: String,
    pub summary: String,
}

impl IssueRef {
    /// Key first: it is short, unique, and what the payload carries.
    pub fn display(&self) -> String {
        if self.summary.is_empty() {
            return self.key.clone();
        }
        format!("{}  {}", self.key, self.summary)
    }
}

/// One of the site's issue link types, as `/issueLinkType` returns it. `inward`
/// and `outward` are the two human-readable halves of the relation ("is blocked
/// by" / "blocks"); `name` is what a create payload references.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct IssueLinkType {
    pub name: String,
    pub inward: String,
    pub outward: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProjectField {
    pub id: String,
    pub key: String,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CommentList {
    pub comments: Vec<Comment>,
    pub total: u32,
    /// How many of `total` this payload carries. Needed because the `comment`
    /// block inside a search response starts at the *oldest* comment, so on a
    /// long-running issue the recent ones are exactly the truncated ones — the
    /// standup collector compares these to decide whether to page properly.
    /// `default` because the on-disk source cache holds payloads written before
    /// these fields were read.
    #[serde(rename = "maxResults", default)]
    pub max_results: u32,
    #[serde(rename = "startAt", default)]
    pub start_at: u32,
}

impl CommentList {
    /// True when this payload is missing comments, so the dedicated endpoint
    /// must be paged to see recent ones.
    pub const fn is_truncated(&self) -> bool {
        self.total > self.max_results && self.max_results > 0
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Comment {
    pub id: String,
    pub author: UserField,
    /// Who last edited it. Lets "comments I edited" be distinguished from
    /// "comments I wrote"; absent on older cached payloads.
    #[serde(rename = "updateAuthor", default)]
    pub update_author: Option<UserField>,
    pub body: serde_json::Value,
    pub created: String,
    pub updated: String,
}

/// Envelope of `GET /rest/api/3/issue/{key}/comment`.
#[derive(Debug, Deserialize)]
pub struct CommentPage {
    pub comments: Vec<Comment>,
}

/// One changegroup: a set of field changes made by one person at one instant.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChangelogEntry {
    pub id: String,
    #[serde(default)]
    pub author: Option<UserField>,
    pub created: String,
    #[serde(default)]
    pub items: Vec<ChangelogItem>,
}

/// One field's before/after inside a changegroup.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChangelogItem {
    /// Human field name, e.g. "status", "description", "Story Points".
    pub field: String,
    /// Stable field id when Jira supplies one; absent for some synthetic rows.
    #[serde(rename = "fieldId", default)]
    pub field_id: Option<String>,
    #[serde(rename = "fromString", default)]
    pub from_string: Option<String>,
    #[serde(rename = "toString", default)]
    pub to_string_value: Option<String>,
}

/// The `changelog` object returned by `expand=changelog`, newest changegroup
/// first — the opposite order to `GET /issue/{key}/changelog`, which is why the
/// inline form is the one the standup collector wants.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Changelog {
    #[serde(default)]
    pub histories: Vec<ChangelogEntry>,
    #[serde(rename = "maxResults", default)]
    pub max_results: u32,
    #[serde(rename = "startAt", default)]
    pub start_at: u32,
    #[serde(default)]
    pub total: u32,
}

impl Changelog {
    /// True when the issue has more changegroups than this payload carries, so
    /// the standalone (oldest-first) endpoint must be asked for the tail.
    pub const fn is_truncated(&self) -> bool {
        self.total > self.max_results && self.max_results > 0
    }
}

/// Envelope of `GET /rest/api/3/issue/{key}/changelog` (oldest first).
#[derive(Debug, Deserialize)]
pub struct ChangelogPage {
    #[serde(default)]
    pub values: Vec<ChangelogEntry>,
}

/// One logged work entry.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Worklog {
    pub id: String,
    #[serde(default)]
    pub author: Option<UserField>,
    /// When the work happened — what `worklogDate` filters on, and the day an
    /// entry is placed under. Distinct from `created`, when it was typed in.
    pub started: String,
    #[serde(default)]
    pub created: Option<String>,
    #[serde(rename = "timeSpentSeconds", default)]
    pub time_spent_seconds: i64,
    #[serde(default)]
    pub comment: Option<serde_json::Value>,
}

/// Envelope of `GET /rest/api/3/issue/{key}/worklog`.
#[derive(Debug, Deserialize)]
pub struct WorklogPage {
    #[serde(default)]
    pub worklogs: Vec<Worklog>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Attachment {
    pub id: String,
    pub filename: String,
    pub author: UserField,
    pub created: String,
    pub size: Option<u64>,
    #[serde(rename = "mimeType")]
    pub mime_type: Option<String>,
    pub content: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Transition {
    pub id: String,
    pub name: String,
    pub to: StatusField,
}

/// Envelope of `GET /rest/api/3/search/jql` (enhanced JQL search). The
/// endpoint is cursor-based: `nextPageToken` requests the next page and is
/// omitted on the last one; `startAt` is silently ignored.
#[derive(Debug, Deserialize)]
pub struct SearchResponse {
    pub issues: Vec<Issue>,
    #[serde(rename = "isLast", default)]
    pub is_last: bool,
    #[serde(rename = "nextPageToken", default)]
    pub next_page_token: Option<String>,
}

/// Envelope for searches that request `fields=key`.
///
/// Cannot reuse [`SearchResponse`]: with `fields=key` Jira omits the `fields`
/// object entirely, and `Issue::fields` is mandatory, so deserializing into it
/// fails with "Failed to parse search response" — which reads as an empty result
/// to any caller that treats an error as "nothing found".
#[derive(Debug, Deserialize)]
pub struct KeysResponse {
    pub issues: Vec<IssueKey>,
    #[serde(rename = "isLast", default)]
    pub is_last: bool,
    #[serde(rename = "nextPageToken", default)]
    pub next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct IssueKey {
    pub key: String,
}

/// Jira REST API transitions response envelope.
#[derive(Debug, Deserialize)]
pub struct TransitionsResponse {
    pub transitions: Vec<Transition>,
}

/// Agile board type from the configuration response.
/// Team-managed (next-gen) boards report "simple" regardless of sprint
/// support, so this alone cannot decide whether a board has sprints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BoardType {
    Scrum,
    Kanban,
    Simple,
    #[serde(other)]
    Unknown,
}

impl BoardType {
    /// The type as Jira spells it, for report lines.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Scrum => "scrum",
            Self::Kanban => "kanban",
            Self::Simple => "simple",
            Self::Unknown => "unknown type",
        }
    }
}

/// Agile board configuration (`GET /rest/agile/1.0/board/{id}/configuration`).
/// Carries the ordered columns and the board type — one call serves both.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BoardConfiguration {
    pub id: u64,
    pub name: String,
    #[serde(rename = "type")]
    pub board_type: BoardType,
    #[serde(rename = "columnConfig")]
    pub column_config: ColumnConfig,
    /// The board's rank field, needed by rank mutations when an instance has
    /// more than one Rank field. Defaulted so pre-existing cache files (and
    /// odd responses) keep deserializing.
    #[serde(default)]
    pub ranking: Option<RankingConfig>,
}

/// The `ranking` block of a board configuration. Boards not ordered by the
/// Rank field return an empty object here, so the field id is optional.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RankingConfig {
    #[serde(rename = "rankCustomFieldId", default)]
    pub rank_custom_field_id: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ColumnConfig {
    pub columns: Vec<BoardColumn>,
}

/// One board column, in board order. A column maps to one or more statuses;
/// `statuses` may be empty for columns with no mapped status.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BoardColumn {
    pub name: String,
    #[serde(default)]
    pub statuses: Vec<ColumnStatus>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ColumnStatus {
    /// Status id; joins `Issue.fields.status.id`.
    pub id: String,
}

impl BoardColumn {
    pub fn contains_status(&self, status_id: &str) -> bool {
        self.statuses.iter().any(|s| s.id == status_id)
    }
}

/// Where a rank mutation places the issues: directly before or directly
/// after the anchor issue (`PUT /rest/agile/1.0/issue/rank`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RankAnchor {
    Before(String),
    After(String),
}

/// Agile sprint (`GET /rest/agile/1.0/board/{id}/sprint`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Sprint {
    pub id: u64,
    pub name: String,
    /// "active" | "future" | "closed"
    pub state: String,
}

/// Agile values-list page envelope (isLast-based; used by the sprint list).
#[derive(Debug, Deserialize)]
pub struct AgilePage<T> {
    pub values: Vec<T>,
    #[serde(rename = "isLast", default)]
    pub is_last: bool,
}

/// Agile issue-list envelope. Unlike the v3 `SearchResponse`, Agile issue
/// endpoints return `total` and no `isLast` — pagination goes by total.
#[derive(Debug, Deserialize)]
pub struct AgileIssuesResponse {
    pub issues: Vec<Issue>,
    pub total: u32,
}

/// Resolved query-swimlane assignment for one board source, computed at
/// fetch time. Keys absent from `assignment` belong to the trailing
/// "everything else" lane when the config enables it.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BoardSwimlanes {
    /// Lane names in display order.
    pub lane_names: Vec<String>,
    /// Issue key → index into `lane_names`.
    pub assignment: std::collections::HashMap<String, usize>,
}

/// Lane definition from Jira's internal `GreenHopper` API (the only place
/// swimlane config exists — it is absent from the public Agile API).
/// Deliberately tolerant: only the fields we need, everything defaulted,
/// unknown fields ignored, because the endpoint is undocumented.
#[derive(Debug, Clone, Deserialize)]
pub struct GreenHopperSwimlane {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub query: String,
    #[serde(default, rename = "isDefault")]
    pub is_default: bool,
}

/// Envelope for the `GreenHopper` board payload; we extract only the swimlane
/// list. Tolerant of both observed layouts (`swimlanes` at the top level or
/// nested under `swimlanesData`) since the endpoint is undocumented.
#[derive(Debug, Deserialize)]
pub struct GreenHopperBoardData {
    #[serde(default)]
    pub swimlanes: Vec<GreenHopperSwimlane>,
    #[serde(default, rename = "swimlanesData")]
    pub swimlanes_data: Option<GreenHopperSwimlanesData>,
}

#[derive(Debug, Deserialize)]
pub struct GreenHopperSwimlanesData {
    #[serde(default)]
    pub swimlanes: Vec<GreenHopperSwimlane>,
}

impl GreenHopperBoardData {
    /// The swimlane list, wherever the payload put it.
    pub fn into_swimlanes(self) -> Vec<GreenHopperSwimlane> {
        if self.swimlanes.is_empty() {
            self.swimlanes_data.map(|d| d.swimlanes).unwrap_or_default()
        } else {
            self.swimlanes
        }
    }
}

/// Metadata for a single Jira field (from `/rest/api/3/field`).
#[derive(Debug, Deserialize)]
pub struct FieldMeta {
    pub id: String,
    pub name: String,
}

/// Light project descriptor used by the search filter picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectInfo {
    pub key: String,
    pub name: String,
}

/// Jira status category. Mirrors the four `statusCategory.key` values returned
/// by the Jira REST API: `new`, `indeterminate`, `done`, `undefined`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StatusCategory {
    New,
    Indeterminate,
    Done,
    Undefined,
}

impl StatusCategory {
    pub fn from_key(key: &str) -> Self {
        match key {
            "new" => Self::New,
            "indeterminate" => Self::Indeterminate,
            "done" => Self::Done,
            _ => Self::Undefined,
        }
    }

    /// The `statusCategory.key` this variant came from — what JQL's
    /// `statusCategory` compares against, so it is also what a config author
    /// needs to see next to a status name.
    pub const fn key(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Indeterminate => "indeterminate",
            Self::Done => "done",
            Self::Undefined => "undefined",
        }
    }
}

/// A Jira status with the category it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusInfo {
    pub name: String,
    pub category: StatusCategory,
}

/// A status as Jira defines it site-wide, id included.
///
/// The id is the part [`StatusInfo`] has no room for and a board cannot do
/// without: a board's column configuration carries only status ids (see
/// [`ColumnStatus`]), so naming its columns means joining against this list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusDetail {
    pub id: String,
    pub name: String,
    pub category: StatusCategory,
}

/// A selectable option for a Jira select/array field (from editmeta `allowedValues`).
#[derive(Debug, Clone)]
pub struct FieldOption {
    pub value: String,
}

/// Schema info for a Jira field, captured from editmeta `schema`.
///
/// `ty` alone isn't enough to detect rich-text fields: Jira returns
/// `schema.type = "string"` for both plain-text and ADF fields. Whether the
/// field expects ADF is encoded in `schema.system` (e.g. `"description"`,
/// `"environment"`) or `schema.custom` (e.g. the multi-line text customfield
/// type).
#[derive(Debug, Clone, Default)]
pub struct FieldSchema {
    pub ty: String,
    pub custom: Option<String>,
    pub system: Option<String>,
}

impl FieldSchema {
    /// Returns true when this field's value is an Atlassian Document.
    pub fn is_adf(&self) -> bool {
        matches!(self.system.as_deref(), Some("description" | "environment"))
            || self.custom.as_deref().is_some_and(is_adf_custom_field_type)
    }

    /// Returns true when this field holds the issue's epic. Jira spells that
    /// two ways: the system `parent` link on team-managed projects (and on
    /// Cloud company-managed ones since the Epic Link retirement), and the
    /// older Greenhopper "Epic Link" custom field everywhere else.
    ///
    /// For a sub-task `parent` is a standard issue rather than an epic; that
    /// distinction lives with the issue type, not the schema, so callers pass
    /// it in separately.
    pub fn is_epic_link(&self) -> bool {
        self.custom.as_deref() == Some(EPIC_LINK_CUSTOM_FIELD_TYPE)
            || (self.ty == "issuelink" && self.system.as_deref() == Some("parent"))
    }

    /// Whether this is the system `parent` field rather than the legacy custom
    /// one — they take different payload shapes.
    pub fn is_system_parent(&self) -> bool {
        self.system.as_deref() == Some("parent")
    }

    /// Whether this field holds labels: the system `labels` field, or a custom
    /// field of the same type. Both are arrays of bare strings, which is what
    /// separates them from the other `array` fields — a label is typed, not
    /// chosen from `allowedValues`.
    pub fn is_labels(&self) -> bool {
        self.system.as_deref() == Some("labels")
            || self.custom.as_deref() == Some(LABELS_CUSTOM_FIELD_TYPE)
    }
}

/// Greenhopper's "Epic Link" custom-field type key.
const EPIC_LINK_CUSTOM_FIELD_TYPE: &str = "com.pyxis.greenhopper.jira:gh-epic-link";

/// The "Labels" custom-field type key.
const LABELS_CUSTOM_FIELD_TYPE: &str = "com.atlassian.jira.plugin.system.customfieldtypes:labels";

/// Custom-field type keys whose values are ADF documents.
///
/// The classic "Paragraph (supports rich text)" custom field is `:textarea`.
/// Sourced from Jira's customfieldtypes plugin keys.
fn is_adf_custom_field_type(custom: &str) -> bool {
    matches!(
        custom,
        "com.atlassian.jira.plugin.system.customfieldtypes:textarea"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed capture of `GET /rest/agile/1.0/board/{id}/configuration`,
    /// keeping the envelope fields we ignore to prove tolerance.
    const BOARD_CONFIG_JSON: &str = r#"{
        "id": 10000,
        "name": "Team board",
        "type": "kanban",
        "self": "https://acme.atlassian.net/rest/agile/1.0/board/10000/configuration",
        "location": { "type": "project", "key": "PROJ" },
        "filter": { "id": "10001" },
        "columnConfig": {
            "columns": [
                { "name": "Backlog", "statuses": [] },
                { "name": "To Do", "statuses": [{ "id": "10100", "self": "..." }] },
                { "name": "In Progress", "statuses": [{ "id": "3", "self": "..." }, { "id": "10101", "self": "..." }] },
                { "name": "Done", "statuses": [{ "id": "10200", "self": "..." }] }
            ],
            "constraintType": "issueCount"
        },
        "ranking": { "rankCustomFieldId": 10019 }
    }"#;

    #[test]
    fn board_configuration_deserializes_from_api_json() {
        let cfg: BoardConfiguration = serde_json::from_str(BOARD_CONFIG_JSON).unwrap();
        assert_eq!(cfg.id, 10000);
        assert_eq!(cfg.board_type, BoardType::Kanban);
        let cols = &cfg.column_config.columns;
        assert_eq!(
            cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["Backlog", "To Do", "In Progress", "Done"]
        );
        assert!(cols[0].statuses.is_empty());
        assert!(cols[2].contains_status("3"));
        assert!(cols[2].contains_status("10101"));
        assert!(!cols[2].contains_status("10100"));
        assert_eq!(cfg.ranking.unwrap().rank_custom_field_id, Some(10019));
    }

    #[test]
    fn board_configuration_tolerates_empty_ranking() {
        // Boards not ordered by the Rank field return `"ranking": {}`.
        let cfg: BoardConfiguration = serde_json::from_str(
            r#"{
            "id": 1, "name": "b", "type": "kanban",
            "columnConfig": { "columns": [] },
            "ranking": {}
        }"#,
        )
        .unwrap();
        assert!(cfg.ranking.unwrap().rank_custom_field_id.is_none());
    }

    #[test]
    fn board_configuration_tolerates_missing_ranking() {
        // Pre-existing cache files were written without `ranking`.
        let cfg: BoardConfiguration = serde_json::from_str(
            r#"{
            "id": 1, "name": "b", "type": "scrum",
            "columnConfig": { "columns": [] }
        }"#,
        )
        .unwrap();
        assert!(cfg.ranking.is_none());
    }

    #[test]
    fn board_type_covers_simple_and_unknown() {
        for (json, expected) in [
            (r#""scrum""#, BoardType::Scrum),
            (r#""simple""#, BoardType::Simple),
            (r#""next-gen-mystery""#, BoardType::Unknown),
        ] {
            let ty: BoardType = serde_json::from_str(json).unwrap();
            assert_eq!(ty, expected);
        }
    }

    #[test]
    fn search_response_paginates_by_cursor_token() {
        // Mid-stream page: more results exist, cursor present.
        let page: SearchResponse = serde_json::from_str(
            r#"{ "issues": [], "isLast": false, "nextPageToken": "CAEaAggD" }"#,
        )
        .unwrap();
        assert!(!page.is_last);
        assert_eq!(page.next_page_token.as_deref(), Some("CAEaAggD"));

        // Last page omits the token.
        let last: SearchResponse =
            serde_json::from_str(r#"{ "issues": [], "isLast": true }"#).unwrap();
        assert!(last.is_last);
        assert!(last.next_page_token.is_none());
    }

    #[test]
    fn agile_issue_envelope_paginates_by_total_not_is_last() {
        let json = r#"{ "startAt": 0, "maxResults": 50, "total": 120, "issues": [] }"#;
        let page: AgileIssuesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(page.total, 120);
        assert!(page.issues.is_empty());
    }

    #[test]
    fn sprint_page_deserializes() {
        let json = r#"{
            "maxResults": 50, "startAt": 0, "isLast": true,
            "values": [{ "id": 137, "name": "Sprint 12", "state": "active", "goal": "ship" }]
        }"#;
        let page: AgilePage<Sprint> = serde_json::from_str(json).unwrap();
        assert!(page.is_last);
        assert_eq!(page.values[0].id, 137);
        assert_eq!(page.values[0].state, "active");
    }

    #[test]
    fn greenhopper_swimlanes_tolerate_both_layouts_and_junk() {
        // Top-level `swimlanes` (lane definitions with query + default lane).
        let top: GreenHopperBoardData = serde_json::from_str(
            r#"{
                "rapidViewId": 42,
                "swimlanes": [
                    { "id": 1, "name": "Expedite", "query": "priority = Highest", "isDefault": false },
                    { "id": 2, "name": "Everything Else", "query": "", "isDefault": true }
                ]
            }"#,
        )
        .unwrap();
        let lanes = top.into_swimlanes();
        assert_eq!(lanes.len(), 2);
        assert_eq!(lanes[0].query, "priority = Highest");
        assert!(lanes[1].is_default);

        // Nested `swimlanesData.swimlanes` layout.
        let nested: GreenHopperBoardData = serde_json::from_str(
            r#"{ "swimlanesData": { "swimlanes": [{ "name": "Bugs", "query": "type = Bug" }] } }"#,
        )
        .unwrap();
        let lanes = nested.into_swimlanes();
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].name, "Bugs");
        assert!(!lanes[0].is_default);

        // No swimlane data at all → empty, not an error.
        let none: GreenHopperBoardData = serde_json::from_str(r#"{ "otherStuff": 1 }"#).unwrap();
        assert!(none.into_swimlanes().is_empty());
    }
}
