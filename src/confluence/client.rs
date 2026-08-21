use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;
use reqwest::Client;
use serde_json::json;
use tokio::sync::RwLock;

use crate::atlassian::auth::{self, Auth};
use crate::config::types::ConfluenceFilters;

use super::types::{
    ApiPageSummary, ApiPageVersion, ApiTask, PageMeta, PageSummariesPage, PageVersionsPage,
    PagesPage, SearchPage, SpacesPage, Task, TasksPage, build_completed_task_query,
    build_page_contributor_cql, build_task_query, next_cursor, to_display,
};

/// Maximum ids per bulk pages/blogposts lookup (API limit is 250).
const BULK_CHUNK: usize = 250;

/// HTTP client for the Confluence Cloud v2 REST API.
///
/// May share its `Auth` handle with the `JiraClient` for the same Atlassian
/// site so OAuth token refresh stays coordinated.
#[derive(Clone)]
pub struct ConfluenceClient {
    client: Client,
    site_url: String,
    auth: Arc<RwLock<Auth>>,
    /// Cached current-user account id.
    me: Arc<RwLock<Option<String>>>,
    /// Cached space key (uppercase) → numeric space id.
    space_ids: Arc<RwLock<HashMap<String, String>>>,
}

impl ConfluenceClient {
    pub fn new(site_url: &str, auth: Auth) -> Result<Self> {
        Self::from_shared(site_url, Arc::new(RwLock::new(auth)))
    }

    /// Build a client that shares an existing auth handle (same site as Jira).
    pub fn from_shared(site_url: &str, auth: Arc<RwLock<Auth>>) -> Result<Self> {
        let client = Client::builder()
            .build()
            .context("Failed to build HTTP client")?;
        Ok(Self {
            client,
            site_url: site_url.trim_end_matches('/').to_owned(),
            auth,
            me: Arc::new(RwLock::new(None)),
            space_ids: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// API base ending in `/wiki`. Depends on the auth variant (OAuth goes
    /// through `api.atlassian.com`), so it is computed per call.
    async fn base(&self) -> String {
        match &*self.auth.read().await {
            Auth::Basic(_) => format!("{}/wiki", self.site_url),
            Auth::OAuth(o) => format!(
                "https://api.atlassian.com/ex/confluence/{}/wiki",
                o.cloud_id
            ),
        }
    }

    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        query: &[(String, String)],
    ) -> Result<T> {
        auth::maybe_refresh(&self.auth).await?;
        let req = self.client.get(url).query(query);
        let resp = auth::apply(&self.auth, req)
            .await
            .send()
            .await
            .with_context(|| format!("Failed to send Confluence request: {url}"))?;
        crate::http::json_response("Confluence", url, resp).await
    }

    /// Fetch all inline tasks matching the source filters, with container
    /// titles resolved for display.
    pub async fn fetch_tasks(&self, filters: &ConfluenceFilters) -> Result<Vec<Task>> {
        let me = if matches!(filters.assignee.as_deref(), None | Some("me")) {
            Some(self.current_account_id().await?)
        } else {
            None
        };
        let space_ids = self.resolve_space_ids(&filters.spaces).await?;
        let space_keys: HashMap<String, String> = self
            .space_ids
            .read()
            .await
            .iter()
            .map(|(key, id)| (id.clone(), key.clone()))
            .collect();

        let base_query = build_task_query(filters, me.as_deref(), &space_ids)?;
        let base = self.base().await;
        let url = format!("{base}/api/v2/tasks");

        let mut tasks: Vec<ApiTask> = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut query = base_query.clone();
            if let Some(c) = &cursor {
                query.push(("cursor".into(), c.clone()));
            }
            log::debug!("Confluence tasks request: cursor={cursor:?}");
            let page: TasksPage = self.get_json(&url, &query).await?;
            tasks.extend(page.results);
            cursor = next_cursor(&page.links);
            if cursor.is_none() {
                break;
            }
        }
        log::debug!("Confluence tasks fetched: {}", tasks.len());

        let containers = self.resolve_containers(&tasks).await;

        let mut out: Vec<Task> = tasks
            .into_iter()
            .map(|t| to_display(t, &containers, &space_keys))
            .collect();
        // Due-dated tasks first (soonest first), then by creation time.
        out.sort_by_key(|t| (t.due_at.is_none(), t.due_at, t.created_at));
        Ok(out)
    }

    /// Resolve container (page/blog post) titles + URLs for a set of tasks.
    /// Best-effort: failures degrade to missing titles, not a failed fetch.
    async fn resolve_containers(&self, tasks: &[ApiTask]) -> HashMap<String, PageMeta> {
        let page_ids: Vec<String> = unique_ids(tasks.iter().filter_map(|t| t.page_id.clone()));
        let blog_ids: Vec<String> = unique_ids(tasks.iter().filter_map(|t| t.blog_post_id.clone()));

        let mut meta = HashMap::new();
        for (endpoint, ids) in [("pages", page_ids), ("blogposts", blog_ids)] {
            for chunk in ids.chunks(BULK_CHUNK) {
                match self.fetch_page_meta(endpoint, chunk).await {
                    Ok(m) => meta.extend(m),
                    Err(e) => log::warn!("Confluence {endpoint} lookup failed: {e:#}"),
                }
            }
        }
        meta
    }

    async fn fetch_page_meta(
        &self,
        endpoint: &str,
        ids: &[String],
    ) -> Result<HashMap<String, PageMeta>> {
        let base = self.base().await;
        let url = format!("{base}/api/v2/{endpoint}");
        let mut query: Vec<(String, String)> =
            ids.iter().map(|id| ("id".to_owned(), id.clone())).collect();
        query.push(("limit".into(), "250".into()));
        let page: PagesPage = self.get_json(&url, &query).await?;
        let web_base = page.links.base.clone();
        Ok(page
            .results
            .into_iter()
            .map(|p| {
                let url = match (&web_base, &p.links.webui) {
                    (Some(b), Some(w)) => Some(format!("{}{w}", b.trim_end_matches('/'))),
                    _ => None,
                };
                (
                    p.id.clone(),
                    PageMeta {
                        title: p.title,
                        url,
                    },
                )
            })
            .collect())
    }

    /// Tasks you ticked off inside a window, filtered entirely server-side.
    pub async fn fetch_completed_tasks(
        &self,
        from: chrono::DateTime<Utc>,
        to: chrono::DateTime<Utc>,
    ) -> Result<Vec<Task>> {
        let me = self.current_account_id().await?;
        let base_query =
            build_completed_task_query(&me, from.timestamp_millis(), to.timestamp_millis());
        let base = self.base().await;
        let url = format!("{base}/api/v2/tasks");

        let mut tasks: Vec<ApiTask> = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut query = base_query.clone();
            if let Some(c) = &cursor {
                query.push(("cursor".into(), c.clone()));
            }
            let page: TasksPage = self.get_json(&url, &query).await?;
            tasks.extend(page.results);
            cursor = next_cursor(&page.links);
            if cursor.is_none() {
                break;
            }
        }
        log::debug!("Confluence completed tasks fetched: {}", tasks.len());

        let containers = self.resolve_containers(&tasks).await;
        let space_keys: HashMap<String, String> = self
            .space_ids
            .read()
            .await
            .iter()
            .map(|(key, id)| (id.clone(), key.clone()))
            .collect();
        Ok(tasks
            .into_iter()
            .map(|t| to_display(t, &containers, &space_keys))
            .collect())
    }

    /// Page ids you contributed to, modified on or after `from` (day-granular).
    ///
    /// Uses the v1 CQL search endpoint, whose granular scope
    /// (`read:content-details:confluence`) is already in the app's Confluence
    /// scope set — adding the classic `search:confluence` would mix scope
    /// families in one authorization, which is the combination Atlassian has not
    /// been verified to accept.
    ///
    /// Returns `(page_id, title, url)`. Precision comes from
    /// [`Self::fetch_page_versions`] afterwards.
    pub async fn search_contributed_pages(
        &self,
        from: chrono::NaiveDate,
        space_keys: &[String],
        limit: u32,
    ) -> Result<Vec<(String, String, Option<String>)>> {
        let mut cql = build_page_contributor_cql(from);
        if !space_keys.is_empty() {
            let list = space_keys
                .iter()
                .map(|k| format!("\"{}\"", k.replace('"', "")))
                .collect::<Vec<_>>()
                .join(", ");
            cql = format!("{cql} AND space IN ({list})");
        }
        let base = self.base().await;
        let url = format!("{base}/rest/api/search");
        let query = vec![
            ("cql".to_owned(), cql.clone()),
            ("limit".to_owned(), limit.to_string()),
        ];
        log::debug!("Confluence CQL search: {cql}");
        let page: SearchPage = self.get_json(&url, &query).await?;
        let web_base = page.links.base.clone();
        Ok(page
            .results
            .into_iter()
            .filter_map(|row| {
                let content = row.content?;
                let title = content
                    .title
                    .or(row.title)
                    .unwrap_or_else(|| content.id.clone());
                let url = match (&web_base, &row.url) {
                    (Some(b), Some(u)) => Some(format!("{}{u}", b.trim_end_matches('/'))),
                    _ => None,
                };
                Some((content.id, title, url))
            })
            .collect())
    }

    /// Versions of one page, newest first.
    pub async fn fetch_page_versions(&self, page_id: &str) -> Result<Vec<ApiPageVersion>> {
        let base = self.base().await;
        let url = format!("{base}/api/v2/pages/{page_id}/versions");
        let query = vec![
            ("limit".to_owned(), "50".to_owned()),
            ("sort".to_owned(), "-modified-date".to_owned()),
        ];
        let page: PageVersionsPage = self.get_json(&url, &query).await?;
        Ok(page.results)
    }

    /// Reduced-accuracy page activity: walk pages newest-modified first and stop
    /// once they predate the window.
    ///
    /// The fallback for when the CQL search is not permitted. Each page carries
    /// its creator and its latest version's author, which covers "I created it"
    /// and "I last edited it" — but *not* a page you edited that someone else
    /// edited afterwards. Requires spaces: unscoped this walks every edit on the
    /// site.
    pub async fn walk_recent_pages(
        &self,
        space_keys: &[String],
        since: chrono::DateTime<Utc>,
        max_pages: u32,
    ) -> Result<Vec<ApiPageSummary>> {
        if space_keys.is_empty() {
            anyhow::bail!(
                "Confluence page activity needs `standup.confluence.spaces` when the CQL \
                 search is unavailable — an unscoped walk would read every recent edit \
                 on the site"
            );
        }
        let space_ids = self.resolve_space_ids(space_keys).await?;
        let base = self.base().await;
        let url = format!("{base}/api/v2/pages");

        let mut out: Vec<ApiPageSummary> = Vec::new();
        let mut cursor: Option<String> = None;
        let mut pages_walked = 0;
        'outer: loop {
            let mut query: Vec<(String, String)> = vec![
                ("limit".to_owned(), "250".to_owned()),
                ("sort".to_owned(), "-modified-date".to_owned()),
            ];
            for id in &space_ids {
                query.push(("space-id".to_owned(), id.clone()));
            }
            if let Some(c) = &cursor {
                query.push(("cursor".into(), c.clone()));
            }
            let page: PageSummariesPage = self.get_json(&url, &query).await?;
            for summary in page.results {
                // Sorted newest-modified first, so the first page older than the
                // window ends the walk.
                let modified = summary
                    .version
                    .as_ref()
                    .map(|v| v.created_at)
                    .or(summary.created_at);
                if modified.is_some_and(|m| m < since) {
                    break 'outer;
                }
                out.push(summary);
            }
            cursor = next_cursor(&page.links);
            pages_walked += 1;
            if cursor.is_none() || pages_walked >= max_pages {
                if cursor.is_some() {
                    log::warn!(
                        "Confluence page walk stopped at the {max_pages}-page cap \
                         ({} pages seen); narrow `standup.confluence.spaces`",
                        out.len()
                    );
                }
                break;
            }
        }
        Ok(out)
    }

    /// Mark a task complete (or incomplete).
    pub async fn set_task_status(&self, task_id: &str, complete: bool) -> Result<()> {
        auth::maybe_refresh(&self.auth).await?;
        let base = self.base().await;
        let url = format!("{base}/api/v2/tasks/{task_id}");
        let status = if complete { "complete" } else { "incomplete" };
        let req = self.client.put(&url).json(&json!({ "status": status }));
        let resp = auth::apply(&self.auth, req)
            .await
            .send()
            .await
            .context("Failed to send Confluence task update")?;
        let code = resp.status();
        if !code.is_success() {
            let body = resp.text().await.unwrap_or_default();
            log::error!("Confluence task update error {code}: {body}");
            anyhow::bail!("Confluence API error {code}: {body}");
        }
        Ok(())
    }

    /// Current user's Atlassian account id — the attribution key standup mode
    /// compares page-version and task authors against.
    pub async fn account_id(&self) -> Result<String> {
        self.current_account_id().await
    }

    /// Current user's Atlassian account id (cached). There is no v2 "myself"
    /// endpoint; the v1 user/current one is stable on Cloud.
    async fn current_account_id(&self) -> Result<String> {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct CurrentUser {
            account_id: String,
        }
        let cached = self.me.read().await.clone();
        if let Some(id) = cached {
            return Ok(id);
        }
        let base = self.base().await;
        let url = format!("{base}/rest/api/user/current");
        let user: CurrentUser = self.get_json(&url, &[]).await?;
        *self.me.write().await = Some(user.account_id.clone());
        Ok(user.account_id)
    }

    /// Resolve space keys to numeric ids (cached), preserving input order.
    /// Errors clearly on keys that don't exist or aren't visible.
    async fn resolve_space_ids(&self, keys: &[String]) -> Result<Vec<String>> {
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let missing: Vec<String> = {
            let cache = self.space_ids.read().await;
            keys.iter()
                .filter(|k| !cache.contains_key(&k.to_uppercase()))
                .cloned()
                .collect()
        };
        if !missing.is_empty() {
            let base = self.base().await;
            let url = format!("{base}/api/v2/spaces");
            let mut query: Vec<(String, String)> = missing
                .iter()
                .map(|k| ("keys".to_owned(), k.clone()))
                .collect();
            query.push(("limit".into(), "250".into()));
            let page: SpacesPage = self.get_json(&url, &query).await?;
            let mut cache = self.space_ids.write().await;
            for space in page.results {
                cache.insert(space.key.to_uppercase(), space.id);
            }
        }
        let cache = self.space_ids.read().await;
        keys.iter()
            .map(|k| {
                cache.get(&k.to_uppercase()).cloned().ok_or_else(|| {
                    anyhow::anyhow!("Confluence space \"{k}\" not found (check the space key)")
                })
            })
            .collect()
    }
}

fn unique_ids(ids: impl Iterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    ids.filter(|id| seen.insert(id.clone())).collect()
}
