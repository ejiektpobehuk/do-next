//! Grafana IRM (`OnCall`) integration: at startup, check whether the current
//! user is on call for each team's configured schedule, and if so switch the
//! team to its on-duty source set (see [`ResolvedTeam::set_on_duty`]; the
//! `D` key toggles it manually at runtime). Every failure is non-fatal —
//! the team keeps its normal sources and the error surfaces as a load
//! warning.

mod client;
pub mod types;

use anyhow::{Context, Result, anyhow};

use crate::config::credentials::resolve_grafana_token;
use crate::config::types::{ResolvedGrafana, ResolvedTeam, ScheduleSelector};
use client::GrafanaClient;
use types::{OnCallUser, is_on_duty};

/// Check a token against the `OnCall` API and return the user it belongs to.
/// Used by the interactive token setup to catch typos before storing.
pub async fn validate_token(oncall_api_url: &str, token: String) -> Result<OnCallUser> {
    GrafanaClient::new(oncall_api_url, token)?
        .current_user()
        .await
        .context("token check against the OnCall API failed")
}

/// One token-setup target: the teams behind a single `OnCall` API URL (one
/// token covers every team on the same stack).
pub struct TokenSetupTarget {
    pub oncall_api_url: String,
    /// Grafana web UI URL for "create your token here" guidance, when any
    /// team in the group configures one.
    pub instance_url: Option<String>,
    pub team_ids: Vec<String>,
}

/// Teams that use the on-call view, grouped by `OnCall` API URL. Order
/// follows the team list.
pub fn grafana_api_urls(teams: &[ResolvedTeam]) -> Vec<TokenSetupTarget> {
    let mut groups: Vec<TokenSetupTarget> = Vec::new();
    for team in teams {
        let Some(grafana) = &team.grafana else {
            continue;
        };
        match groups
            .iter_mut()
            .find(|g| g.oncall_api_url == grafana.oncall_api_url)
        {
            Some(group) => {
                group.team_ids.push(team.id.clone());
                if group.instance_url.is_none() {
                    group.instance_url.clone_from(&grafana.instance_url);
                }
            }
            None => groups.push(TokenSetupTarget {
                oncall_api_url: grafana.oncall_api_url.clone(),
                instance_url: grafana.instance_url.clone(),
                team_ids: vec![team.id.clone()],
            }),
        }
    }
    groups
}

/// Like [`grafana_api_urls`], but only teams whose token resolves to
/// "not configured anywhere" — the ones the startup setup offer targets.
/// Hard resolution failures (locked keyring, failing command) are excluded;
/// they surface as warnings from [`apply_on_duty_sources`] instead.
pub fn teams_missing_token(teams: &[ResolvedTeam]) -> Vec<TokenSetupTarget> {
    let missing: Vec<ResolvedTeam> = teams
        .iter()
        .filter(|t| {
            t.grafana
                .as_ref()
                .is_some_and(|g| matches!(resolve_grafana_token(g), Ok(None)))
        })
        .cloned()
        .collect();
    grafana_api_urls(&missing)
}

/// True when the token's user is currently on call for the configured
/// schedule.
async fn check_on_duty(cfg: &ResolvedGrafana, token: String) -> Result<bool> {
    let client = GrafanaClient::new(&cfg.oncall_api_url, token)?;
    let user = client
        .current_user()
        .await
        .context("fetching current OnCall user")?;
    log::debug!(
        "Grafana OnCall current user: id={} username={:?} email={:?}",
        user.id,
        user.username,
        user.email
    );
    let schedule = match &cfg.schedule {
        ScheduleSelector::Id(id) => client
            .schedule_by_id(id)
            .await
            .with_context(|| format!("fetching schedule '{id}'"))?,
        ScheduleSelector::Name(name) => client
            .schedule_by_name(name)
            .await
            .with_context(|| format!("fetching schedule '{name}'"))?
            .ok_or_else(|| anyhow!("schedule '{name}' not found"))?,
    };
    log::debug!(
        "Grafana OnCall schedule '{}' (id={}): on_call_now={:?}",
        schedule.name,
        schedule.id,
        schedule.on_call_now
    );
    Ok(is_on_duty(&user.id, &schedule))
}

/// Check every grafana-enabled team concurrently and swap in the on-duty
/// source set where the user is on call. Returns non-fatal error strings for
/// the `load_errors` pathway.
pub async fn apply_on_duty_sources(teams: &mut [ResolvedTeam]) -> Vec<String> {
    let mut errors = Vec::new();
    let mut checks = Vec::new();
    for (idx, team) in teams.iter().enumerate() {
        let Some(grafana) = team.grafana.clone() else {
            continue;
        };
        // Token resolution is synchronous and per-team: teams may carry
        // different credential overrides.
        match resolve_grafana_token(&grafana) {
            Ok(Some(token)) => checks.push((idx, grafana, token)),
            Ok(None) => errors.push(format!(
                "team '{}': on-call view disabled: no Grafana token configured \
                 (run `do-next auth` to set it up)",
                team.id
            )),
            Err(e) => errors.push(format!(
                "team '{}': grafana on-call check skipped: {e:#}",
                team.id
            )),
        }
    }

    let results = futures::future::join_all(
        checks
            .iter()
            .map(|(_, grafana, token)| check_on_duty(grafana, token.clone())),
    )
    .await;

    for ((idx, grafana, _), result) in checks.into_iter().zip(results) {
        match result {
            Ok(true) => {
                teams[idx].set_on_duty(true);
                log::info!(
                    "team '{}': on call — using on-duty sources ({:?} mode)",
                    teams[idx].id,
                    grafana.mode
                );
            }
            Ok(false) => {
                log::debug!("team '{}': not on call", teams[idx].id);
            }
            Err(e) => errors.push(format!(
                "team '{}': grafana on-call check failed: {e:#}",
                teams[idx].id
            )),
        }
    }
    errors
}
