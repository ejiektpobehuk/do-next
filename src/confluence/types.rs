use std::collections::HashMap;

use anyhow::{Result, anyhow};
use chrono::{DateTime, NaiveTime, Utc};
use serde::{Deserialize, Deserializer};

use crate::config::types::ConfluenceFilters;

/// Inline-task status as returned by the Confluence v2 tasks API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Complete,
    Incomplete,
}

impl TaskStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Incomplete => "incomplete",
        }
    }
}

/// Wire shape of one task from `GET /wiki/api/v2/tasks`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiTask {
    #[serde(deserialize_with = "de_id")]
    pub id: String,
    #[serde(default, deserialize_with = "de_opt_id")]
    pub space_id: Option<String>,
    #[serde(default, deserialize_with = "de_opt_id")]
    pub page_id: Option<String>,
    #[serde(default, deserialize_with = "de_opt_id")]
    pub blog_post_id: Option<String>,
    pub status: TaskStatus,
    #[serde(default)]
    pub body: Option<TaskBody>,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub due_at: Option<DateTime<Utc>>,
}

/// Task body container; we always request `atlas_doc_format`.
#[derive(Debug, Clone, Deserialize)]
pub struct TaskBody {
    #[serde(default)]
    pub atlas_doc_format: Option<BodyRepresentation>,
    #[serde(default)]
    pub storage: Option<BodyRepresentation>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BodyRepresentation {
    pub value: String,
}

/// Response envelope for cursor-paginated v2 collections.
#[derive(Debug, Clone, Deserialize)]
pub struct TasksPage {
    #[serde(default)]
    pub results: Vec<ApiTask>,
    #[serde(rename = "_links", default)]
    pub links: PageLinks,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct PageLinks {
    /// Relative URL of the next page (absent on the last page).
    #[serde(default)]
    pub next: Option<String>,
    /// Site base, e.g. `https://site.atlassian.net/wiki`.
    #[serde(default)]
    pub base: Option<String>,
}

/// Wire shape of one page from the bulk `GET /wiki/api/v2/pages` lookup
/// (blog posts share the same shape).
#[derive(Debug, Clone, Deserialize)]
pub struct ApiPage {
    #[serde(deserialize_with = "de_id")]
    pub id: String,
    pub title: String,
    #[serde(rename = "_links", default)]
    pub links: PageWebLinks,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct PageWebLinks {
    #[serde(default)]
    pub webui: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PagesPage {
    #[serde(default)]
    pub results: Vec<ApiPage>,
    #[serde(rename = "_links", default)]
    pub links: PageLinks,
}

/// Wire shape of one space from `GET /wiki/api/v2/spaces?keys=...`.
#[derive(Debug, Clone, Deserialize)]
pub struct ApiSpace {
    #[serde(deserialize_with = "de_id")]
    pub id: String,
    pub key: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SpacesPage {
    #[serde(default)]
    pub results: Vec<ApiSpace>,
}

/// Display metadata for the container (page or blog post) a task lives on.
#[derive(Debug, Clone)]
pub struct PageMeta {
    pub title: String,
    /// Absolute URL of the page in the browser.
    pub url: Option<String>,
}

/// Display-ready Confluence inline task handed to the TUI as a `WorkItem`.
#[derive(Debug, Clone)]
pub struct Task {
    pub id: String,
    /// Work-item key ("CONF:{id}") — cannot collide with Jira issue keys
    /// (`[A-Z]+-[0-9]+`), which also namespaces hidden-for-a-day entries.
    pub key: String,
    pub status: TaskStatus,
    /// First line of the task body — the list-row title.
    pub title: String,
    pub page_title: Option<String>,
    pub page_url: Option<String>,
    pub due_at: Option<DateTime<Utc>>,
    pub created_at: Option<DateTime<Utc>>,
    /// Which source this task was fetched from (set after fetch).
    pub source_id: Option<String>,
    /// Field map rendered by the default/custom views, keyed by `conf.*` ids.
    pub extra: HashMap<String, serde_json::Value>,
}

impl Task {
    /// Mark the task complete in place (after a successful API update).
    pub fn set_complete(&mut self) {
        self.status = TaskStatus::Complete;
        self.extra
            .insert(FIELD_STATUS.into(), TaskStatus::Complete.as_str().into());
    }

    /// The list-row label per the source's `label` option. Falls back to the
    /// task content when the page title is unavailable.
    pub fn list_label(&self, mode: crate::config::types::ConfluenceLabel) -> String {
        use crate::config::types::ConfluenceLabel;
        match mode {
            ConfluenceLabel::Task => self.title.clone(),
            ConfluenceLabel::Page => self
                .page_title
                .clone()
                .unwrap_or_else(|| self.title.clone()),
            ConfluenceLabel::Both => self.page_title.as_ref().map_or_else(
                || self.title.clone(),
                |page| format!("{} · {page}", self.title),
            ),
        }
    }
}

/// Stable field ids exposed to the view layer for Confluence tasks.
pub const FIELD_TASK: &str = "conf.task";
pub const FIELD_PAGE: &str = "conf.page";
pub const FIELD_SPACE: &str = "conf.space";
pub const FIELD_STATUS: &str = "conf.status";
pub const FIELD_DUE: &str = "conf.due";
pub const FIELD_URL: &str = "conf.url";

/// Builtin display names for the `conf.*` field ids (no editmeta exists).
pub fn field_name(field_id: &str) -> Option<&'static str> {
    match field_id {
        FIELD_TASK => Some("Task"),
        FIELD_PAGE => Some("Page"),
        FIELD_SPACE => Some("Space"),
        FIELD_STATUS => Some("Status"),
        FIELD_DUE => Some("Due date"),
        FIELD_URL => Some("Link"),
        _ => None,
    }
}

/// Build the display task from a wire task plus resolved container metadata.
/// `space_keys` maps space id → key for display.
pub fn to_display(
    task: ApiTask,
    containers: &HashMap<String, PageMeta>,
    space_keys: &HashMap<String, String>,
) -> Task {
    let body_markdown = task
        .body
        .as_ref()
        .and_then(|b| {
            b.atlas_doc_format
                .as_ref()
                .map(|r| adf_value_to_markdown(&r.value))
        })
        .or_else(|| {
            task.body
                .as_ref()
                .and_then(|b| b.storage.as_ref().map(|r| r.value.clone()))
        })
        .unwrap_or_default();
    let title = body_markdown
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("(blank task)")
        .trim()
        .to_owned();

    let container_id = task.page_id.as_ref().or(task.blog_post_id.as_ref());
    let meta = container_id.and_then(|id| containers.get(id));
    let page_title = meta.map(|m| m.title.clone());
    let page_url = meta.and_then(|m| m.url.clone());
    let space_key = task
        .space_id
        .as_ref()
        .and_then(|id| space_keys.get(id))
        .cloned();

    let mut extra: HashMap<String, serde_json::Value> = HashMap::new();
    extra.insert(FIELD_TASK.into(), body_markdown.into());
    extra.insert(FIELD_STATUS.into(), task.status.as_str().into());
    if let Some(p) = &page_title {
        extra.insert(FIELD_PAGE.into(), p.clone().into());
    }
    if let Some(s) = space_key {
        extra.insert(FIELD_SPACE.into(), s.into());
    }
    if let Some(due) = task.due_at {
        extra.insert(FIELD_DUE.into(), due.format("%Y-%m-%d").to_string().into());
    }
    if let Some(u) = &page_url {
        extra.insert(FIELD_URL.into(), u.clone().into());
    }

    Task {
        key: format!("CONF:{}", task.id),
        id: task.id,
        status: task.status,
        title,
        page_title,
        page_url,
        due_at: task.due_at,
        created_at: task.created_at,
        source_id: None,
        extra,
    }
}

/// The v2 API returns the ADF body as a JSON-encoded string; parse it and
/// reuse the Jira ADF renderer. Falls back to the raw string on parse failure.
fn adf_value_to_markdown(value: &str) -> String {
    serde_json::from_str::<serde_json::Value>(value).map_or_else(
        |_| value.to_owned(),
        |doc| crate::jira::adf::adf_to_markdown(&doc),
    )
}

/// Map per-source filters to `GET /wiki/api/v2/tasks` query parameters.
///
/// `me` is the current user's account id (required unless the filter pins an
/// explicit assignee or "any"); `space_ids` are the numeric ids the config's
/// space keys resolved to.
pub fn build_task_query(
    filters: &ConfluenceFilters,
    me: Option<&str>,
    space_ids: &[String],
) -> Result<Vec<(String, String)>> {
    let mut q: Vec<(String, String)> = vec![
        ("body-format".into(), "atlas_doc_format".into()),
        ("limit".into(), "250".into()),
        (
            "include-blank-tasks".into(),
            filters.include_blank.to_string(),
        ),
    ];

    match filters.status.as_deref() {
        None => q.push(("status".into(), "incomplete".into())),
        Some("any") => {}
        Some(s) => q.push(("status".into(), s.into())),
    }

    match filters.assignee.as_deref() {
        None | Some("me") => {
            let me = me.ok_or_else(|| {
                anyhow!("cannot resolve the current Confluence user for `assignee: \"me\"`")
            })?;
            q.push(("assigned-to".into(), me.into()));
        }
        Some("any") => {}
        Some(account_id) => q.push(("assigned-to".into(), account_id.into())),
    }

    for id in space_ids {
        q.push(("space-id".into(), id.clone()));
    }
    for id in &filters.pages {
        q.push(("page-id".into(), id.clone()));
    }

    if let Some(date) = filters.due_after.as_deref() {
        q.push(("due-at-from".into(), day_bound_millis(date, false)?));
    }
    if let Some(date) = filters.due_before.as_deref() {
        q.push(("due-at-to".into(), day_bound_millis(date, true)?));
    }

    Ok(q)
}

/// Epoch milliseconds for the start (or end) of a `YYYY-MM-DD` day in UTC.
fn day_bound_millis(date: &str, end_of_day: bool) -> Result<String> {
    let day = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map_err(|e| anyhow!("invalid date \"{date}\": {e}"))?;
    let time = if end_of_day {
        NaiveTime::from_hms_milli_opt(23, 59, 59, 999)
    } else {
        NaiveTime::from_hms_opt(0, 0, 0)
    }
    .ok_or_else(|| anyhow!("invalid time bound"))?;
    Ok(day.and_time(time).and_utc().timestamp_millis().to_string())
}

/// Extract the `cursor` query parameter from a `_links.next` relative URL.
/// Only the cursor is reused — the relative URL must never be joined onto the
/// OAuth `api.atlassian.com` base.
pub fn next_cursor(links: &PageLinks) -> Option<String> {
    let next = links.next.as_deref()?;
    let query = next.split_once('?').map(|(_, q)| q)?;
    query
        .split('&')
        .find_map(|pair| pair.strip_prefix("cursor="))
        .map(percent_decode)
}

/// Minimal percent-decoding for a URL query value.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let (Some(h), Some(l)) = (
                bytes.get(i + 1).copied().and_then(hex_val),
                bytes.get(i + 2).copied().and_then(hex_val),
            )
        {
            out.push(h * 16 + l);
            i += 3;
        } else if bytes[i] == b'+' {
            out.push(b' ');
            i += 1;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

const fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn de_id<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    id_from_value(&v).ok_or_else(|| serde::de::Error::custom("expected string or number id"))
}

fn de_opt_id<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    let v = Option::<serde_json::Value>::deserialize(d)?;
    Ok(v.as_ref().and_then(id_from_value))
}

fn id_from_value(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(query: &[(String, String)], key: &str) -> Vec<String> {
        query
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .collect()
    }

    #[test]
    fn default_filters_mean_my_incomplete_tasks() {
        let query = build_task_query(&ConfluenceFilters::default(), Some("acc-1"), &[]).unwrap();
        assert_eq!(q(&query, "status"), vec!["incomplete"]);
        assert_eq!(q(&query, "assigned-to"), vec!["acc-1"]);
        assert_eq!(q(&query, "include-blank-tasks"), vec!["false"]);
        assert_eq!(q(&query, "body-format"), vec!["atlas_doc_format"]);
        assert!(q(&query, "space-id").is_empty());
    }

    #[test]
    fn any_status_and_assignee_omit_params() {
        let filters = ConfluenceFilters {
            status: Some("any".into()),
            assignee: Some("any".into()),
            ..Default::default()
        };
        let query = build_task_query(&filters, None, &[]).unwrap();
        assert!(q(&query, "status").is_empty());
        assert!(q(&query, "assigned-to").is_empty());
    }

    #[test]
    fn assignee_me_without_account_id_errors() {
        assert!(build_task_query(&ConfluenceFilters::default(), None, &[]).is_err());
    }

    #[test]
    fn spaces_and_pages_repeat_params() {
        let filters = ConfluenceFilters {
            pages: vec!["11".into(), "22".into()],
            assignee: Some("any".into()),
            ..Default::default()
        };
        let query = build_task_query(&filters, None, &["100".into(), "200".into()]).unwrap();
        assert_eq!(q(&query, "space-id"), vec!["100", "200"]);
        assert_eq!(q(&query, "page-id"), vec!["11", "22"]);
    }

    #[test]
    fn due_dates_become_inclusive_epoch_millis() {
        let filters = ConfluenceFilters {
            assignee: Some("any".into()),
            due_after: Some("2026-07-01".into()),
            due_before: Some("2026-07-31".into()),
            ..Default::default()
        };
        let query = build_task_query(&filters, None, &[]).unwrap();
        // 2026-07-01T00:00:00Z / 2026-07-31T23:59:59.999Z
        assert_eq!(q(&query, "due-at-from"), vec!["1782864000000"]);
        assert_eq!(q(&query, "due-at-to"), vec!["1785542399999"]);
    }

    #[test]
    fn next_cursor_extracts_and_decodes() {
        let links = PageLinks {
            next: Some("/wiki/api/v2/tasks?cursor=abc%3D%3D&limit=250".into()),
            base: None,
        };
        assert_eq!(next_cursor(&links).as_deref(), Some("abc=="));
        assert_eq!(next_cursor(&PageLinks::default()), None);
    }

    #[test]
    fn api_task_deserializes_with_double_encoded_adf_body() {
        let raw = r#"{
            "id": "42",
            "localId": "x",
            "spaceId": "10",
            "pageId": "1234",
            "status": "incomplete",
            "body": {
                "atlas_doc_format": {
                    "representation": "atlas_doc_format",
                    "value": "{\"type\":\"doc\",\"version\":1,\"content\":[{\"type\":\"paragraph\",\"content\":[{\"type\":\"text\",\"text\":\"Review the runbook\"}]}]}"
                }
            },
            "createdAt": "2026-07-01T10:00:00.000Z",
            "dueAt": "2026-07-10T00:00:00.000Z"
        }"#;
        let task: ApiTask = serde_json::from_str(raw).unwrap();
        assert_eq!(task.id, "42");
        assert_eq!(task.status, TaskStatus::Incomplete);

        let display = to_display(
            task,
            &std::collections::HashMap::from([(
                "1234".to_owned(),
                PageMeta {
                    title: "Ops runbook".into(),
                    url: Some("https://acme.atlassian.net/wiki/spaces/OPS/pages/1234".into()),
                },
            )]),
            &std::collections::HashMap::from([("10".to_owned(), "OPS".to_owned())]),
        );
        assert_eq!(display.key, "CONF:42");
        assert_eq!(display.title, "Review the runbook");
        assert_eq!(display.page_title.as_deref(), Some("Ops runbook"));
        assert_eq!(
            display.extra.get(FIELD_DUE).and_then(|v| v.as_str()),
            Some("2026-07-10")
        );
        assert_eq!(
            display.extra.get(FIELD_SPACE).and_then(|v| v.as_str()),
            Some("OPS")
        );
        assert_eq!(
            display.extra.get(FIELD_URL).and_then(|v| v.as_str()),
            Some("https://acme.atlassian.net/wiki/spaces/OPS/pages/1234")
        );
    }

    #[test]
    fn blank_task_body_gets_placeholder_title() {
        let raw = r#"{ "id": "7", "status": "complete" }"#;
        let task: ApiTask = serde_json::from_str(raw).unwrap();
        let display = to_display(
            task,
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
        );
        assert_eq!(display.title, "(blank task)");
        assert_eq!(display.status, TaskStatus::Complete);
        assert!(display.page_url.is_none());
    }

    #[test]
    fn list_label_modes() {
        use crate::config::types::ConfluenceLabel;
        let raw = r#"{ "id": "1", "status": "incomplete", "body": { "atlas_doc_format": {
            "value": "{\"type\":\"doc\",\"version\":1,\"content\":[{\"type\":\"paragraph\",\"content\":[{\"type\":\"text\",\"text\":\"Ship it\"}]}]}" } } }"#;
        let task: ApiTask = serde_json::from_str(raw).unwrap();
        let with_page = to_display(
            ApiTask {
                page_id: Some("9".into()),
                ..task.clone()
            },
            &std::collections::HashMap::from([(
                "9".to_owned(),
                PageMeta {
                    title: "Release plan".into(),
                    url: None,
                },
            )]),
            &std::collections::HashMap::new(),
        );
        assert_eq!(with_page.list_label(ConfluenceLabel::Task), "Ship it");
        assert_eq!(with_page.list_label(ConfluenceLabel::Page), "Release plan");
        assert_eq!(
            with_page.list_label(ConfluenceLabel::Both),
            "Ship it · Release plan"
        );

        // No page title resolved → all modes fall back to the content.
        let no_page = to_display(
            task,
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
        );
        assert_eq!(no_page.list_label(ConfluenceLabel::Page), "Ship it");
        assert_eq!(no_page.list_label(ConfluenceLabel::Both), "Ship it");
    }

    #[test]
    fn numeric_ids_deserialize_as_strings() {
        let raw = r#"{ "id": 42, "pageId": 1234, "status": "incomplete" }"#;
        let task: ApiTask = serde_json::from_str(raw).unwrap();
        assert_eq!(task.id, "42");
        assert_eq!(task.page_id.as_deref(), Some("1234"));
    }
}
