//! Thin HTTP client for the Grafana `OnCall` API, authenticated with a
//! personal `OnCall` API key (created in the user's Grafana IRM/`OnCall`
//! profile; the raw key goes in the `Authorization` header, no Bearer).

use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::Client;

use super::types::{OnCallUser, Schedule, SchedulesPage, find_schedule_by_name};

pub struct GrafanaClient {
    client: Client,
    base_url: String,
    token: String,
}

impl GrafanaClient {
    pub fn new(oncall_api_url: &str, token: String) -> Result<Self> {
        // Unlike the Jira/Confluence clients, this one gates TUI startup, so
        // a stuck connection must time out instead of blocking the launch.
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .context("Failed to build HTTP client")?;
        Ok(Self {
            client,
            base_url: oncall_api_url.trim_end_matches('/').to_string(),
            token,
        })
    }

    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        query: &[(&str, &str)],
    ) -> Result<T> {
        log::debug!("Grafana OnCall request: {url}");
        let resp = self
            .client
            .get(url)
            .query(query)
            .header(reqwest::header::AUTHORIZATION, &self.token)
            .send()
            .await
            .context("Failed to send Grafana OnCall request")?;
        let status = resp.status();
        log::debug!("Grafana OnCall response: HTTP {status}");
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            log::error!("Grafana OnCall API error {status}: {body}");
            anyhow::bail!("Grafana OnCall API error {status}: {body}");
        }
        resp.json()
            .await
            .context("Failed to parse Grafana OnCall response")
    }

    /// The user this personal API token belongs to.
    pub async fn current_user(&self) -> Result<OnCallUser> {
        let url = format!("{}/api/v1/users/current/", self.base_url);
        self.get_json(&url, &[]).await
    }

    /// Look a schedule up by exact name via the list endpoint's name filter.
    pub async fn schedule_by_name(&self, name: &str) -> Result<Option<Schedule>> {
        let url = format!("{}/api/v1/schedules/", self.base_url);
        let page: SchedulesPage = self.get_json(&url, &[("name", name)]).await?;
        Ok(find_schedule_by_name(page, name))
    }

    pub async fn schedule_by_id(&self, id: &str) -> Result<Schedule> {
        let url = format!("{}/api/v1/schedules/{id}/", self.base_url);
        self.get_json(&url, &[]).await
    }
}
