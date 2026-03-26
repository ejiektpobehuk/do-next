use std::collections::HashMap;

use anyhow::{Context, Result};
use reqwest::{Client, RequestBuilder};
use serde_json::json;

use crate::jira::auth::Credentials;
use crate::jira::types::{
    Attachment, Comment, FieldMeta, Issue, SearchResponse, Transition, TransitionsResponse,
};

const MAX_RESULTS: u32 = 100;

#[derive(Clone)]
pub struct JiraClient {
    client: Client,
    base_url: String,
    credentials: Credentials,
}

impl JiraClient {
    pub fn new(base_url: String, credentials: Credentials) -> Result<Self> {
        let client = Client::builder()
            .build()
            .context("Failed to build HTTP client")?;
        Ok(Self {
            client,
            base_url,
            credentials,
        })
    }

    fn apply_auth(&self, req: RequestBuilder) -> RequestBuilder {
        match &self.credentials {
            Credentials::Token(token) => req.bearer_auth(token),
            Credentials::Basic { username, password } => req.basic_auth(username, Some(password)),
        }
    }

    /// Fetch all issues matching a JQL query, paginating automatically.
    pub async fn fetch_jql(&self, jql: &str) -> Result<Vec<Issue>> {
        let mut all_issues = Vec::new();
        let mut start_at = 0u32;

        loop {
            let url = format!("{}/rest/api/2/search", self.base_url);
            log::debug!("JQL request: startAt={start_at} jql={jql}");
            let resp = self
                .apply_auth(self.client.get(&url))
                .query(&[
                    ("jql", jql),
                    ("maxResults", &MAX_RESULTS.to_string()),
                    ("startAt", &start_at.to_string()),
                    ("fields", "*all"),
                ])
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
            let fetched = u32::try_from(page.issues.len()).unwrap_or(0);
            all_issues.extend(page.issues);

            if start_at + fetched >= page.total || fetched == 0 {
                break;
            }
            start_at += fetched;
        }

        Ok(all_issues)
    }

    /// Fetch a single issue by key.
    pub async fn get_issue(&self, key: &str) -> Result<Issue> {
        let url = format!("{}/rest/api/2/issue/{key}", self.base_url);
        let resp = self
            .apply_auth(self.client.get(&url))
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
        let url = format!("{}/rest/api/2/issue/{key}/transitions", self.base_url);
        let resp = self
            .apply_auth(self.client.get(&url))
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
        let url = format!("{}/rest/api/2/issue/{key}/transitions", self.base_url);
        let body = json!({ "transition": { "id": transition_id } });
        let resp = self
            .apply_auth(self.client.post(&url))
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
        let url = format!("{}/rest/api/2/issue/{key}/comment", self.base_url);
        let body = json!({ "body": body_text });
        let resp = self
            .apply_auth(self.client.post(&url))
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
        let url = format!("{}/rest/api/2/issue/{issue_key}/attachments", self.base_url);
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
        let resp = self
            .apply_auth(self.client.post(&url))
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

    /// Assign an issue to the given username (use "`currentUser()`" or actual username).
    #[allow(dead_code)]
    pub async fn set_assignee(&self, key: &str, username: &str) -> Result<()> {
        let url = format!("{}/rest/api/2/issue/{key}/assignee", self.base_url);
        let body = json!({ "name": username });
        let resp = self
            .apply_auth(self.client.put(&url))
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
        let url = format!("{}/rest/api/2/issue/{key}", self.base_url);
        let body = json!({ "fields": { field_id: value } });
        let resp = self
            .apply_auth(self.client.put(&url))
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
        let url = format!("{}/rest/api/2/issue/{key}", self.base_url);
        let body = json!({
            "fields": {
                "project": { "key": target_project_key }
            }
        });
        let resp = self
            .apply_auth(self.client.put(&url))
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

    /// Get the currently authenticated user's username/name.
    pub async fn current_user(&self) -> Result<String> {
        #[derive(serde::Deserialize)]
        struct MyselfResponse {
            name: Option<String>,
            #[serde(rename = "accountId")]
            account_id: Option<String>,
        }

        let url = format!("{}/rest/api/2/myself", self.base_url);
        let resp = self
            .apply_auth(self.client.get(&url))
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
        me.name
            .or(me.account_id)
            .ok_or_else(|| anyhow::anyhow!("Could not determine current user"))
    }

    /// Fetch all field definitions from this Jira instance.
    pub async fn get_all_fields(&self) -> Result<Vec<FieldMeta>> {
        let url = format!("{}/rest/api/2/field", self.base_url);
        let resp = self
            .apply_auth(self.client.get(&url))
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
        let url = format!("{}/rest/api/2/issue/{key}", self.base_url);
        let resp = self
            .apply_auth(self.client.get(&url))
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

    /// Fetch allowed values for a field via `GET /rest/api/2/issue/{key}/editmeta`.
    pub async fn get_field_options(
        &self,
        issue_key: &str,
        field_id: &str,
    ) -> Result<Vec<crate::jira::types::FieldOption>> {
        let url = format!("{}/rest/api/2/issue/{issue_key}/editmeta", self.base_url);
        let resp = self
            .apply_auth(self.client.get(&url))
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
        let url = format!("{}/rest/api/2/issue/{issue_key}/editmeta", self.base_url);
        let resp = self
            .apply_auth(self.client.get(&url))
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

    /// Fetch display names and schema types for a set of field IDs via
    /// `GET /rest/api/2/issue/{key}/editmeta`.
    /// Returns `(names, schemas)` where both are `field_id → value`.
    /// Unknown fields are silently omitted.
    pub async fn get_field_labels(
        &self,
        issue_key: &str,
        field_ids: &[&str],
    ) -> Result<(HashMap<String, String>, HashMap<String, String>)> {
        let url = format!("{}/rest/api/2/issue/{issue_key}/editmeta", self.base_url);
        let resp = self
            .apply_auth(self.client.get(&url))
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
            let schema_ptr = format!("/fields/{field_id}/schema/type");
            if let Some(schema_type) = body.pointer(&schema_ptr).and_then(|v| v.as_str()) {
                schemas.insert((*field_id).to_string(), schema_type.to_string());
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
            "{}/rest/api/2/issue/{issue_key}/comment/{comment_id}",
            self.base_url
        );
        let body = serde_json::json!({ "body": new_body });
        let resp = self
            .apply_auth(self.client.put(&url))
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
            "{}/rest/api/2/issue/{issue_key}/comment/{comment_id}",
            self.base_url
        );
        let resp = self
            .apply_auth(self.client.delete(&url))
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
        let url = format!("{}/rest/api/2/attachment/{attachment_id}", self.base_url);
        let resp = self
            .apply_auth(self.client.delete(&url))
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
        let resp = self
            .apply_auth(self.client.get(url))
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
}
