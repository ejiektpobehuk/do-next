use std::collections::HashMap;

use crate::jira::types::{
    Attachment, Comment, FieldOption, FieldSchema, Issue, IssueTypeField, ProjectInfo, StatusInfo,
    Transition,
};

#[derive(Debug)]
pub enum AppEvent {
    /// Keyboard or mouse event from the terminal.
    Input(crossterm::event::Event),
    /// A background fetch completed successfully.
    SourceLoaded(String, Vec<Issue>),
    /// A whole-source fetch failed (no subsources).
    SourceError(String, anyhow::Error),
    /// One subsource fetch failed; other subsources continue.
    SubsourceError(String, usize, anyhow::Error),
    /// A Jira action (transition, comment, assign, move) completed.
    ActionDone(ActionResult),
    /// Current user resolved (sent once on startup).
    CurrentUserResolved(String),
    /// Spinner animation frame — only sent while sources are loading.
    Tick,
    /// Filesystem path completions ready (from debounced async fetch).
    PathCompletions {
        generation: u64,
        completions: Vec<String>,
    },
    /// Git-based update warnings for team configs (sent once on startup).
    UpdateWarnings(Vec<String>),
    /// A single-issue background refresh completed successfully.
    IssueRefreshed(Box<Issue>),
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
}

#[derive(Debug)]
pub enum ActionResult {
    TransitionApplied {
        issue_key: String,
        new_status: String,
    },
    TransitionsLoaded {
        issue_key: String,
        transitions: Vec<Transition>,
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
    Error(anyhow::Error),
}
