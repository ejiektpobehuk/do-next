use std::collections::HashMap;

use crate::confluence::types::{Task, TaskStatus};
use crate::gitlab::types::MergeRequest;
use crate::jira::types::{Issue, PriorityField, UserField};

/// Field id of the Jira issue description. Unlike every other renderable
/// field it is a *typed* field on `IssueFields`, not an entry in the flattened
/// `extra` map, so `field()` resolves it specially.
pub const FIELD_DESCRIPTION: &str = "description";

/// A single unit of work from any configured source.
///
/// Sources produce concrete payloads; the TUI operates on this enum through
/// the shared accessors below and reaches for `as_jira` / `as_confluence` /
/// `as_gitlab` only in source-specific flows (transitions, comments,
/// mark-complete, list rows, …).
///
/// Payloads are stored inline, not boxed: these items live in `Vec`s that held
/// full `Issue`s before, so boxing would only add indirection to the hot
/// rendering path.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub enum WorkItem {
    Jira(Issue),
    Confluence(Task),
    Gitlab(MergeRequest),
}

impl WorkItem {
    /// Globally-unique identity used for dedup, hidden-for-a-day entries and
    /// selection restore. Jira items use the issue key ("PROJ-123");
    /// Confluence tasks use `CONF:{task_id}`; merge requests use
    /// `MR:{project_path}!{iid}`.
    pub fn key(&self) -> &str {
        match self {
            Self::Jira(issue) => &issue.key,
            Self::Confluence(task) => &task.key,
            Self::Gitlab(mr) => &mr.key,
        }
    }

    pub fn title(&self) -> &str {
        match self {
            Self::Jira(issue) => &issue.fields.summary,
            Self::Confluence(task) => &task.title,
            Self::Gitlab(mr) => &mr.title,
        }
    }

    pub fn status_name(&self) -> &str {
        match self {
            Self::Jira(issue) => &issue.fields.status.name,
            Self::Confluence(task) => match task.status {
                TaskStatus::Incomplete => "To do",
                TaskStatus::Complete => "Done",
            },
            Self::Gitlab(mr) => &mr.status_label,
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
            Self::Confluence(_) | Self::Gitlab(_) => "·",
        }
    }

    pub fn assignee_display(&self) -> Option<&str> {
        match self {
            Self::Jira(issue) => issue.fields.assignee.as_ref().map(UserField::display),
            Self::Confluence(_) => None,
            Self::Gitlab(mr) => mr.assignees.first().map(String::as_str),
        }
    }

    /// Project key for search filter matching; `None` for sources whose items
    /// have no project.
    pub fn project_key(&self) -> Option<&str> {
        match self {
            Self::Jira(issue) => Some(&issue.fields.project.key),
            Self::Confluence(_) => None,
            Self::Gitlab(mr) => mr.project_path.as_deref(),
        }
    }

    pub fn source_id(&self) -> Option<&str> {
        match self {
            Self::Jira(issue) => issue.source_id.as_deref(),
            Self::Confluence(task) => task.source_id.as_deref(),
            Self::Gitlab(mr) => mr.source_id.as_deref(),
        }
    }

    pub const fn subsource_idx(&self) -> usize {
        match self {
            Self::Jira(issue) => issue.subsource_idx,
            Self::Confluence(_) | Self::Gitlab(_) => 0,
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
            Self::Gitlab(mr) => {
                mr.source_id = Some(source_id);
            }
        }
    }

    /// Field map rendered by the default and custom views, keyed by field id
    /// (Jira custom-field ids; `conf.*` for Confluence tasks, `gl.*` for merge
    /// requests).
    pub const fn fields_map(&self) -> &HashMap<String, serde_json::Value> {
        match self {
            Self::Jira(issue) => &issue.fields.extra,
            Self::Confluence(task) => &task.extra,
            Self::Gitlab(mr) => &mr.extra,
        }
    }

    /// Value of a renderable field. `description` is a typed field on a Jira
    /// issue rather than an `extra` entry, so it is resolved before the map
    /// lookup; `extra` can never carry that key (it is `#[serde(flatten)]`
    /// alongside the named field), so the order is unambiguous.
    pub fn field(&self, field_id: &str) -> Option<&serde_json::Value> {
        if field_id == FIELD_DESCRIPTION
            && let Self::Jira(issue) = self
        {
            return issue.fields.description.as_ref();
        }
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
            Self::Gitlab(mr) => mr.web_url.clone(),
        }
    }

    pub const fn as_jira(&self) -> Option<&Issue> {
        match self {
            Self::Jira(issue) => Some(issue),
            Self::Confluence(_) | Self::Gitlab(_) => None,
        }
    }

    pub const fn as_jira_mut(&mut self) -> Option<&mut Issue> {
        match self {
            Self::Jira(issue) => Some(issue),
            Self::Confluence(_) | Self::Gitlab(_) => None,
        }
    }

    pub const fn as_confluence(&self) -> Option<&Task> {
        match self {
            Self::Confluence(task) => Some(task),
            Self::Jira(_) | Self::Gitlab(_) => None,
        }
    }

    pub const fn as_gitlab(&self) -> Option<&MergeRequest> {
        match self {
            Self::Gitlab(mr) => Some(mr),
            Self::Jira(_) | Self::Confluence(_) => None,
        }
    }

    // ── Capabilities — gate keybindings and detail-view chrome ────────────

    /// Comments/Attachments sub-views (and their nav widgets) exist.
    pub const fn supports_comments(&self) -> bool {
        matches!(self, Self::Jira(_))
    }

    /// Fields can be edited and pushed back (Jira editmeta flow).
    /// False for merge requests — that is what makes them read-only for free:
    /// `t`, `c`, `a`, `m` and Enter-to-edit all gate through the capabilities.
    pub const fn supports_field_edit(&self) -> bool {
        matches!(self, Self::Jira(_))
    }

    /// The item can be checked off (Confluence inline task).
    pub const fn supports_complete(&self) -> bool {
        matches!(self, Self::Confluence(_))
    }
}
