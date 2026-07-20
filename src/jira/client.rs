use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use reqwest::{Client, RequestBuilder};
use serde_json::json;
use tokio::sync::RwLock;

use crate::jira::auth::Auth;
use crate::jira::types::{
    AgileIssuesResponse, AgilePage, Attachment, BoardConfiguration, Comment, FieldMeta,
    FieldSchema, GreenHopperBoardData, GreenHopperSwimlane, Issue, IssueTypeField, ProjectInfo,
    RankAnchor, SearchResponse, Sprint, StatusCategory, StatusInfo, Transition,
    TransitionsResponse,
};

const MAX_RESULTS: u32 = 100;

#[derive(Clone)]
pub struct JiraClient {
    client: Client,
    base_url: String,
    auth: Arc<RwLock<Auth>>,
}

impl JiraClient {
    pub fn new(site_url: String, auth: Auth) -> Result<Self> {
        let base_url = match &auth {
            Auth::Basic(_) => site_url,
            Auth::OAuth(o) => format!("https://api.atlassian.com/ex/jira/{}", o.cloud_id),
        };
        let client = Client::builder()
            .build()
            .context("Failed to build HTTP client")?;
        Ok(Self {
            client,
            base_url,
            auth: Arc::new(RwLock::new(auth)),
        })
    }

    /// The shared auth handle — passed to a `ConfluenceClient` for the same
    /// Atlassian site so OAuth token refresh stays coordinated.
    pub fn auth_handle(&self) -> Arc<RwLock<Auth>> {
        self.auth.clone()
    }

    async fn maybe_refresh(&self) -> Result<()> {
        crate::jira::auth::maybe_refresh(&self.auth).await
    }

    async fn apply_auth(&self, req: RequestBuilder) -> RequestBuilder {
        crate::jira::auth::apply(&self.auth, req).await
    }

    /// Fetch all issues matching a JQL query, paginating automatically.
    /// `/search/jql` is cursor-based: pagination goes by `nextPageToken`
    /// (`startAt` is ignored and would refetch the first page forever).
    pub async fn fetch_jql(&self, jql: &str) -> Result<Vec<Issue>> {
        let mut all_issues = Vec::new();
        let mut next_page_token: Option<String> = None;

        loop {
            let url = format!("{}/rest/api/3/search/jql", self.base_url);
            log::debug!(
                "JQL request: token={} jql={jql}",
                next_page_token.as_deref().unwrap_or("<first page>")
            );

            let mut query: Vec<(&str, String)> = vec![
                ("jql", jql.to_string()),
                ("maxResults", MAX_RESULTS.to_string()),
                ("fields", "*all".to_string()),
            ];
            if let Some(token) = &next_page_token {
                query.push(("nextPageToken", token.clone()));
            }

            self.maybe_refresh().await?;
            let resp = self
                .apply_auth(self.client.get(&url))
                .await
                .query(&query)
                .send()
                .await
                .map_err(|e| {
                    log::error!("JQL send error: {e}");
                    let mut src: Option<&dyn std::error::Error> = std::error::Error::source(&e);
                    while let Some(cause) = src {
                        log::error!("  caused by: {cause}");
                        src = cause.source();
                    }
                    e
                })
                .context("Failed to send JQL request")?;

            let status = resp.status();
            log::debug!("JQL response: HTTP {status}");
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                log::error!("JQL API error {status}: {body}");
                anyhow::bail!("Jira API error {status}: {body}");
            }

            let page: SearchResponse = resp
                .json()
                .await
                .context("Failed to parse search response")?;
            let fetched = page.issues.len();
            log::debug!("JQL page: fetched={fetched} isLast={}", page.is_last);
            let is_last = page.is_last;
            next_page_token = page.next_page_token;
            all_issues.extend(page.issues);

            if is_last || next_page_token.is_none() || fetched == 0 {
                break;
            }
        }

        Ok(all_issues)
    }

    /// Fetch only the keys of issues matching a JQL query. Used for cheap
    /// swimlane-membership checks where full issue payloads would be waste.
    pub async fn fetch_jql_keys(&self, jql: &str) -> Result<Vec<String>> {
        let mut all_keys = Vec::new();
        let mut next_page_token: Option<String> = None;

        loop {
            let url = format!("{}/rest/api/3/search/jql", self.base_url);
            let mut query: Vec<(&str, String)> = vec![
                ("jql", jql.to_string()),
                ("maxResults", MAX_RESULTS.to_string()),
                ("fields", "key".to_string()),
            ];
            if let Some(token) = &next_page_token {
                query.push(("nextPageToken", token.clone()));
            }
            self.maybe_refresh().await?;
            let resp = self
                .apply_auth(self.client.get(&url))
                .await
                .query(&query)
                .send()
                .await
                .context("Failed to send JQL keys request")?;

            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                anyhow::bail!("Jira API error {status}: {body}");
            }

            let page: SearchResponse = resp
                .json()
                .await
                .context("Failed to parse search response")?;
            let fetched = page.issues.len();
            let is_last = page.is_last;
            next_page_token = page.next_page_token;
            all_keys.extend(page.issues.into_iter().map(|i| i.key));

            if is_last || next_page_token.is_none() || fetched == 0 {
                break;
            }
        }

        Ok(all_keys)
    }

    /// Fetch an Agile board's column/status configuration and board type.
    pub async fn get_board_configuration(&self, board_id: u64) -> Result<BoardConfiguration> {
        let url = format!(
            "{}/rest/agile/1.0/board/{board_id}/configuration",
            self.base_url
        );
        self.maybe_refresh().await?;
        let resp = self
            .apply_auth(self.client.get(&url))
            .await
            .send()
            .await
            .context("Failed to fetch board configuration")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Jira API error {status}: {body}");
        }

        resp.json()
            .await
            .context("Failed to parse board configuration")
    }

    /// List a board's sprints filtered by state (e.g. "active").
    ///
    /// Returns `Ok(None)` on HTTP 400, which is how Jira says "this board
    /// does not support sprints" (kanban boards and sprint-less team-managed
    /// boards) — callers fall back to plain board issues without
    /// string-matching errors.
    pub async fn get_board_sprints(
        &self,
        board_id: u64,
        state: &str,
    ) -> Result<Option<Vec<Sprint>>> {
        let mut sprints = Vec::new();
        let mut start_at = 0u32;

        loop {
            let url = format!("{}/rest/agile/1.0/board/{board_id}/sprint", self.base_url);
            self.maybe_refresh().await?;
            let resp = self
                .apply_auth(self.client.get(&url))
                .await
                .query(&[
                    ("state", state),
                    ("maxResults", "50"),
                    ("startAt", &start_at.to_string()),
                ])
                .send()
                .await
                .context("Failed to fetch board sprints")?;

            let status = resp.status();
            if status == reqwest::StatusCode::BAD_REQUEST {
                return Ok(None);
            }
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                anyhow::bail!("Jira API error {status}: {body}");
            }

            let page: AgilePage<Sprint> = resp.json().await.context("Failed to parse sprints")?;
            let fetched = u32::try_from(page.values.len()).unwrap_or(0);
            let is_last = page.is_last;
            sprints.extend(page.values);

            if is_last || fetched == 0 {
                break;
            }
            start_at += fetched;
        }

        Ok(Some(sprints))
    }

    /// All issues on a board, in board rank order. `fields` is the Jira
    /// `fields` query value (e.g. `*all` or a comma-separated whitelist).
    pub async fn fetch_board_issues(&self, board_id: u64, fields: &str) -> Result<Vec<Issue>> {
        self.fetch_agile_issues(&format!("board/{board_id}/issue"), fields)
            .await
    }

    /// All issues in one sprint of a board, in rank order.
    pub async fn fetch_board_sprint_issues(
        &self,
        board_id: u64,
        sprint_id: u64,
        fields: &str,
    ) -> Result<Vec<Issue>> {
        self.fetch_agile_issues(
            &format!("board/{board_id}/sprint/{sprint_id}/issue"),
            fields,
        )
        .await
    }

    /// Pagination for Agile issue endpoints. Issues come back in board rank
    /// order and are appended in response order — callers must not re-sort,
    /// the rank IS the board order. Agile endpoints paginate by `total` (there
    /// is no `isLast`).
    ///
    /// The first page is fetched to learn `total` (and the server's effective
    /// page size), then the remaining pages are fetched concurrently and
    /// stitched back together in `startAt` order so rank is preserved.
    async fn fetch_agile_issues(&self, path: &str, fields: &str) -> Result<Vec<Issue>> {
        let first = self.fetch_agile_page(path, 0, fields).await?;
        let total = first.total;
        // Use the page size the server actually returned as the stride — Jira
        // may cap `maxResults` below what we ask, and a wrong stride would skip
        // issues. A zero-length first page means an empty board.
        let stride = u32::try_from(first.issues.len()).unwrap_or(0);
        let mut all_issues = first.issues;
        if stride == 0 || u32::try_from(all_issues.len()).unwrap_or(0) >= total {
            return Ok(all_issues);
        }

        let offsets = agile_page_offsets(stride, total);
        log::debug!(
            "Agile issues {path}: total={total} stride={stride}; fetching {} more page(s) concurrently",
            offsets.len()
        );
        let pages = futures::future::try_join_all(
            offsets
                .into_iter()
                .map(|off| self.fetch_agile_page(path, off, fields)),
        )
        .await?;
        // `try_join_all` preserves input order, so extending in sequence keeps
        // issues in ascending `startAt` (i.e. rank) order.
        for page in pages {
            all_issues.extend(page.issues);
        }

        Ok(all_issues)
    }

    /// All issues in a board's backlog (on the board but not in an active
    /// sprint), in rank order.
    pub async fn fetch_board_backlog_issues(
        &self,
        board_id: u64,
        fields: &str,
    ) -> Result<Vec<Issue>> {
        self.fetch_agile_issues(&format!("board/{board_id}/backlog"), fields)
            .await
    }

    /// Re-rank issues directly before/after the anchor issue
    /// (`PUT /rest/agile/1.0/issue/rank`). Needs the *Schedule Issues*
    /// permission. `rank_field_id` disambiguates instances with multiple Rank
    /// fields; `None` uses Jira's default Rank field.
    pub async fn rank_issues(
        &self,
        keys: &[String],
        anchor: &RankAnchor,
        rank_field_id: Option<u64>,
    ) -> Result<()> {
        let url = format!("{}/rest/agile/1.0/issue/rank", self.base_url);
        let body = rank_payload(keys, anchor, rank_field_id);
        self.maybe_refresh().await?;
        let resp = self
            .apply_auth(self.client.put(&url))
            .await
            .json(&body)
            .send()
            .await
            .context("Failed to send rank request")?;

        let status = resp.status();
        // 207 Multi-Status means some issues failed to rank — a partial
        // success is still a failure for our single-issue moves.
        if !status.is_success() || status == reqwest::StatusCode::MULTI_STATUS {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Jira API error {status}: {body}");
        }
        Ok(())
    }

    /// Move issues from the backlog into a sprint
    /// (`POST /rest/agile/1.0/sprint/{id}/issue`).
    pub async fn move_issues_to_sprint(&self, sprint_id: u64, keys: &[String]) -> Result<()> {
        let url = format!("{}/rest/agile/1.0/sprint/{sprint_id}/issue", self.base_url);
        self.maybe_refresh().await?;
        let resp = self
            .apply_auth(self.client.post(&url))
            .await
            .json(&serde_json::json!({ "issues": keys }))
            .send()
            .await
            .context("Failed to send move-to-sprint request")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Jira API error {status}: {body}");
        }
        Ok(())
    }

    /// Fetch a single page of an Agile issue endpoint at the given `startAt`.
    async fn fetch_agile_page(
        &self,
        path: &str,
        start_at: u32,
        fields: &str,
    ) -> Result<AgileIssuesResponse> {
        let url = format!("{}/rest/agile/1.0/{path}", self.base_url);
        log::debug!("Agile issues request: {path} startAt={start_at} fields={fields}");

        self.maybe_refresh().await?;
        let resp = self
            .apply_auth(self.client.get(&url))
            .await
            .query(&[
                ("maxResults", &MAX_RESULTS.to_string()),
                ("startAt", &start_at.to_string()),
                ("fields", &fields.to_string()),
            ])
            .send()
            .await
            .context("Failed to send Agile issues request")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Jira API error {status}: {body}");
        }

        resp.json()
            .await
            .context("Failed to parse Agile issues response")
    }

    /// Swimlane definitions from Jira's internal `GreenHopper` API — the only
    /// place swimlane configuration exists (the public Agile API omits it).
    ///
    /// Unsupported, undocumented endpoint: it works with basic auth against
    /// the site URL but is not served through the OAuth API gateway, so this
    /// fails fast with an explanatory error under OAuth. May break without
    /// notice on Atlassian's side; callers must degrade gracefully.
    pub async fn get_greenhopper_swimlanes(
        &self,
        board_id: u64,
    ) -> Result<Vec<GreenHopperSwimlane>> {
        if matches!(&*self.auth.read().await, Auth::OAuth(_)) {
            anyhow::bail!(
                "swimlanes: \"auto\" needs basic auth (the board's lane config \
                 is only exposed by an internal Jira API that OAuth cannot reach); \
                 declare lanes explicitly in the source's `swimlanes` block"
            );
        }
        let url = format!(
            "{}/rest/greenhopper/1.0/xboard/work/allData.json",
            self.base_url
        );
        self.maybe_refresh().await?;
        let resp = self
            .apply_auth(self.client.get(&url))
            .await
            .query(&[("rapidViewId", &board_id.to_string())])
            .send()
            .await
            .context("Failed to fetch board swimlanes")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Jira API error {status}: {body}");
        }

        let data: GreenHopperBoardData = resp
            .json()
            .await
            .context("Failed to parse board swimlanes")?;
        Ok(data.into_swimlanes())
    }

    /// Fetch a single issue by key, with all fields (`fields=*all`) so the
    /// detail view and a lazy detail-load get description, comments, and every
    /// custom field.
    pub async fn get_issue(&self, key: &str) -> Result<Issue> {
        let url = format!("{}/rest/api/3/issue/{key}", self.base_url);
        self.maybe_refresh().await?;
        let resp = self
            .apply_auth(self.client.get(&url))
            .await
            .query(&[("fields", "*all")])
            .send()
            .await
            .context("Failed to fetch issue")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Jira API error {status}: {body}");
        }

        resp.json().await.context("Failed to parse issue response")
    }

    /// Get available transitions for an issue.
    pub async fn get_transitions(&self, key: &str) -> Result<Vec<Transition>> {
        let url = format!("{}/rest/api/3/issue/{key}/transitions", self.base_url);
        self.maybe_refresh().await?;
        let resp = self
            .apply_auth(self.client.get(&url))
            .await
            .send()
            .await
            .context("Failed to fetch transitions")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Jira API error {status}: {body}");
        }

        let tr: TransitionsResponse = resp.json().await.context("Failed to parse transitions")?;
        Ok(tr.transitions)
    }

    /// Apply a transition to an issue.
    pub async fn post_transition(&self, key: &str, transition_id: &str) -> Result<()> {
        let url = format!("{}/rest/api/3/issue/{key}/transitions", self.base_url);
        let body = json!({ "transition": { "id": transition_id } });
        self.maybe_refresh().await?;
        let resp = self
            .apply_auth(self.client.post(&url))
            .await
            .json(&body)
            .send()
            .await
            .context("Failed to post transition")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Transition failed {status}: {body}");
        }
        Ok(())
    }

    /// Post a comment on an issue.
    pub async fn post_comment(&self, key: &str, body_text: &str) -> Result<Comment> {
        let url = format!("{}/rest/api/3/issue/{key}/comment", self.base_url);
        let body = json!({ "body": crate::jira::adf::markdown_to_adf(body_text) });
        self.maybe_refresh().await?;
        let resp = self
            .apply_auth(self.client.post(&url))
            .await
            .json(&body)
            .send()
            .await
            .context("Failed to post comment")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Post comment failed {status}: {body}");
        }

        resp.json()
            .await
            .context("Failed to parse comment response")
    }

    /// Upload a file as an attachment to an issue.
    pub async fn upload_attachment(
        &self,
        issue_key: &str,
        file_path: &std::path::Path,
    ) -> Result<Vec<Attachment>> {
        let url = format!("{}/rest/api/3/issue/{issue_key}/attachments", self.base_url);
        let filename = file_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let bytes = tokio::fs::read(file_path)
            .await
            .context("Failed to read file for upload")?;
        let part = reqwest::multipart::Part::bytes(bytes).file_name(filename);
        let form = reqwest::multipart::Form::new().part("file", part);
        self.maybe_refresh().await?;
        let resp = self
            .apply_auth(self.client.post(&url))
            .await
            .header("X-Atlassian-Token", "no-check")
            .multipart(form)
            .send()
            .await
            .context("Failed to upload attachment")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Upload attachment failed {status}: {body}");
        }

        resp.json()
            .await
            .context("Failed to parse upload attachment response")
    }

    /// Assign an issue to the given account ID.
    #[allow(dead_code)]
    pub async fn set_assignee(&self, key: &str, account_id: &str) -> Result<()> {
        let url = format!("{}/rest/api/3/issue/{key}/assignee", self.base_url);
        let body = json!({ "accountId": account_id });
        self.maybe_refresh().await?;
        let resp = self
            .apply_auth(self.client.put(&url))
            .await
            .json(&body)
            .send()
            .await
            .context("Failed to set assignee")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Set assignee failed {status}: {body}");
        }
        Ok(())
    }

    /// Update a single field on an issue.
    #[allow(dead_code)]
    pub async fn update_field(
        &self,
        key: &str,
        field_id: &str,
        value: serde_json::Value,
    ) -> Result<()> {
        let url = format!("{}/rest/api/3/issue/{key}", self.base_url);
        let body = json!({ "fields": { field_id: value } });
        self.maybe_refresh().await?;
        let resp = self
            .apply_auth(self.client.put(&url))
            .await
            .json(&body)
            .send()
            .await
            .context("Failed to update field")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Update field failed {status}: {body}");
        }
        Ok(())
    }

    /// Move an issue to a different project by updating its project field.
    #[allow(dead_code)]
    pub async fn move_issue(&self, key: &str, target_project_key: &str) -> Result<()> {
        let url = format!("{}/rest/api/3/issue/{key}", self.base_url);
        let body = json!({
            "fields": {
                "project": { "key": target_project_key }
            }
        });
        self.maybe_refresh().await?;
        let resp = self
            .apply_auth(self.client.put(&url))
            .await
            .json(&body)
            .send()
            .await
            .context("Failed to move issue")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Move issue failed {status}: {body}");
        }
        Ok(())
    }

    /// Fetch all pages of a paginated createmeta endpoint, returning the
    /// concatenated array found under the first present key in `array_keys`.
    /// Mirrors `fetch_jql`'s pagination (`startAt` / `isLast`).
    async fn fetch_createmeta_pages(
        &self,
        url_path: &str,
        array_keys: &[&str],
    ) -> Result<Vec<serde_json::Value>> {
        let mut out = Vec::new();
        let mut start_at = 0u32;
        loop {
            let url = format!("{}{url_path}", self.base_url);
            self.maybe_refresh().await?;
            let resp = self
                .apply_auth(self.client.get(&url))
                .await
                .query(&[
                    ("maxResults", MAX_RESULTS.to_string()),
                    ("startAt", start_at.to_string()),
                ])
                .send()
                .await
                .context("Failed to fetch createmeta")?;

            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                anyhow::bail!("Jira API error {status}: {body}");
            }

            let body: serde_json::Value = resp
                .json()
                .await
                .context("Failed to parse createmeta response")?;
            let page = array_keys
                .iter()
                .find_map(|k| body.get(*k))
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let fetched = u32::try_from(page.len()).unwrap_or(0);
            let is_last = body.get("isLast").and_then(serde_json::Value::as_bool);
            out.extend(page);

            // Stop when the server says it's the last page, when a page comes
            // back empty, or when `isLast` is absent but the page was short.
            if is_last == Some(true) || fetched == 0 || (is_last.is_none() && fetched < MAX_RESULTS)
            {
                break;
            }
            start_at += fetched;
        }
        Ok(out)
    }

    /// Get the issue types creatable in a project (new split createmeta endpoint).
    pub async fn get_create_issuetypes(&self, project: &str) -> Result<Vec<IssueTypeField>> {
        let path = format!("/rest/api/3/issue/createmeta/{project}/issuetypes");
        let values = self
            .fetch_createmeta_pages(&path, &["values", "issueTypes"])
            .await?;
        let types = values
            .iter()
            .filter_map(|v| {
                let id = v.get("id").and_then(serde_json::Value::as_str)?.to_string();
                let name = v
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string();
                Some(IssueTypeField { id, name })
            })
            .collect();
        Ok(types)
    }

    /// Get the field metadata for a project + issue type (new split createmeta
    /// endpoint). Returns the raw field descriptors so the caller can map them.
    pub async fn get_create_fields(
        &self,
        project: &str,
        issuetype_id: &str,
    ) -> Result<Vec<serde_json::Value>> {
        let path = format!("/rest/api/3/issue/createmeta/{project}/issuetypes/{issuetype_id}");
        self.fetch_createmeta_pages(&path, &["values", "fields"])
            .await
    }

    /// Create a new issue. `payload` must be the full `{ "fields": { … } }`
    /// object. Returns the new issue's key.
    pub async fn create_issue(&self, payload: serde_json::Value) -> Result<String> {
        let url = format!("{}/rest/api/3/issue", self.base_url);
        self.maybe_refresh().await?;
        let resp = self
            .apply_auth(self.client.post(&url))
            .await
            .json(&payload)
            .send()
            .await
            .context("Failed to create issue")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Create issue failed {status}: {body}");
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .context("Failed to parse create issue response")?;
        body.get("key")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("Create issue response had no key"))
    }

    /// Get the currently authenticated user's username/name.
    pub async fn current_user(&self) -> Result<String> {
        #[derive(serde::Deserialize)]
        struct MyselfResponse {
            name: Option<String>,
            #[serde(rename = "accountId")]
            account_id: Option<String>,
        }

        let url = format!("{}/rest/api/3/myself", self.base_url);
        self.maybe_refresh().await?;
        let resp = self
            .apply_auth(self.client.get(&url))
            .await
            .send()
            .await
            .context("Failed to fetch current user")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Fetch current user failed {status}: {body}");
        }

        let me: MyselfResponse = resp
            .json()
            .await
            .context("Failed to parse myself response")?;
        me.account_id
            .or(me.name)
            .ok_or_else(|| anyhow::anyhow!("Could not determine current user"))
    }

    /// Fetch all field definitions from this Jira instance.
    pub async fn get_all_fields(&self) -> Result<Vec<FieldMeta>> {
        let url = format!("{}/rest/api/3/field", self.base_url);
        self.maybe_refresh().await?;
        let resp = self
            .apply_auth(self.client.get(&url))
            .await
            .send()
            .await
            .context("Failed to fetch field definitions")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Jira API error {status}: {body}");
        }

        resp.json()
            .await
            .context("Failed to parse field definitions")
    }

    /// Fetch a single issue with all fields (`fields=*all`).
    pub async fn get_issue_all_fields(&self, key: &str) -> Result<serde_json::Value> {
        let url = format!("{}/rest/api/3/issue/{key}", self.base_url);
        self.maybe_refresh().await?;
        let resp = self
            .apply_auth(self.client.get(&url))
            .await
            .query(&[("fields", "*all")])
            .send()
            .await
            .context("Failed to fetch issue")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Jira API error {status}: {body}");
        }

        resp.json().await.context("Failed to parse issue response")
    }

    /// Fetch allowed values for a field via `GET /rest/api/3/issue/{key}/editmeta`.
    pub async fn get_field_options(
        &self,
        issue_key: &str,
        field_id: &str,
    ) -> Result<Vec<crate::jira::types::FieldOption>> {
        let url = format!("{}/rest/api/3/issue/{issue_key}/editmeta", self.base_url);
        self.maybe_refresh().await?;
        let resp = self
            .apply_auth(self.client.get(&url))
            .await
            .send()
            .await
            .context("Failed to fetch editmeta")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Jira API error {status}: {body}");
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .context("Failed to parse editmeta response")?;

        let pointer = format!("/fields/{field_id}/allowedValues");
        let allowed = body
            .pointer(&pointer)
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let options = allowed
            .into_iter()
            .filter_map(|item| {
                let value = item
                    .get("value")
                    .or_else(|| item.get("name"))
                    .and_then(|v| v.as_str())?
                    .to_string();
                Some(crate::jira::types::FieldOption { value })
            })
            .collect();

        Ok(options)
    }

    /// Fetch the raw editmeta JSON object for a single field.
    /// Useful for inspecting what keys Jira actually returns (e.g. to find where hint text lives).
    pub async fn get_editmeta_field_raw(
        &self,
        issue_key: &str,
        field_id: &str,
    ) -> Result<serde_json::Value> {
        let url = format!("{}/rest/api/3/issue/{issue_key}/editmeta", self.base_url);
        self.maybe_refresh().await?;
        let resp = self
            .apply_auth(self.client.get(&url))
            .await
            .send()
            .await
            .context("Failed to fetch editmeta")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Jira API error {status}: {body}");
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .context("Failed to parse editmeta response")?;

        body.pointer(&format!("/fields/{field_id}"))
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Field '{field_id}' not found in editmeta"))
    }

    /// Fetch display names and schemas for a set of field IDs via
    /// `GET /rest/api/3/issue/{key}/editmeta`.
    /// Returns `(names, schemas)` where both are `field_id → value`.
    /// Unknown fields are silently omitted.
    pub async fn get_field_labels(
        &self,
        issue_key: &str,
        field_ids: &[&str],
    ) -> Result<(HashMap<String, String>, HashMap<String, FieldSchema>)> {
        let url = format!("{}/rest/api/3/issue/{issue_key}/editmeta", self.base_url);
        self.maybe_refresh().await?;
        let resp = self
            .apply_auth(self.client.get(&url))
            .await
            .send()
            .await
            .context("Failed to fetch editmeta")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Jira API error {status}: {body}");
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .context("Failed to parse editmeta response")?;

        let mut names = HashMap::new();
        let mut schemas = HashMap::new();
        for field_id in field_ids {
            let name_ptr = format!("/fields/{field_id}/name");
            if let Some(name) = body.pointer(&name_ptr).and_then(|v| v.as_str()) {
                names.insert((*field_id).to_string(), name.to_string());
            }
            let schema_ptr = format!("/fields/{field_id}/schema");
            if let Some(schema_val) = body.pointer(&schema_ptr) {
                let str_at = |k: &str| {
                    schema_val
                        .get(k)
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                };
                if let Some(ty) = str_at("type") {
                    schemas.insert(
                        (*field_id).to_string(),
                        FieldSchema {
                            ty,
                            custom: str_at("custom"),
                            system: str_at("system"),
                        },
                    );
                }
            }
        }
        Ok((names, schemas))
    }

    /// Update the body of an existing comment.
    pub async fn update_comment(
        &self,
        issue_key: &str,
        comment_id: &str,
        new_body: &str,
    ) -> Result<Comment> {
        let url = format!(
            "{}/rest/api/3/issue/{issue_key}/comment/{comment_id}",
            self.base_url
        );
        let body = serde_json::json!({ "body": crate::jira::adf::markdown_to_adf(new_body) });
        self.maybe_refresh().await?;
        let resp = self
            .apply_auth(self.client.put(&url))
            .await
            .json(&body)
            .send()
            .await
            .context("Failed to update comment")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Update comment failed {status}: {body}");
        }
        resp.json().await.context("Failed to parse updated comment")
    }

    /// Delete a comment.
    pub async fn delete_comment(&self, issue_key: &str, comment_id: &str) -> Result<()> {
        let url = format!(
            "{}/rest/api/3/issue/{issue_key}/comment/{comment_id}",
            self.base_url
        );
        self.maybe_refresh().await?;
        let resp = self
            .apply_auth(self.client.delete(&url))
            .await
            .send()
            .await
            .context("Failed to delete comment")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Delete comment failed {status}: {body}");
        }
        Ok(())
    }

    /// Delete an attachment by its ID.
    pub async fn delete_attachment(&self, attachment_id: &str) -> Result<()> {
        let url = format!("{}/rest/api/3/attachment/{attachment_id}", self.base_url);
        self.maybe_refresh().await?;
        let resp = self
            .apply_auth(self.client.delete(&url))
            .await
            .send()
            .await
            .context("Failed to delete attachment")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Delete attachment failed {status}: {body}");
        }
        Ok(())
    }

    /// Download the raw bytes of an attachment by its content URL.
    pub async fn download_attachment(&self, url: &str) -> Result<Vec<u8>> {
        self.maybe_refresh().await?;
        let resp = self
            .apply_auth(self.client.get(url))
            .await
            .send()
            .await
            .context("Failed to download attachment")?;
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!(
                "Failed to download {status}: {}",
                resp.text().await.unwrap_or_default()
            );
        }
        Ok(resp.bytes().await?.to_vec())
    }

    #[allow(dead_code)]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Fetch all statuses configured on this Jira instance via
    /// `GET /rest/api/3/status`. Returns the distinct status names alongside
    /// their `statusCategory.key` (used to surface terminal statuses).
    pub async fn get_all_statuses(&self) -> Result<Vec<StatusInfo>> {
        #[derive(serde::Deserialize)]
        struct RawStatus {
            name: String,
            #[serde(rename = "statusCategory")]
            status_category: Option<RawCategory>,
        }
        #[derive(serde::Deserialize)]
        struct RawCategory {
            key: String,
        }

        let url = format!("{}/rest/api/3/status", self.base_url);
        self.maybe_refresh().await?;
        let resp = self
            .apply_auth(self.client.get(&url))
            .await
            .send()
            .await
            .context("Failed to fetch all statuses")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Jira API error {status}: {body}");
        }

        let raw: Vec<RawStatus> = resp.json().await.context("Failed to parse statuses")?;

        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut out: Vec<StatusInfo> = Vec::new();
        for s in raw {
            if seen.insert(s.name.clone()) {
                let category = s.status_category.map_or(StatusCategory::Undefined, |c| {
                    StatusCategory::from_key(&c.key)
                });
                out.push(StatusInfo {
                    name: s.name,
                    category,
                });
            }
        }
        Ok(out)
    }

    /// Fetch the distinct status names available for issues in a project via
    /// `GET /rest/api/3/project/{key}/statuses`. Jira returns statuses grouped
    /// by issue type; we flatten and dedupe by name.
    pub async fn get_project_statuses(&self, project_key: &str) -> Result<Vec<String>> {
        #[derive(serde::Deserialize)]
        struct IssueTypeStatuses {
            statuses: Vec<NamedStatus>,
        }
        #[derive(serde::Deserialize)]
        struct NamedStatus {
            name: String,
        }

        let url = format!(
            "{}/rest/api/3/project/{project_key}/statuses",
            self.base_url
        );
        self.maybe_refresh().await?;
        let resp = self
            .apply_auth(self.client.get(&url))
            .await
            .send()
            .await
            .context("Failed to fetch project statuses")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Jira API error {status}: {body}");
        }

        let groups: Vec<IssueTypeStatuses> = resp
            .json()
            .await
            .context("Failed to parse project statuses")?;

        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut out: Vec<String> = Vec::new();
        for g in groups {
            for s in g.statuses {
                if seen.insert(s.name.clone()) {
                    out.push(s.name);
                }
            }
        }
        Ok(out)
    }

    /// Fetch up to one page (100) of visible projects via
    /// `GET /rest/api/3/project/search`.
    pub async fn search_projects(&self) -> Result<Vec<ProjectInfo>> {
        #[derive(serde::Deserialize)]
        struct ProjectSearchResponse {
            values: Vec<RawProject>,
        }
        #[derive(serde::Deserialize)]
        struct RawProject {
            key: String,
            name: String,
        }

        let url = format!("{}/rest/api/3/project/search", self.base_url);
        self.maybe_refresh().await?;
        let resp = self
            .apply_auth(self.client.get(&url))
            .await
            .query(&[("maxResults", "100")])
            .send()
            .await
            .context("Failed to fetch projects")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Jira API error {status}: {body}");
        }

        let page: ProjectSearchResponse = resp
            .json()
            .await
            .context("Failed to parse project search response")?;

        Ok(page
            .values
            .into_iter()
            .map(|p| ProjectInfo {
                key: p.key,
                name: p.name,
            })
            .collect())
    }
}

/// `startAt` offsets for the Agile pages after the first, given the server's
/// effective page size (`stride`) and reported `total`. Empty when the first
/// page already covers everything or the board is empty (`stride == 0`).
fn agile_page_offsets(stride: u32, total: u32) -> Vec<u32> {
    if stride == 0 {
        return Vec::new();
    }
    (stride..total).step_by(stride as usize).collect()
}

/// Request body for `PUT /rest/agile/1.0/issue/rank`.
fn rank_payload(
    keys: &[String],
    anchor: &RankAnchor,
    rank_field_id: Option<u64>,
) -> serde_json::Value {
    let mut body = json!({ "issues": keys });
    match anchor {
        RankAnchor::Before(key) => body["rankBeforeIssue"] = json!(key),
        RankAnchor::After(key) => body["rankAfterIssue"] = json!(key),
    }
    if let Some(id) = rank_field_id {
        body["rankCustomFieldId"] = json!(id);
    }
    body
}

#[cfg(test)]
mod tests {
    use super::{agile_page_offsets, rank_payload};
    use crate::jira::types::RankAnchor;
    use serde_json::json;

    #[test]
    fn rank_payload_places_before_or_after_the_anchor() {
        let keys = vec!["PROJ-2".to_string()];
        assert_eq!(
            rank_payload(&keys, &RankAnchor::Before("PROJ-1".into()), None),
            json!({ "issues": ["PROJ-2"], "rankBeforeIssue": "PROJ-1" })
        );
        assert_eq!(
            rank_payload(&keys, &RankAnchor::After("PROJ-3".into()), None),
            json!({ "issues": ["PROJ-2"], "rankAfterIssue": "PROJ-3" })
        );
    }

    #[test]
    fn rank_payload_includes_rank_field_only_when_known() {
        let keys = vec!["PROJ-2".to_string()];
        let with_field = rank_payload(&keys, &RankAnchor::Before("PROJ-1".into()), Some(10019));
        assert_eq!(with_field["rankCustomFieldId"], json!(10019));
        let without = rank_payload(&keys, &RankAnchor::Before("PROJ-1".into()), None);
        assert!(without.get("rankCustomFieldId").is_none());
    }

    #[test]
    fn offsets_cover_all_pages_after_the_first_in_order() {
        // 250 issues, 100 per page → first page is startAt=0, then 100 and 200.
        assert_eq!(agile_page_offsets(100, 250), vec![100, 200]);
    }

    #[test]
    fn single_page_needs_no_extra_requests() {
        assert!(agile_page_offsets(100, 100).is_empty());
        assert!(agile_page_offsets(100, 42).is_empty());
    }

    #[test]
    fn stride_below_the_asked_page_size_is_honored() {
        // Jira capped the page at 50; offsets must step by 50, not 100.
        assert_eq!(agile_page_offsets(50, 120), vec![50, 100]);
    }

    #[test]
    fn empty_board_yields_no_offsets() {
        assert!(agile_page_offsets(0, 0).is_empty());
    }
}
