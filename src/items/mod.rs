use std::collections::HashMap;

use crate::confluence::types::{Task, TaskStatus};
use crate::jira::types::{Issue, PriorityField, UserField};

/// A single unit of work from any configured source.
///
/// Sources produce concrete payloads; the TUI operates on this enum through
/// the shared accessors below and reaches for `as_jira` / `as_confluence`
/// only in source-specific flows (transitions, comments, mark-complete, …).
#[expect(
    clippy::large_enum_variant,
    reason = "items live in Vecs that held full Issues before; boxing would \
              only add indirection to the hot rendering path"
)]
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub enum WorkItem {
    Jira(Issue),
    Confluence(Task),
}

impl WorkItem {
    /// Globally-unique identity used for dedup, hidden-for-a-day entries and
    /// selection restore. Jira items use the issue key ("PROJ-123");
    /// Confluence tasks use `CONF:{task_id}`.
    pub fn key(&self) -> &str {
        match self {
            Self::Jira(issue) => &issue.key,
            Self::Confluence(task) => &task.key,
        }
    }

    pub fn title(&self) -> &str {
        match self {
            Self::Jira(issue) => &issue.fields.summary,
            Self::Confluence(task) => &task.title,
        }
    }

    pub fn status_name(&self) -> &str {
        match self {
            Self::Jira(issue) => &issue.fields.status.name,
            Self::Confluence(task) => match task.status {
                TaskStatus::Incomplete => "To do",
                TaskStatus::Complete => "Done",
            },
        }
    }

    /// Single-char priority indicator for the list row.
    pub fn priority_symbol(&self) -> &'static str {
        match self {
            Self::Jira(issue) => issue
                .fields
                .priority
                .as_ref()
                .map_or("·", PriorityField::symbol),
            Self::Confluence(_) => "·",
        }
    }

    pub fn assignee_display(&self) -> Option<&str> {
        match self {
            Self::Jira(issue) => issue.fields.assignee.as_ref().map(UserField::display),
            Self::Confluence(_) => None,
        }
    }

    /// Project key for search filter matching; `None` for sources whose items
    /// have no project.
    pub fn project_key(&self) -> Option<&str> {
        match self {
            Self::Jira(issue) => Some(&issue.fields.project.key),
            Self::Confluence(_) => None,
        }
    }

    pub fn source_id(&self) -> Option<&str> {
        match self {
            Self::Jira(issue) => issue.source_id.as_deref(),
            Self::Confluence(task) => task.source_id.as_deref(),
        }
    }

    pub const fn subsource_idx(&self) -> usize {
        match self {
            Self::Jira(issue) => issue.subsource_idx,
            Self::Confluence(_) => 0,
        }
    }

    pub fn set_source(&mut self, source_id: String, subsource_idx: usize) {
        match self {
            Self::Jira(issue) => {
                issue.source_id = Some(source_id);
                issue.subsource_idx = subsource_idx;
            }
            Self::Confluence(task) => {
                task.source_id = Some(source_id);
            }
        }
    }

    /// Field map rendered by the default and custom views, keyed by field id
    /// (Jira custom-field ids; `conf.*` ids for Confluence tasks).
    pub const fn fields_map(&self) -> &HashMap<String, serde_json::Value> {
        match self {
            Self::Jira(issue) => &issue.fields.extra,
            Self::Confluence(task) => &task.extra,
        }
    }

    pub fn field(&self, field_id: &str) -> Option<&serde_json::Value> {
        self.fields_map().get(field_id)
    }

    /// URL opened by `o` (open in browser).
    pub fn browse_url(&self, jira_base_url: &str) -> String {
        match self {
            Self::Jira(issue) => format!("{jira_base_url}/browse/{}", issue.key),
            Self::Confluence(task) => task
                .page_url
                .clone()
                .unwrap_or_else(|| jira_base_url.to_owned()),
        }
    }

    pub const fn as_jira(&self) -> Option<&Issue> {
        match self {
            Self::Jira(issue) => Some(issue),
            Self::Confluence(_) => None,
        }
    }

    pub const fn as_jira_mut(&mut self) -> Option<&mut Issue> {
        match self {
            Self::Jira(issue) => Some(issue),
            Self::Confluence(_) => None,
        }
    }

    pub const fn as_confluence(&self) -> Option<&Task> {
        match self {
            Self::Confluence(task) => Some(task),
            Self::Jira(_) => None,
        }
    }

    // ── Capabilities — gate keybindings and detail-view chrome ────────────

    /// Comments/Attachments sub-views (and their nav widgets) exist.
    pub const fn supports_comments(&self) -> bool {
        matches!(self, Self::Jira(_))
    }

    /// Fields can be edited and pushed back (Jira editmeta flow).
    pub const fn supports_field_edit(&self) -> bool {
        matches!(self, Self::Jira(_))
    }

    /// The item can be checked off (Confluence inline task).
    pub const fn supports_complete(&self) -> bool {
        matches!(self, Self::Confluence(_))
    }
}
