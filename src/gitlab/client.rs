//! HTTP client for the GitLab REST API (`/api/v4`), authenticated with a
//! personal access token in the `PRIVATE-TOKEN` header.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::Client;
use tokio::sync::RwLock;

use crate::config::types::GitlabFilters;

use super::types::{
    ApiApprovals, ApiEvent, ApiMergeRequest, ApiMergeRequestDetail, ApiProject, ApiUser,
    MergeRequest, apply_enriched_fields, build_mr_query, build_standup_mr_query, encode_path,
    needs_current_user, to_display,
};

/// Page size for list endpoints (GitLab's maximum).
const PER_PAGE: usize = 100;

/// Give up on a self-hosted instance we cannot even reach — usually a VPN
/// that is off — instead of leaving the source spinning.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Ceiling on a whole request; generous, since a large MR list is legitimately
/// slow, but bounded so a stalled connection surfaces as an error.
const REQUEST_TIMEOUT: Duration = Duration::from_mins(1);

/// Hard cap on pages followed per list endpoint. A "what should I do next"
/// queue never runs this long; the cap is the runaway-paging guard.
const MAX_PAGES: u32 = 20;

/// Max concurrent enrichment lookups. Matches the board preload bound so a
/// large queue can't burst on the instance.
const ENRICH_CONCURRENCY: usize = 8;

/// Decide which page to fetch next, or `None` to stop.
///
/// Pure so the runaway-paging guard is testable: a server that omits, garbles
/// or keeps echoing `x-next-page` must terminate the loop rather than spin
/// (the failure mode commit 3c7b941 fixed for JQL paging).
fn next_page(current: u32, header: Option<&str>, batch_len: usize) -> Option<u32> {
    let next: u32 = header?.trim().parse().ok()?;
    // Never stand still or go backwards.
    if next <= current {
        return None;
    }
    // A short page is the last one whatever the header claims.
    if batch_len < PER_PAGE {
        return None;
    }
    Some(next)
}

/// GitLab API client. Cloneable: the underlying `reqwest::Client` pools
/// connections and the cached username is shared.
#[derive(Clone)]
pub struct GitlabClient {
    client: Client,
    /// Instance base URL without a trailing slash, e.g. `https://gitlab.com`.
    base_url: String,
    token: String,
    /// Cached username of the token's own user.
    me: Arc<RwLock<Option<String>>>,
}

impl GitlabClient {
    pub fn new(base_url: &str, token: String) -> Result<Self> {
        // Self-hosted instances are commonly VPN-only; with the VPN off the
        // connect never answers, and without a deadline the merge-request
        // sources would spin for the whole session instead of showing an
        // error row the user can act on.
        let client = Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .context("Failed to build HTTP client")?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_owned(),
            token,
            me: Arc::new(RwLock::new(None)),
        })
    }

    fn api(&self, path: &str) -> String {
        format!("{}/api/v4{path}", self.base_url)
    }

    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        query: &[(String, String)],
    ) -> Result<T> {
        log::debug!("GitLab request: {url}");
        let resp = self
            .client
            .get(url)
            .query(query)
            .header("PRIVATE-TOKEN", &self.token)
            .send()
            .await
            .with_context(|| format!("Failed to send GitLab request: {url}"))?;
        crate::http::json_response("GitLab", url, resp).await
    }

    /// Follow a paginated list endpoint via the `x-next-page` response header.
    ///
    /// Stops at [`MAX_PAGES`] (with a warning) and whenever the header fails to
    /// advance the page number — a server that keeps echoing the same page
    /// would otherwise loop forever.
    async fn get_paginated<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        query: &[(String, String)],
    ) -> Result<Vec<T>> {
        let mut out: Vec<T> = Vec::new();
        let mut page: u32 = 1;
        loop {
            let mut paged = query.to_vec();
            paged.push(("per_page".into(), PER_PAGE.to_string()));
            paged.push(("page".into(), page.to_string()));

            log::debug!("GitLab request: {url} (page {page})");
            let resp = self
                .client
                .get(url)
                .query(&paged)
                .header("PRIVATE-TOKEN", &self.token)
                .send()
                .await
                .with_context(|| format!("Failed to send GitLab request: {url}"))?;
            // Read the paging header before the body is consumed.
            let header = resp
                .headers()
                .get("x-next-page")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let batch: Vec<T> = crate::http::json_response("GitLab", url, resp).await?;
            let batch_len = batch.len();
            out.extend(batch);

            let Some(next) = next_page(page, header.as_deref(), batch_len) else {
                break;
            };
            if next > MAX_PAGES {
                log::warn!(
                    "GitLab paging stopped at the {MAX_PAGES}-page cap for {url} \
                     ({} items so far); narrow the source's filters",
                    out.len()
                );
                break;
            }
            page = next;
        }
        Ok(out)
    }

    /// The user this token belongs to.
    pub async fn current_user(&self) -> Result<ApiUser> {
        self.get_json(&self.api("/user"), &[]).await
    }

    /// Username of the token's own user (cached).
    pub async fn current_username(&self) -> Result<String> {
        // Bound the read guard to its own statement so it drops before the
        // write below — holding it across the fetch would deadlock.
        let cached = self.me.read().await.clone();
        if let Some(username) = cached {
            return Ok(username);
        }
        let user = self.current_user().await?;
        log::debug!(
            "GitLab current user: id={} username={}",
            user.id,
            user.username
        );
        let username = user.username;
        *self.me.write().await = Some(username.clone());
        Ok(username)
    }

    /// Fetch all merge requests matching the source filters, with approval and
    /// pipeline state enriched in.
    pub async fn fetch_merge_requests(&self, filters: &GitlabFilters) -> Result<Vec<MergeRequest>> {
        let me = if needs_current_user(filters) {
            Some(self.current_username().await?)
        } else {
            None
        };
        let query = build_mr_query(filters, me.as_deref());

        // Endpoint set: the instance-wide list when nothing narrows the scope,
        // otherwise one list per configured project and group.
        let mut urls: Vec<String> = Vec::new();
        for project in &filters.projects {
            urls.push(self.api(&format!(
                "/projects/{}/merge_requests",
                encode_path(project)
            )));
        }
        for group in &filters.groups {
            urls.push(self.api(&format!("/groups/{}/merge_requests", encode_path(group))));
        }
        if urls.is_empty() {
            urls.push(self.api("/merge_requests"));
        }

        // Every endpoint is independent; `try_join_all` preserves input order,
        // which the first-wins dedup below relies on.
        let per_endpoint: Vec<Vec<ApiMergeRequest>> = futures::future::try_join_all(
            urls.iter()
                .map(|url| self.get_paginated::<ApiMergeRequest>(url, &query)),
        )
        .await?;

        let mut seen: HashSet<(u64, u64)> = HashSet::new();
        let mut out: Vec<MergeRequest> = Vec::new();
        for batch in per_endpoint {
            for api_mr in batch {
                if seen.insert((api_mr.project_id, api_mr.iid)) {
                    out.push(to_display(api_mr));
                }
            }
        }
        log::debug!(
            "GitLab merge requests fetched: {} (from {} endpoint(s))",
            out.len(),
            urls.len()
        );

        self.enrich(&mut out).await;
        Ok(out)
    }

    /// Merge requests you authored that changed since `since`.
    ///
    /// Deliberately *not* enriched: a standup timeline shows what you did, not
    /// approval or CI state, so skipping enrichment saves the 2N lookups
    /// [`Self::enrich`] would cost.
    ///
    /// `updated_after` is precise to the second — the events feed, by contrast,
    /// only takes whole dates — so nothing needs widening and filtering here.
    pub async fn fetch_my_merge_requests_since(
        &self,
        since: chrono::DateTime<chrono::Utc>,
        projects: &[String],
        groups: &[String],
    ) -> Result<Vec<MergeRequest>> {
        let me = self.current_username().await?;
        let query = build_standup_mr_query(&me, since);
        log::debug!("GitLab standup MR query: {query:?}");

        let mut urls: Vec<String> = Vec::new();
        for project in projects {
            urls.push(self.api(&format!(
                "/projects/{}/merge_requests",
                encode_path(project)
            )));
        }
        for group in groups {
            urls.push(self.api(&format!("/groups/{}/merge_requests", encode_path(group))));
        }
        if urls.is_empty() {
            urls.push(self.api("/merge_requests"));
        }

        let per_endpoint: Vec<Vec<ApiMergeRequest>> = futures::future::try_join_all(
            urls.iter()
                .map(|url| self.get_paginated::<ApiMergeRequest>(url, &query)),
        )
        .await?;

        let mut seen: HashSet<(u64, u64)> = HashSet::new();
        let mut out: Vec<MergeRequest> = Vec::new();
        for batch in per_endpoint {
            for api_mr in batch {
                if seen.insert((api_mr.project_id, api_mr.iid)) {
                    out.push(to_display(api_mr));
                }
            }
        }
        log::debug!("GitLab standup merge requests: {}", out.len());
        Ok(out)
    }

    /// The authenticated user's contribution events between two dates.
    ///
    /// `after`/`before` are whole dates and *exclusive*, so callers pass
    /// day-widened bounds and filter on `created_at` themselves. Opt-in because
    /// the documented scope is `read_user`/`api` while onboarding only asks for
    /// `read_api` — a 403 here must degrade, not fail the standup.
    pub async fn fetch_events_between(
        &self,
        after: chrono::NaiveDate,
        before: chrono::NaiveDate,
    ) -> Result<Vec<ApiEvent>> {
        let query = vec![
            ("after".into(), after.format("%Y-%m-%d").to_string()),
            ("before".into(), before.format("%Y-%m-%d").to_string()),
        ];
        self.get_paginated::<ApiEvent>(&self.api("/events"), &query)
            .await
    }

    /// Projects owned by the token's user, newest first. One request; callers
    /// filter to the window themselves.
    pub async fn fetch_owned_projects(&self) -> Result<Vec<ApiProject>> {
        let query = vec![
            ("owned".into(), "true".into()),
            ("order_by".into(), "created_at".into()),
            ("sort".into(), "desc".into()),
            // Without this GitLab computes counts and permissions we never read.
            ("simple".into(), "true".into()),
        ];
        self.get_paginated::<ApiProject>(&self.api("/projects"), &query)
            .await
    }

    /// Fill in approval state and head-pipeline status, which the list
    /// endpoints omit. Best-effort: a failed lookup logs a warning and leaves
    /// the fields `None` — it never fails the source.
    async fn enrich(&self, mrs: &mut [MergeRequest]) {
        use futures::stream::StreamExt;

        if mrs.is_empty() {
            return;
        }
        log::debug!("GitLab enrichment: {} merge request(s)", mrs.len());
        let targets: Vec<(usize, u64, u64)> = mrs
            .iter()
            .enumerate()
            .map(|(idx, mr)| (idx, mr.project_id, mr.iid))
            .collect();

        // `buffer_unordered` rather than `for_each_concurrent`: results are
        // collected and applied afterwards, so no shared mutable borrow of
        // `mrs` crosses an await point.
        let results: Vec<(usize, Option<ApiApprovals>, Option<String>)> =
            futures::stream::iter(targets)
                .map(|(idx, project_id, iid)| async move {
                    let approvals = self.fetch_approvals(project_id, iid).await;
                    let ci = self.fetch_ci_status(project_id, iid).await;
                    (idx, approvals, ci)
                })
                .buffer_unordered(ENRICH_CONCURRENCY)
                .collect()
                .await;

        for (idx, approvals, ci) in results {
            let Some(mr) = mrs.get_mut(idx) else { continue };
            if let Some(approvals) = approvals {
                mr.approvals_required = approvals.approvals_required;
                mr.approvals_left = approvals.approvals_left;
                mr.approved_by = approvals
                    .approved_by
                    .iter()
                    .map(|a| a.user.display().to_owned())
                    .collect();
            }
            mr.ci_status = ci;
            apply_enriched_fields(mr);
        }
    }

    async fn fetch_approvals(&self, project_id: u64, iid: u64) -> Option<ApiApprovals> {
        let url = self.api(&format!(
            "/projects/{project_id}/merge_requests/{iid}/approvals"
        ));
        match self.get_json::<ApiApprovals>(&url, &[]).await {
            Ok(approvals) => Some(approvals),
            Err(e) => {
                log::warn!("GitLab approvals lookup failed for !{iid}: {e:#}");
                None
            }
        }
    }

    /// Head-pipeline status. Only the single-MR endpoint carries it.
    async fn fetch_ci_status(&self, project_id: u64, iid: u64) -> Option<String> {
        let url = self.api(&format!("/projects/{project_id}/merge_requests/{iid}"));
        match self.get_json::<ApiMergeRequestDetail>(&url, &[]).await {
            Ok(detail) => detail.head_pipeline.and_then(|p| p.status),
            Err(e) => {
                log::warn!("GitLab pipeline lookup failed for !{iid}: {e:#}");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_page_with_an_advancing_header_continues() {
        assert_eq!(next_page(1, Some("2"), PER_PAGE), Some(2));
        assert_eq!(next_page(7, Some("8"), PER_PAGE), Some(8));
    }

    #[test]
    fn the_last_page_stops_the_loop() {
        // GitLab sends an empty header on the last page...
        assert_eq!(next_page(3, Some(""), PER_PAGE), None);
        // ...or omits it entirely.
        assert_eq!(next_page(3, None, PER_PAGE), None);
        // A short page is the last one even if the header says otherwise.
        assert_eq!(next_page(1, Some("2"), PER_PAGE - 1), None);
        assert_eq!(next_page(1, Some("2"), 0), None);
    }

    #[test]
    fn a_non_advancing_or_garbled_header_stops_the_loop() {
        // The runaway case: the same page echoed forever.
        assert_eq!(next_page(2, Some("2"), PER_PAGE), None);
        // Backwards is just as bad.
        assert_eq!(next_page(5, Some("3"), PER_PAGE), None);
        // Anything unparseable stops rather than guessing.
        for garbage in ["", "abc", "-1", "2.5", "9999999999999"] {
            assert_eq!(
                next_page(1, Some(garbage), PER_PAGE),
                None,
                "header {garbage:?} must stop paging"
            );
        }
    }

    #[test]
    fn the_page_cap_is_reachable_but_bounded() {
        // The cap is enforced by the caller; this pins the value it guards so
        // a page-size change can't silently blow the ceiling up.
        assert_eq!(MAX_PAGES, 20);
        assert_eq!(PER_PAGE * MAX_PAGES as usize, 2000);
        // Paging itself keeps advancing right up to the cap...
        assert_eq!(next_page(MAX_PAGES, Some("21"), PER_PAGE), Some(21));
        // ...and 21 > MAX_PAGES is what makes the caller stop and warn.
        assert!(21 > MAX_PAGES);
    }
}
