use std::collections::HashMap;

use crate::items::WorkItem;
use crate::jira::types::{
    Attachment, Comment, FieldOption, FieldSchema, Issue, IssueLinkType, IssueRef, IssueTypeField,
    ProjectInfo, StatusField, StatusInfo, Transition, UserField,
};

#[derive(Debug)]
pub enum AppEvent {
    /// Keyboard or mouse event from the terminal.
    Input(crossterm::event::Event),
    /// A background fetch completed successfully.
    SourceLoaded(String, Vec<WorkItem>),
    /// A whole-source fetch failed (no subsources).
    SourceError(String, anyhow::Error),
    /// One subsource fetch failed; other subsources continue.
    SubsourceError(String, usize, anyhow::Error),
    /// A board source's column configuration arrived. Sent before that
    /// source's `SourceLoaded` on the same channel, so ordering is guaranteed.
    BoardConfigLoaded(String, crate::jira::types::BoardConfiguration),
    /// A board source's query-swimlane assignment resolved (sent after
    /// `SourceLoaded`; only for `auto`/query lane strategies). Errors degrade
    /// the board to laneless — they never fail the source.
    BoardLanesLoaded(
        String,
        Result<crate::jira::types::BoardSwimlanes, anyhow::Error>,
    ),
    /// A Jira action (transition, comment, assign, move) completed.
    ActionDone(ActionResult),
    /// Current user resolved (sent once on startup). Carries the whole identity:
    /// the account id addresses them in payloads, the display name shows them.
    CurrentUserResolved(UserField),
    /// Spinner animation frame — only sent while sources are loading.
    Tick,
    /// Filesystem path completions ready (from debounced async fetch).
    PathCompletions {
        generation: u64,
        completions: Vec<String>,
    },
    /// Git-based update warnings for team configs (sent once on startup).
    UpdateWarnings(Vec<String>),
    /// A backlog rank mutation finished. Success rewrites that source's
    /// cache with the already-applied optimistic order; failure refetches
    /// the source so the server's order is truth again.
    IssueRanked {
        source_id: String,
        result: Result<(), anyhow::Error>,
    },
    /// A standup source finished collecting. Sent alongside that source's
    /// `SourceLoaded` (which carries the underlying payloads so the timeline's
    /// Enter can open the normal detail view). Boxed because the payload is much
    /// larger than any other variant.
    StandupLoaded(String, Box<crate::standup::types::StandupData>),
    /// A single-issue background refresh completed successfully.
    IssueRefreshed(Box<WorkItem>),
    /// A single-issue background refresh failed.
    IssueRefreshError {
        issue_key: String,
        error: anyhow::Error,
    },
    /// Debounced Jira-side search returned; carries the `debounce_token` that
    /// was current when the request was spawned. Stale responses (token
    /// mismatch) are dropped by the handler.
    SearchJiraResult {
        token: u64,
        result: Result<Vec<Issue>, anyhow::Error>,
    },
    /// Distinct status names from the team projects' workflows, deduped
    /// across projects. `team_idx` lets the handler discard responses for a
    /// team the user has since left.
    TeamStatusesLoaded {
        team_idx: usize,
        result: Result<Vec<String>, anyhow::Error>,
    },
    /// All statuses configured on this Jira instance, used to populate the
    /// status picker's "Other" section.
    AllStatusesLoaded {
        team_idx: usize,
        result: Result<Vec<StatusInfo>, anyhow::Error>,
    },
    /// Visible Jira projects fetched via `/project/search`.
    AllProjectsLoaded {
        team_idx: usize,
        result: Result<Vec<ProjectInfo>, anyhow::Error>,
    },
    /// Issue types for the create form's selected project. `token` is the
    /// `CreateForm::meta_token` current when the fetch was spawned; stale
    /// responses (project changed since) are dropped.
    CreateIssueTypesLoaded {
        token: u64,
        result: Result<Vec<IssueTypeField>, anyhow::Error>,
    },
    /// Field metadata (raw createmeta descriptors) for the create form's
    /// selected project + issue type. `token` guards against stale responses.
    CreateFieldsLoaded {
        token: u64,
        result: Result<Vec<serde_json::Value>, anyhow::Error>,
    },
    /// Users matching the create form's assignee/reporter picker query.
    /// `token` is the search's own counter, bumped on every query change.
    CreateUsersLoaded {
        token: u64,
        result: Result<Vec<UserField>, anyhow::Error>,
    },
    /// Epics matching the create form's epic-picker query. `token` works the
    /// same way as `CreateUsersLoaded`'s.
    CreateEpicsLoaded {
        token: u64,
        result: Result<Vec<IssueRef>, anyhow::Error>,
    },
    /// The site's issue link types, for the create form's relation chooser.
    /// Untokened: they are site-wide, so no selection can make them stale.
    CreateLinkTypesLoaded {
        result: Result<Vec<IssueLinkType>, anyhow::Error>,
    },
    /// Issues matching the create form's linked-issue query. `token` works the
    /// same way as `CreateUsersLoaded`'s.
    CreateLinkIssuesLoaded {
        token: u64,
        result: Result<Vec<IssueRef>, anyhow::Error>,
    },
    /// The site's labels, for the create form's labels chooser. Untokened like
    /// `CreateLinkTypesLoaded`: they are site-wide, so no selection can make
    /// them stale.
    CreateLabelsLoaded {
        result: Result<Vec<String>, anyhow::Error>,
    },
}

#[derive(Debug)]
pub enum ActionResult {
    TransitionApplied {
        issue_key: String,
        /// Full target status (id + name) so the board view can re-group the
        /// card by status id. None when the transition lookup failed.
        new_status: Option<StatusField>,
    },
    TransitionsLoaded {
        issue_key: String,
        transitions: Vec<Transition>,
    },
    /// Sprint list for the backlog's send-to-sprint picker. `None` means the
    /// board doesn't support sprints (kanban).
    SprintsLoaded {
        issue_key: String,
        sprints: Option<Vec<crate::jira::types::Sprint>>,
    },
    /// An issue left the backlog for a sprint.
    MovedToSprint {
        issue_key: String,
    },
    CommentPosted {
        issue_key: String,
        new_comment: Comment,
    },
    AssignedToMe {
        issue_key: String,
    },
    MovedToProject {
        issue_key: String,
        project: String,
    },
    Hidden {
        issue_key: String,
    },
    FieldUpdated {
        issue_key: String,
        field_id: String,
        new_value: serde_json::Value,
    },
    FieldOptionsLoaded {
        issue_key: String,
        field_id: String,
        label: String,
        original_json: serde_json::Value,
        options: Vec<FieldOption>,
        description: Option<String>,
        multi: bool,
    },
    FieldNamesLoaded {
        names: HashMap<String, String>,
        /// Jira editmeta schema info per `field_id` — used to drive picker
        /// selection (e.g. datetime) and ADF conversion for currently-empty
        /// rich-text fields.
        schemas: HashMap<String, FieldSchema>,
        /// True when this came from the global field registry (all fields loaded).
        all_fields: bool,
    },
    CommentEdited {
        issue_key: String,
        updated_comment: Comment,
    },
    CommentDeleted {
        issue_key: String,
        comment_id: String,
    },
    AttachmentCached {
        attachment_id: String,
        cache_path: std::path::PathBuf,
        open_after: bool,
    },
    AttachmentUploaded {
        issue_key: String,
        new_attachment: Attachment,
    },
    AttachmentDeleted {
        issue_key: String,
        attachment_id: String,
    },
    /// A new issue was created; carries its key for the confirmation popup.
    IssueCreated {
        key: String,
    },
    /// A Confluence inline task was marked complete.
    TaskCompleted {
        item_key: String,
    },
    Error(anyhow::Error),
}
