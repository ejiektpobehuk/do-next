use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Issue {
    pub id: String,
    pub key: String,
    pub fields: IssueFields,
    /// Which source this issue was fetched from (set after fetch).
    #[serde(skip)]
    pub source_id: Option<String>,
    /// Within-source subsource index for ordering (set after fetch).
    #[serde(skip)]
    pub subsource_idx: usize,
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
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Comment {
    pub id: String,
    pub author: UserField,
    pub body: serde_json::Value,
    pub created: String,
    pub updated: String,
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

/// Jira Cloud REST API v3 search response envelope.
#[derive(Debug, Deserialize)]
pub struct SearchResponse {
    pub issues: Vec<Issue>,
    #[serde(rename = "isLast", default)]
    pub is_last: bool,
}

/// Jira REST API transitions response envelope.
#[derive(Debug, Deserialize)]
pub struct TransitionsResponse {
    pub transitions: Vec<Transition>,
}

/// Metadata for a single Jira field (from `/rest/api/3/field`).
#[derive(Debug, Deserialize)]
pub struct FieldMeta {
    pub id: String,
    pub name: String,
}

/// A selectable option for a Jira select/array field (from editmeta `allowedValues`).
#[derive(Debug, Clone)]
pub struct FieldOption {
    pub value: String,
}
