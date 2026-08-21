pub mod company;
pub mod credentials;
pub mod hidden;
pub mod types;
pub mod updates;

use anyhow::{Context, Result, anyhow};
use std::path::{Path, PathBuf};

use types::{
    AtlassianConfig, AtlassianOverride, Config, GitlabConfig, GrafanaConfig, ResolvedGitlab,
    ResolvedGrafana, ResolvedTeam, SourceKind, TeamAtlassianOverride, TeamConfig,
    TeamGrafanaConfig, TeamRef,
};

/// Result of loading user config + all team configs.
pub struct LoadedConfig {
    /// Effective config: the user's file with company manifest values merged
    /// in. Use for running the app; never write this back to disk.
    pub config: Config,
    /// The user's config exactly as parsed from disk (no company merge).
    /// Anything that rewrites `config.json5` must start from this.
    pub raw: Config,
    pub teams: Vec<ResolvedTeam>,
    /// Non-fatal errors from team configs that failed to load.
    pub load_errors: Vec<String>,
}

/// Load user configuration and resolve all team configs.
pub fn load() -> Result<LoadedConfig> {
    let user_path = user_config_path()?;

    let mut config: Config = if user_path.exists() {
        load_file(&user_path)?
    } else {
        Config::default()
    };

    let raw = config.clone();
    let mut teams = Vec::new();
    let mut load_errors = Vec::new();
    let mut team_refs = resolve_company(&mut config, &mut load_errors);
    team_refs.extend(config.teams.iter().cloned());
    for team_ref in &team_refs {
        match load_team_config(team_ref) {
            Ok((mut team_config, warnings)) => {
                apply_backlog_choice(team_ref, &mut team_config);
                for w in warnings {
                    load_errors.push(format!("team '{}': {w}", team_ref.id));
                }
                let atlassian = resolve_team_atlassian(&config.atlassian, &team_config);
                let confluence = resolve_team_confluence(
                    &atlassian,
                    config.confluence.as_ref(),
                    team_config.confluence.as_ref(),
                );
                let open_slack_in_app = team_config
                    .open_slack_in_app
                    .or(config.open_slack_in_app)
                    .unwrap_or(true);
                let slack_team_id = team_config
                    .slack_team_id
                    .clone()
                    .or_else(|| config.slack_team_id.clone());
                let (grafana, grafana_error) = resolve_team_grafana(
                    config.grafana.as_ref(),
                    team_config.grafana.as_ref(),
                    &team_ref.id,
                );
                if let Some(e) = grafana_error {
                    load_errors.push(e);
                }
                let gitlab =
                    resolve_team_gitlab(config.gitlab.as_ref(), team_config.gitlab.as_ref());
                let normal_sources = team_config.sources.clone();
                teams.push(ResolvedTeam {
                    id: team_ref.id.clone(),
                    path: team_ref.path.clone(),
                    config: team_config,
                    atlassian,
                    confluence,
                    open_slack_in_app,
                    slack_team_id,
                    grafana,
                    gitlab,
                    normal_sources,
                    on_duty: false,
                });
            }
            Err(e) => {
                load_errors.push(format!("team '{}': {e:#}", team_ref.id));
            }
        }
    }

    Ok(LoadedConfig {
        config,
        raw,
        teams,
        load_errors,
    })
}

/// Resolve the optional company block: parse the manifest from the clone
/// directory, overlay company values onto the user's global settings
/// (in memory only — the user's file is never rewritten), and synthesize
/// `TeamRef`s for the selected catalog teams. All failures are non-fatal:
/// they land in `load_errors` and manually-configured teams keep working.
fn resolve_company(config: &mut Config, load_errors: &mut Vec<String>) -> Vec<TeamRef> {
    let Some(company_ref) = config.company.clone() else {
        return Vec::new();
    };
    let dir = expand_tilde(&company_ref.path);
    let manifest = match company::load_manifest(&dir) {
        Ok(m) => m,
        Err(e) => {
            load_errors.push(format!("company '{}': {e:#}", company_ref.path));
            return Vec::new();
        }
    };
    config.atlassian = company::apply_company_defaults(&config.atlassian, &manifest);
    if config.confluence.is_none() {
        config.confluence.clone_from(&manifest.defaults.confluence);
    }
    if config.slack_team_id.is_none() {
        config
            .slack_team_id
            .clone_from(&manifest.defaults.slack_team_id);
    }
    if config.open_slack_in_app.is_none() {
        config.open_slack_in_app = manifest.defaults.open_slack_in_app;
    }
    apply_grafana_defaults(&mut config.grafana, manifest.defaults.grafana.as_ref());
    apply_gitlab_defaults(&mut config.gitlab, manifest.defaults.gitlab.as_ref());
    let (refs, errors) = company::company_team_refs(&dir, &manifest, &company_ref.teams);
    load_errors.extend(errors);
    refs
}

/// Fill unset user Grafana fields from the company defaults, field by field:
/// a user may set only `credential_command` while taking the company
/// `oncall_api_url`. Precedence stays: team override > user config > company
/// manifest.
fn apply_grafana_defaults(user: &mut Option<GrafanaConfig>, company: Option<&GrafanaConfig>) {
    let Some(company) = company else {
        return;
    };
    let user = user.get_or_insert_with(GrafanaConfig::default);
    if user.oncall_api_url.is_none() {
        user.oncall_api_url.clone_from(&company.oncall_api_url);
    }
    if user.instance_url.is_none() {
        user.instance_url.clone_from(&company.instance_url);
    }
    if user.credential_command.is_none() {
        user.credential_command
            .clone_from(&company.credential_command);
    }
    if user.credential_store.is_none() {
        user.credential_store.clone_from(&company.credential_store);
    }
    if user.credential_key.is_none() {
        user.credential_key.clone_from(&company.credential_key);
    }
}

/// Fill unset user GitLab fields from the company defaults, field by field —
/// a user may set only `credential_command` while taking the company
/// `base_url`. Precedence stays: team override > user config > company
/// manifest > `https://gitlab.com`.
fn apply_gitlab_defaults(user: &mut Option<GitlabConfig>, company: Option<&GitlabConfig>) {
    let Some(company) = company else {
        return;
    };
    let user = user.get_or_insert_with(GitlabConfig::default);
    if user.base_url.is_none() {
        user.base_url.clone_from(&company.base_url);
    }
    if user.credential_command.is_none() {
        user.credential_command
            .clone_from(&company.credential_command);
    }
    if user.credential_store.is_none() {
        user.credential_store.clone_from(&company.credential_store);
    }
    if user.credential_key.is_none() {
        user.credential_key.clone_from(&company.credential_key);
    }
    if user.auth_method.is_none() {
        user.auth_method.clone_from(&company.auth_method);
    }
    if user.oauth_client_id.is_none() {
        user.oauth_client_id.clone_from(&company.oauth_client_id);
    }
    if user.oauth_client_secret.is_none() {
        user.oauth_client_secret
            .clone_from(&company.oauth_client_secret);
    }
}

/// Merge the effective GitLab connection: team override → company-merged user
/// config → `https://gitlab.com`. Unlike Grafana this cannot fail: `base_url`
/// has a default, so every team gets a usable value (whether it has GitLab
/// sources is a separate question — see [`ResolvedTeam::uses_gitlab`]).
fn resolve_team_gitlab(user: Option<&GitlabConfig>, team: Option<&GitlabConfig>) -> ResolvedGitlab {
    let mut resolved = ResolvedGitlab::default();
    for overlay in [user, team].into_iter().flatten() {
        if let Some(url) = &overlay.base_url {
            url.trim_end_matches('/').clone_into(&mut resolved.base_url);
        }
        if overlay.credential_command.is_some() {
            resolved
                .credential_command
                .clone_from(&overlay.credential_command);
        }
        if overlay.credential_store.is_some() {
            resolved
                .credential_store
                .clone_from(&overlay.credential_store);
        }
        if overlay.credential_key.is_some() {
            resolved.credential_key.clone_from(&overlay.credential_key);
        }
        if overlay.auth_method.is_some() {
            resolved.auth_method.clone_from(&overlay.auth_method);
        }
        if overlay.oauth_client_id.is_some() {
            resolved
                .oauth_client_id
                .clone_from(&overlay.oauth_client_id);
        }
        if overlay.oauth_client_secret.is_some() {
            resolved
                .oauth_client_secret
                .clone_from(&overlay.oauth_client_secret);
        }
    }
    resolved
}

/// Combine a team's `grafana` block (schedule + duty sources) with the
/// company-merged user connection settings into the effective settings.
/// Returns `(None, Some(error))` when the block is present but the
/// connection is unusable — the team keeps its normal sources and the error
/// surfaces as a non-fatal load error. `schedule` and `on_duty_sources`
/// presence is enforced fatally by `validate_team_config`.
fn resolve_team_grafana(
    user: Option<&GrafanaConfig>,
    team: Option<&TeamGrafanaConfig>,
    team_id: &str,
) -> (Option<ResolvedGrafana>, Option<String>) {
    let Some(team) = team else {
        return (None, None);
    };
    let Some(oncall_api_url) = user.and_then(|u| u.oncall_api_url.clone()) else {
        return (
            None,
            Some(format!(
                "team '{team_id}': grafana.oncall_api_url is not set (set it in your \
                 config.json5 or the company manifest `defaults.grafana`)"
            )),
        );
    };
    let Some(schedule) = team.schedule.clone() else {
        // Unreachable after validation; defensive for direct callers.
        return (
            None,
            Some(format!("team '{team_id}': grafana.schedule is not set")),
        );
    };
    (
        Some(ResolvedGrafana {
            oncall_api_url,
            instance_url: user.and_then(|u| u.instance_url.clone()),
            schedule,
            mode: team.mode,
            on_duty_sources: team.on_duty_sources.clone(),
            credential_command: user.and_then(|u| u.credential_command.clone()),
            credential_store: user.and_then(|u| u.credential_store.clone()),
            credential_key: user.and_then(|u| u.credential_key.clone()),
        }),
        None,
    )
}

/// True when the config references any team, either manually or through a
/// company team selection. Used by `main` to decide whether team setup runs.
pub fn has_team_refs(config: &Config) -> bool {
    !config.teams.is_empty() || config.company.as_ref().is_some_and(|c| !c.teams.is_empty())
}

pub fn user_config_path() -> Result<PathBuf> {
    Ok(dirs::config_dir()
        .context("Cannot determine config directory")?
        .join("do-next")
        .join("config.json5"))
}

pub fn load_file<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read config file: {}", path.display()))?;
    json5::from_str(&content)
        .map_err(|e| anyhow::anyhow!("{}{}", e, alias_collision_hint(&e.to_string())))
        .with_context(|| format!("Failed to parse config file: {}", path.display()))
}

/// Extra explanation for a duplicate-field error caused by a renamed key.
///
/// `atlassian:` accepts `jira:` as a serde alias, so a file carrying both
/// fails — but serde names only the canonical spelling, which may not even
/// appear in the user's file. Say what the two keys have to do with each other.
fn alias_collision_hint(message: &str) -> String {
    if message.contains("duplicate field `atlassian`") {
        "\n\nhint: `atlassian:` and `jira:` are the same key — `jira:` is the \
         old spelling, still accepted. Keep one of them."
            .to_string()
    } else {
        String::new()
    }
}

/// The backlog tab is a per-user choice: when the team ref opts out, its
/// backlog sources are dropped before the team reaches the app, so tabs,
/// fetches and OAuth scopes all follow. Unset means enabled.
fn apply_backlog_choice(team_ref: &TeamRef, config: &mut TeamConfig) {
    if !team_ref.backlog.unwrap_or(true) {
        config.sources.retain(|s| s.kind != SourceKind::Backlog);
        if let Some(grafana) = &mut config.grafana {
            grafana
                .on_duty_sources
                .retain(|s| s.kind != SourceKind::Backlog);
        }
    }
}

/// Load a single team config from disk. Returns the parsed config plus
/// any non-fatal warnings (e.g. template files that couldn't be read).
fn load_team_config(team_ref: &TeamRef) -> Result<(TeamConfig, Vec<String>)> {
    let dir = expand_tilde(&team_ref.path);
    let file_name = team_ref.file.as_deref().unwrap_or("do-next.json5");
    let path = dir.join(file_name);
    let team: TeamConfig = load_file(&path)
        .with_context(|| format!("Failed to load team '{}' config", team_ref.id))?;
    validate_team_config(&team)?;
    let warnings = collect_team_warnings(&team, &dir);
    Ok((team, warnings))
}

fn validate_team_config(team: &TeamConfig) -> Result<()> {
    for source in &team.sources {
        validate_source_config(source)?;
    }
    if let Some(grafana) = &team.grafana {
        if grafana.schedule.is_none() {
            return Err(anyhow!(
                "grafana: set `schedule` to a schedule name or {{ id: ... }}"
            ));
        }
        if grafana.on_duty_sources.is_empty() {
            return Err(anyhow!(
                "grafana: `on_duty_sources` must not be empty (it replaces `sources` while on call)"
            ));
        }
        for source in &grafana.on_duty_sources {
            validate_source_config(source)?;
        }
        // In prepend mode both sets are active at once — an id collision
        // would produce two live sources with the same identity.
        if grafana.mode == types::OnDutyMode::Prepend {
            for duty in &grafana.on_duty_sources {
                if team.sources.iter().any(|s| s.id == duty.id) {
                    return Err(anyhow!(
                        "grafana: on-duty source id '{}' collides with a normal source \
                         (ids must be distinct in `prepend` mode)",
                        duty.id
                    ));
                }
            }
        }
    }
    for (view_id, view) in &team.views {
        for section in &view.sections {
            for field in &section.fields {
                if field.template.is_some() && field.templates.is_some() {
                    return Err(anyhow!(
                        "view '{}', field '{}': set either `template` or `templates`, not both",
                        view_id,
                        field.field_id
                    ));
                }
                if field.r#type.is_some() && field.uses_legacy_date_flags() {
                    return Err(anyhow!(
                        "view '{}', field '{}': set `type`, not the deprecated `date`/`datetime` flags alongside it",
                        view_id,
                        field.field_id
                    ));
                }
                if field.date == Some(true) && field.datetime == Some(true) {
                    return Err(anyhow!(
                        "view '{}', field '{}': set either `date` or `datetime`, not both",
                        view_id,
                        field.field_id
                    ));
                }
                if let Some(entries) = &field.templates {
                    for (i, entry) in entries.iter().enumerate() {
                        if entry.name.trim().is_empty() {
                            return Err(anyhow!(
                                "view '{}', field '{}': templates[{}].name is empty",
                                view_id,
                                field.field_id,
                                i
                            ));
                        }
                        if entry.path.trim().is_empty() {
                            return Err(anyhow!(
                                "view '{}', field '{}': templates[{}].path is empty",
                                view_id,
                                field.field_id,
                                i
                            ));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::{
        CustomViewConfig, CustomViewFieldConfig, CustomViewSectionConfig, TeamConfig,
    };

    fn team_with_field(field: CustomViewFieldConfig) -> TeamConfig {
        let view = CustomViewConfig {
            timezone: None,
            sections: vec![CustomViewSectionConfig {
                title: "Section".into(),
                description: None,
                fields: vec![field],
            }],
        };
        TeamConfig {
            views: std::iter::once(("view".to_string(), view)).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn date_and_datetime_together_is_an_error() {
        let team = team_with_field(CustomViewFieldConfig {
            field_id: "duedate".into(),
            date: Some(true),
            datetime: Some(true),
            ..Default::default()
        });
        let err = validate_team_config(&team).expect_err("conflict must be rejected");
        assert!(err.to_string().contains("`date` or `datetime`"));
    }

    #[test]
    fn date_alone_is_valid() {
        let team = team_with_field(CustomViewFieldConfig {
            field_id: "duedate".into(),
            date: Some(true),
            ..Default::default()
        });
        assert!(validate_team_config(&team).is_ok());
    }

    #[test]
    fn explicit_false_does_not_conflict() {
        let team = team_with_field(CustomViewFieldConfig {
            field_id: "duedate".into(),
            date: Some(true),
            datetime: Some(false),
            ..Default::default()
        });
        assert!(validate_team_config(&team).is_ok());
    }

    #[test]
    fn type_alone_is_valid() {
        let team = team_with_field(CustomViewFieldConfig {
            field_id: "duedate".into(),
            r#type: Some(crate::config::types::FieldType::Date),
            ..Default::default()
        });
        assert!(validate_team_config(&team).is_ok());
    }

    #[test]
    fn type_alongside_legacy_flag_is_an_error() {
        let team = team_with_field(CustomViewFieldConfig {
            field_id: "duedate".into(),
            r#type: Some(crate::config::types::FieldType::DateTime),
            datetime: Some(true),
            ..Default::default()
        });
        let err = validate_team_config(&team).expect_err("conflict must be rejected");
        assert!(err.to_string().contains("deprecated"));
    }

    #[test]
    fn legacy_flags_produce_deprecation_warning() {
        let team = team_with_field(CustomViewFieldConfig {
            field_id: "duedate".into(),
            date: Some(true),
            ..Default::default()
        });
        let warnings = collect_team_warnings(&team, std::path::Path::new("."));
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("deprecated") && w.contains("type: \"date\""))
        );
    }

    // ── Source kinds ─────────────────────────────────────────────────────

    use crate::config::types::{ConfluenceFilters, SourceConfig};

    #[test]
    fn source_without_kind_parses_as_jira() {
        let src: SourceConfig =
            json5::from_str(r#"{ id: "mine", jql: "assignee = currentUser()" }"#).unwrap();
        assert_eq!(src.kind, SourceKind::Jira);
        assert_eq!(src.jql, "assignee = currentUser()");
        assert!(src.confluence.is_none());
    }

    #[test]
    fn confluence_source_parses_with_filters() {
        let src: SourceConfig = json5::from_str(
            r#"{
                id: "actions",
                kind: "confluence",
                confluence: { spaces: ["ENG"], status: "incomplete", due_before: "2026-08-01" },
            }"#,
        )
        .unwrap();
        assert_eq!(src.kind, SourceKind::Confluence);
        let filters = src.confluence.unwrap();
        assert_eq!(filters.spaces, vec!["ENG"]);
        assert_eq!(filters.status.as_deref(), Some("incomplete"));
        assert!(!filters.include_blank);
        assert!(
            validate_source_config(
                &json5::from_str::<SourceConfig>(r#"{ id: "actions", kind: "confluence" }"#)
                    .unwrap()
            )
            .is_ok()
        );
    }

    fn confluence_source(filters: ConfluenceFilters) -> SourceConfig {
        SourceConfig {
            id: "actions".into(),
            kind: SourceKind::Confluence,
            confluence: Some(filters),
            ..Default::default()
        }
    }

    #[test]
    fn jira_source_rejects_confluence_block() {
        let src = SourceConfig {
            id: "mine".into(),
            jql: "assignee = currentUser()".into(),
            confluence: Some(ConfluenceFilters::default()),
            ..Default::default()
        };
        let err = validate_source_config(&src).expect_err("must reject");
        assert!(err.to_string().contains("confluence"));
    }

    #[test]
    fn confluence_source_rejects_jql() {
        let src = SourceConfig {
            id: "actions".into(),
            kind: SourceKind::Confluence,
            jql: "assignee = currentUser()".into(),
            ..Default::default()
        };
        assert!(validate_source_config(&src).is_err());
    }

    #[test]
    fn confluence_filters_validate_status_dates_and_pages() {
        let bad_status = confluence_source(ConfluenceFilters {
            status: Some("done".into()),
            ..Default::default()
        });
        assert!(validate_source_config(&bad_status).is_err());

        let bad_date = confluence_source(ConfluenceFilters {
            due_before: Some("next week".into()),
            ..Default::default()
        });
        assert!(validate_source_config(&bad_date).is_err());

        let bad_page = confluence_source(ConfluenceFilters {
            pages: vec!["My Page".into()],
            ..Default::default()
        });
        assert!(validate_source_config(&bad_page).is_err());

        let ok = confluence_source(ConfluenceFilters {
            status: Some("any".into()),
            due_before: Some("2026-08-01".into()),
            pages: vec!["12345".into()],
            assignee: Some("me".into()),
            ..Default::default()
        });
        assert!(validate_source_config(&ok).is_ok());
    }

    // ── GitLab sources ────────────────────────────────────────────────────

    fn gitlab_source(filters: types::GitlabFilters) -> SourceConfig {
        SourceConfig {
            id: "reviews".into(),
            kind: SourceKind::Gitlab,
            gitlab: Some(filters),
            ..Default::default()
        }
    }

    #[test]
    fn gitlab_source_parses_with_filters() {
        let src: SourceConfig = json5::from_str(
            r#"{
                id: "reviews",
                kind: "gitlab",
                display_name: "My reviews",
                gitlab: {
                    role: "reviewer",
                    state: "opened",
                    groups: ["backend"],
                    projects: ["backend/api"],
                    labels: ["needs-review"],
                    draft: "exclude",
                    label: "both",
                },
                allow_hide_for_a_day: true,
            }"#,
        )
        .unwrap();
        assert_eq!(src.kind, SourceKind::Gitlab);
        let filters = src.gitlab.as_ref().expect("gitlab filters");
        assert_eq!(filters.role, types::GitlabRole::Reviewer);
        assert_eq!(filters.groups, vec!["backend"]);
        assert_eq!(filters.projects, vec!["backend/api"]);
        assert_eq!(filters.labels, vec!["needs-review"]);
        assert_eq!(filters.draft, types::DraftFilter::Exclude);
        assert!(src.allow_hide_for_a_day);
        assert!(validate_source_config(&src).is_ok());

        // The whole filter block is optional — "my open MRs as reviewer".
        let bare: SourceConfig = json5::from_str(r#"{ id: "reviews", kind: "gitlab" }"#).unwrap();
        assert!(bare.gitlab.is_none());
        assert!(validate_source_config(&bare).is_ok());
    }

    #[test]
    fn gitlab_source_rejects_jira_and_confluence_options() {
        let mut with_jql = gitlab_source(types::GitlabFilters::default());
        with_jql.jql = "project = X".into();
        let err = validate_source_config(&with_jql).expect_err("must reject jql");
        assert!(err.to_string().contains("jql"), "{err}");

        let mut with_subsources = gitlab_source(types::GitlabFilters::default());
        with_subsources.subsources = vec![types::SubsourceConfig::default()];
        assert!(validate_source_config(&with_subsources).is_err());

        let mut with_expected = gitlab_source(types::GitlabFilters::default());
        with_expected.expected_project = Some("OPS".into());
        assert!(validate_source_config(&with_expected).is_err());

        let mut with_board = gitlab_source(types::GitlabFilters::default());
        with_board.board = Some(types::BoardFilters {
            board_id: 42,
            ..Default::default()
        });
        assert!(validate_source_config(&with_board).is_err());

        let mut with_confluence = gitlab_source(types::GitlabFilters::default());
        with_confluence.confluence = Some(ConfluenceFilters::default());
        assert!(validate_source_config(&with_confluence).is_err());
    }

    #[test]
    fn every_other_source_kind_rejects_a_gitlab_block() {
        for kind in [
            SourceKind::Jira,
            SourceKind::Confluence,
            SourceKind::Board,
            SourceKind::Backlog,
        ] {
            let src = SourceConfig {
                id: "s".into(),
                kind,
                gitlab: Some(types::GitlabFilters::default()),
                // Board kinds need an otherwise-valid board block so the
                // gitlab rejection is what fails, not a missing board_id.
                board: matches!(kind, SourceKind::Board | SourceKind::Backlog).then(|| {
                    types::BoardFilters {
                        board_id: 42,
                        ..Default::default()
                    }
                }),
                ..Default::default()
            };
            let err = validate_source_config(&src).expect_err("must reject: {kind:?}");
            assert!(
                err.to_string().contains("gitlab"),
                "kind {kind:?} rejected for the wrong reason: {err}"
            );
        }
    }

    #[test]
    fn gitlab_filters_validate_paths_labels_and_username() {
        let empty_group = gitlab_source(types::GitlabFilters {
            groups: vec![" ".into()],
            ..Default::default()
        });
        assert!(validate_source_config(&empty_group).is_err());

        let empty_label = gitlab_source(types::GitlabFilters {
            labels: vec![String::new()],
            ..Default::default()
        });
        assert!(validate_source_config(&empty_label).is_err());

        for path in ["/backend/api", "backend/api/"] {
            let bad = gitlab_source(types::GitlabFilters {
                projects: vec![path.into()],
                ..Default::default()
            });
            let err = validate_source_config(&bad).expect_err("must reject: {path}");
            assert!(err.to_string().contains('/'), "{err}");
        }

        let empty_username = gitlab_source(types::GitlabFilters {
            username: Some("  ".into()),
            ..Default::default()
        });
        assert!(validate_source_config(&empty_username).is_err());

        let ok = gitlab_source(types::GitlabFilters {
            groups: vec!["acme/backend".into()],
            projects: vec!["backend/api".into()],
            labels: vec!["needs-review".into()],
            username: Some("someone".into()),
            ..Default::default()
        });
        assert!(validate_source_config(&ok).is_ok());
    }

    // ── GitLab connection resolution ──────────────────────────────────────

    #[test]
    fn gitlab_precedence_is_team_over_user_over_company_over_default() {
        // Nothing configured anywhere → gitlab.com.
        let resolved = resolve_team_gitlab(None, None);
        assert_eq!(resolved.base_url, types::GITLAB_DEFAULT_BASE_URL);
        assert_eq!(resolved.credential_store, None);

        // Company defaults fill an absent user block wholesale...
        let company = GitlabConfig {
            base_url: Some("https://gitlab.company.com".into()),
            credential_store: Some("keyring".into()),
            ..Default::default()
        };
        let mut user = None;
        apply_gitlab_defaults(&mut user, Some(&company));
        let resolved = resolve_team_gitlab(user.as_ref(), None);
        assert_eq!(resolved.base_url, "https://gitlab.company.com");
        assert_eq!(resolved.credential_store.as_deref(), Some("keyring"));

        // ...but only unset fields: the user's own base_url wins.
        let mut user = Some(GitlabConfig {
            base_url: Some("https://gitlab.mine.com".into()),
            ..Default::default()
        });
        apply_gitlab_defaults(&mut user, Some(&company));
        let resolved = resolve_team_gitlab(user.as_ref(), None);
        assert_eq!(resolved.base_url, "https://gitlab.mine.com");
        assert_eq!(resolved.credential_store.as_deref(), Some("keyring"));

        // And a team override beats the user config, field by field.
        let team = GitlabConfig {
            credential_key: Some("team-key".into()),
            ..Default::default()
        };
        let resolved = resolve_team_gitlab(user.as_ref(), Some(&team));
        assert_eq!(resolved.base_url, "https://gitlab.mine.com");
        assert_eq!(resolved.credential_key.as_deref(), Some("team-key"));
        assert_eq!(resolved.credential_store.as_deref(), Some("keyring"));

        // No company defaults at all: user config untouched.
        let mut user = None;
        apply_gitlab_defaults(&mut user, None);
        assert!(user.is_none());
    }

    #[test]
    fn gitlab_oauth_settings_follow_the_same_precedence() {
        // A company manifest is how a team shares one registered OAuth app:
        // the client id is not a secret, so it travels with the connection
        // settings.
        let company = GitlabConfig {
            base_url: Some("https://gitlab.company.com".into()),
            auth_method: Some("oauth".into()),
            oauth_client_id: Some("company-app".into()),
            ..Default::default()
        };
        let mut user = None;
        apply_gitlab_defaults(&mut user, Some(&company));
        let resolved = resolve_team_gitlab(user.as_ref(), None);
        assert!(resolved.uses_oauth());
        assert_eq!(resolved.oauth_client_id.as_deref(), Some("company-app"));
        assert_eq!(resolved.oauth_client_secret, None);

        // A user who registered their own app keeps it.
        let mut user = Some(GitlabConfig {
            oauth_client_id: Some("my-app".into()),
            ..Default::default()
        });
        apply_gitlab_defaults(&mut user, Some(&company));
        let resolved = resolve_team_gitlab(user.as_ref(), None);
        assert_eq!(resolved.oauth_client_id.as_deref(), Some("my-app"));
        // ...while still inheriting the company's method and instance.
        assert!(resolved.uses_oauth());
        assert_eq!(resolved.base_url, "https://gitlab.company.com");

        // A team can opt back out of an OAuth-by-default company setup.
        let team = GitlabConfig {
            auth_method: Some("token".into()),
            ..Default::default()
        };
        let resolved = resolve_team_gitlab(user.as_ref(), Some(&team));
        assert!(!resolved.uses_oauth());
    }

    #[test]
    fn gitlab_defaults_to_the_token_path_when_nothing_says_otherwise() {
        let resolved = resolve_team_gitlab(None, None);
        assert_eq!(resolved.auth_method, None);
        assert!(
            !resolved.uses_oauth(),
            "absent auth_method must mean the personal-token path"
        );
        // An unrecognised value is not OAuth either — it must not silently
        // enable a different flow.
        let odd = GitlabConfig {
            auth_method: Some("OAuth".into()),
            ..Default::default()
        };
        assert!(!resolve_team_gitlab(Some(&odd), None).uses_oauth());
    }

    #[test]
    fn gitlab_base_url_loses_its_trailing_slash() {
        let user = GitlabConfig {
            base_url: Some("https://gitlab.example.com/".into()),
            ..Default::default()
        };
        assert_eq!(
            resolve_team_gitlab(Some(&user), None).base_url,
            "https://gitlab.example.com"
        );
    }

    #[test]
    fn gitlab_sources_need_no_extra_atlassian_scopes() {
        let teams = [team_with_kinds(&[SourceKind::Gitlab])];
        let extra = extra_scopes_for(&teams);
        assert!(!extra.confluence);
        assert!(!extra.board);
    }

    // ── Board sources ─────────────────────────────────────────────────────

    fn board_source(board: types::BoardFilters) -> SourceConfig {
        SourceConfig {
            id: "sprint-board".into(),
            kind: SourceKind::Board,
            board: Some(board),
            ..Default::default()
        }
    }

    #[test]
    fn board_source_parses_with_all_sprint_forms() {
        for (form, expected) in [
            (r#"sprint: "active","#, Some(types::SprintSelector::Active)),
            (r#"sprint: "all","#, Some(types::SprintSelector::All)),
            ("sprint: 137,", Some(types::SprintSelector::Id(137))),
            ("", None), // omitted → fetcher defaults to Active
        ] {
            let src: SourceConfig = json5::from_str(&format!(
                r#"{{ id: "b", kind: "board", board: {{ board_id: 42, {form} }} }}"#
            ))
            .unwrap();
            assert_eq!(src.kind, SourceKind::Board);
            let board = src.board.unwrap();
            assert_eq!(board.board_id, 42);
            assert_eq!(board.sprint, expected, "form: {form}");
        }
        assert!(
            json5::from_str::<SourceConfig>(
                r#"{ id: "b", kind: "board", board: { board_id: 42, sprint: "next" } }"#
            )
            .is_err()
        );
    }

    #[test]
    fn board_swimlanes_parse_all_forms() {
        let auto: SourceConfig = json5::from_str(
            r#"{ id: "b", kind: "board", board: { board_id: 1, swimlanes: "auto" } }"#,
        )
        .unwrap();
        assert_eq!(
            auto.board.unwrap().swimlanes,
            Some(types::SwimlaneConfig::Auto)
        );

        let field: SourceConfig = json5::from_str(
            r#"{ id: "b", kind: "board", board: { board_id: 1, swimlanes: { field: "priority" } } }"#,
        )
        .unwrap();
        assert_eq!(
            field.board.unwrap().swimlanes,
            Some(types::SwimlaneConfig::Field {
                field: "priority".into()
            })
        );

        let queries: SourceConfig = json5::from_str(
            r#"{ id: "b", kind: "board", board: { board_id: 1,
                swimlanes: { lanes: [{ name: "Expedite", jql: "priority = Highest" }] } } }"#,
        )
        .unwrap();
        let Some(types::SwimlaneConfig::Queries {
            lanes,
            everything_else,
        }) = queries.board.unwrap().swimlanes
        else {
            panic!("expected query lanes");
        };
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].name, "Expedite");
        assert!(everything_else, "everything_else defaults to true");

        // Unknown string / unknown key / both field and lanes are rejected.
        for bad in [
            r#"{ board_id: 1, swimlanes: "assignee" }"#,
            r#"{ board_id: 1, swimlanes: { strategy: "auto" } }"#,
            r#"{ board_id: 1, swimlanes: { field: "priority", lanes: [] } }"#,
        ] {
            assert!(
                json5::from_str::<types::BoardFilters>(bad).is_err(),
                "should reject: {bad}"
            );
        }
    }

    #[test]
    fn sprint_selector_and_swimlanes_round_trip() {
        for board in [
            types::BoardFilters {
                board_id: 7,
                sprint: Some(types::SprintSelector::Id(137)),
                swimlanes: Some(types::SwimlaneConfig::Auto),
                detail_load: None,
            },
            types::BoardFilters {
                board_id: 7,
                sprint: Some(types::SprintSelector::All),
                swimlanes: Some(types::SwimlaneConfig::Queries {
                    lanes: vec![types::QueryLane {
                        name: "Expedite".into(),
                        jql: "priority = Highest".into(),
                    }],
                    everything_else: false,
                }),
                detail_load: Some(types::DetailLoad::Full),
            },
        ] {
            let json = serde_json::to_string(&board).unwrap();
            let back: types::BoardFilters = serde_json::from_str(&json).unwrap();
            assert_eq!(back.sprint, board.sprint);
            assert_eq!(back.swimlanes, board.swimlanes);
        }
    }

    #[test]
    fn board_source_requires_board_block_and_positive_id() {
        let missing = SourceConfig {
            id: "b".into(),
            kind: SourceKind::Board,
            ..Default::default()
        };
        let err = validate_source_config(&missing).expect_err("must reject");
        assert!(err.to_string().contains("board"));

        let zero = board_source(types::BoardFilters::default());
        assert!(validate_source_config(&zero).is_err());

        let ok = board_source(types::BoardFilters {
            board_id: 42,
            ..Default::default()
        });
        assert!(validate_source_config(&ok).is_ok());
    }

    #[test]
    fn board_source_rejects_jql_subsources_and_confluence() {
        let base = || types::BoardFilters {
            board_id: 42,
            ..Default::default()
        };
        let mut with_jql = board_source(base());
        with_jql.jql = "project = X".into();
        assert!(validate_source_config(&with_jql).is_err());

        let mut with_subsources = board_source(base());
        with_subsources.subsources = vec![types::SubsourceConfig::default()];
        assert!(validate_source_config(&with_subsources).is_err());

        let mut with_confluence = board_source(base());
        with_confluence.confluence = Some(ConfluenceFilters::default());
        assert!(validate_source_config(&with_confluence).is_err());
    }

    #[test]
    fn board_swimlane_validation_rejects_empty_lanes_and_field() {
        let empty_lanes = board_source(types::BoardFilters {
            board_id: 42,
            swimlanes: Some(types::SwimlaneConfig::Queries {
                lanes: vec![],
                everything_else: true,
            }),
            ..Default::default()
        });
        assert!(validate_source_config(&empty_lanes).is_err());

        let blank_jql = board_source(types::BoardFilters {
            board_id: 42,
            swimlanes: Some(types::SwimlaneConfig::Queries {
                lanes: vec![types::QueryLane {
                    name: "Expedite".into(),
                    jql: String::new(),
                }],
                everything_else: true,
            }),
            ..Default::default()
        });
        assert!(validate_source_config(&blank_jql).is_err());

        let blank_field = board_source(types::BoardFilters {
            board_id: 42,
            swimlanes: Some(types::SwimlaneConfig::Field {
                field: String::new(),
            }),
            ..Default::default()
        });
        assert!(validate_source_config(&blank_field).is_err());
    }

    // ── Backlog sources ───────────────────────────────────────────────────

    fn backlog_source(board: types::BoardFilters) -> SourceConfig {
        SourceConfig {
            id: "backlog".into(),
            kind: SourceKind::Backlog,
            board: Some(board),
            ..Default::default()
        }
    }

    #[test]
    fn backlog_source_parses_shortcut_and_rich_forms() {
        let shortcut: SourceConfig =
            json5::from_str(r#"{ id: "bl", kind: "backlog", board: { board_id: 42 } }"#).unwrap();
        assert_eq!(shortcut.kind, SourceKind::Backlog);
        assert_eq!(shortcut.board.as_ref().unwrap().board_id, 42);
        assert!(validate_source_config(&shortcut).is_ok());

        let rich: SourceConfig = json5::from_str(
            r#"{ id: "bl", display_name: "Backlog", kind: "backlog",
                 board: { board_id: 42, detail_load: "full" } }"#,
        )
        .unwrap();
        assert_eq!(
            rich.board.as_ref().unwrap().detail_load,
            Some(types::DetailLoad::Full)
        );
        assert!(validate_source_config(&rich).is_ok());
    }

    #[test]
    fn backlog_source_requires_board_block_and_positive_id() {
        let missing = SourceConfig {
            id: "bl".into(),
            kind: SourceKind::Backlog,
            ..Default::default()
        };
        let err = validate_source_config(&missing).expect_err("must reject");
        assert!(err.to_string().contains("board"));

        let zero = backlog_source(types::BoardFilters::default());
        assert!(validate_source_config(&zero).is_err());
    }

    #[test]
    fn backlog_source_rejects_board_only_and_jira_only_options() {
        let base = || types::BoardFilters {
            board_id: 42,
            ..Default::default()
        };

        let with_sprint = backlog_source(types::BoardFilters {
            sprint: Some(types::SprintSelector::Active),
            ..base()
        });
        let err = validate_source_config(&with_sprint).expect_err("must reject sprint");
        assert!(err.to_string().contains("sprint"));

        let with_lanes = backlog_source(types::BoardFilters {
            swimlanes: Some(types::SwimlaneConfig::Auto),
            ..base()
        });
        let err = validate_source_config(&with_lanes).expect_err("must reject swimlanes");
        assert!(err.to_string().contains("swimlanes"));

        let mut with_jql = backlog_source(base());
        with_jql.jql = "project = X".into();
        assert!(validate_source_config(&with_jql).is_err());

        let mut with_subsources = backlog_source(base());
        with_subsources.subsources = vec![types::SubsourceConfig::default()];
        assert!(validate_source_config(&with_subsources).is_err());

        let mut with_confluence = backlog_source(base());
        with_confluence.confluence = Some(ConfluenceFilters::default());
        assert!(validate_source_config(&with_confluence).is_err());
    }

    #[test]
    fn non_board_sources_reject_board_block() {
        let jira = SourceConfig {
            id: "mine".into(),
            jql: "assignee = currentUser()".into(),
            board: Some(types::BoardFilters {
                board_id: 42,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(validate_source_config(&jira).is_err());

        let confluence = SourceConfig {
            id: "actions".into(),
            kind: SourceKind::Confluence,
            board: Some(types::BoardFilters {
                board_id: 42,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(validate_source_config(&confluence).is_err());
    }

    // ── Confluence connection resolution ─────────────────────────────────

    #[test]
    fn confluence_defaults_to_effective_jira() {
        let jira = AtlassianConfig {
            base_url: "https://acme.atlassian.net".into(),
            email: Some("me@acme.com".into()),
            ..Default::default()
        };
        let conf = resolve_team_confluence(&jira, None, None);
        assert_eq!(conf, jira);
    }

    // ── resolve_company ───────────────────────────────────────────────────

    /// Build a company clone fixture: manifest + one team config on disk.
    fn company_fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("company.json5"),
            r#"{
                name: "Acme",
                jira: { base_url: "https://acme.atlassian.net", default_project: "CORE" },
                oauth: { client_id: "cid", client_secret: "sec" },
                defaults: { slack_team_id: "T0123" },
                teams: [{ id: "platform" }, { id: "ghost" }],
            }"#,
        )
        .expect("write manifest");
        let team_dir = dir.path().join("teams/platform");
        std::fs::create_dir_all(&team_dir).expect("team dir");
        std::fs::write(
            team_dir.join("do-next.json5"),
            r#"{ sources: [{ id: "mine", jql: "assignee = currentUser()" }] }"#,
        )
        .expect("write team config");
        dir
    }

    fn config_with_company(dir: &tempfile::TempDir, teams: &[&str]) -> Config {
        Config {
            company: Some(types::CompanyRef {
                url: None,
                path: dir.path().to_string_lossy().into_owned(),
                teams: teams.iter().map(|&t| t.into()).collect(),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn resolve_company_merges_defaults_and_synthesizes_refs() {
        let fixture = company_fixture();
        let mut config = config_with_company(&fixture, &["platform"]);
        let mut errors = Vec::new();
        let refs = resolve_company(&mut config, &mut errors);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].id, "platform");
        assert!(refs[0].path.ends_with("teams/platform"));
        // Company values landed in the in-memory global config.
        assert_eq!(config.atlassian.base_url, "https://acme.atlassian.net");
        assert_eq!(config.atlassian.default_project, "CORE");
        assert_eq!(config.atlassian.auth_method.as_deref(), Some("oauth"));
        assert_eq!(config.slack_team_id.as_deref(), Some("T0123"));
        // The synthesized ref resolves through the normal team loader.
        let (team, warnings) = load_team_config(&refs[0]).expect("team loads");
        assert!(warnings.is_empty());
        assert_eq!(team.sources.len(), 1);
    }

    #[test]
    fn resolve_company_missing_manifest_is_nonfatal() {
        let empty = tempfile::tempdir().expect("tempdir");
        let mut config = config_with_company(&empty, &["platform"]);
        let mut errors = Vec::new();
        let refs = resolve_company(&mut config, &mut errors);
        assert!(refs.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("company"));
        // No merge happened — user config untouched.
        assert!(config.atlassian.base_url.is_empty());
    }

    #[test]
    fn resolve_company_unknown_selected_team_errors_but_keeps_rest() {
        let fixture = company_fixture();
        let mut config = config_with_company(&fixture, &["platform", "gone"]);
        let mut errors = Vec::new();
        let refs = resolve_company(&mut config, &mut errors);
        assert_eq!(refs.len(), 1);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("gone"));
    }

    #[test]
    fn resolve_company_user_values_win() {
        let fixture = company_fixture();
        let mut config = config_with_company(&fixture, &[]);
        config.atlassian.base_url = "https://mine.atlassian.net".into();
        config.slack_team_id = Some("TMINE".into());
        let mut errors = Vec::new();
        resolve_company(&mut config, &mut errors);
        assert_eq!(config.atlassian.base_url, "https://mine.atlassian.net");
        assert_eq!(config.slack_team_id.as_deref(), Some("TMINE"));
    }

    #[test]
    fn has_team_refs_counts_manual_and_company_selections() {
        assert!(!has_team_refs(&Config::default()));
        let manual = Config {
            teams: vec![TeamRef {
                id: "personal".into(),
                path: "/tmp/personal".into(),
                file: None,
                backlog: None,
            }],
            ..Default::default()
        };
        assert!(has_team_refs(&manual));
        let company_only = Config {
            company: Some(types::CompanyRef {
                url: None,
                path: "/tmp/acme".into(),
                teams: vec!["platform".into()],
            }),
            ..Default::default()
        };
        assert!(has_team_refs(&company_only));
        let company_no_selection = Config {
            company: Some(types::CompanyRef {
                url: None,
                path: "/tmp/acme".into(),
                teams: vec![],
            }),
            ..Default::default()
        };
        assert!(!has_team_refs(&company_no_selection));
    }

    #[test]
    fn company_selection_gates_backlog_sources_end_to_end() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("company.json5"),
            r#"{
                name: "Acme",
                jira: { base_url: "https://acme.atlassian.net" },
                teams: [{ id: "platform" }],
            }"#,
        )
        .expect("write manifest");
        let team_dir = dir.path().join("teams/platform");
        std::fs::create_dir_all(&team_dir).expect("team dir");
        std::fs::write(
            team_dir.join("do-next.json5"),
            r#"{ sources: [
                { id: "mine", jql: "assignee = currentUser()" },
                { id: "bl", kind: "backlog", board: { board_id: 7 } },
            ] }"#,
        )
        .expect("write team config");

        let load_platform = |teams_json: &str| {
            let mut config: Config = json5::from_str(&format!(
                r#"{{ company: {{ path: "{}", teams: [{teams_json}] }} }}"#,
                dir.path().display()
            ))
            .expect("valid config");
            let mut errors = Vec::new();
            let refs = resolve_company(&mut config, &mut errors);
            assert!(errors.is_empty(), "unexpected errors: {errors:?}");
            let (mut team, _) = load_team_config(&refs[0]).expect("team loads");
            apply_backlog_choice(&refs[0], &mut team);
            team
        };

        // The shortcut form (and rich form without the flag) opts out.
        for teams_json in [r#""platform""#, r#"{ id: "platform" }"#] {
            let team = load_platform(teams_json);
            assert_eq!(team.sources.len(), 1, "selection: {teams_json}");
            assert_eq!(team.sources[0].kind, SourceKind::Jira);
        }
        // The rich form with backlog: true keeps the backlog source.
        let team = load_platform(r#"{ id: "platform", backlog: true }"#);
        assert_eq!(team.sources.len(), 2);
    }

    // ── apply_backlog_choice ──────────────────────────────────────────────

    fn ref_with_backlog(backlog: Option<bool>) -> TeamRef {
        TeamRef {
            id: "t".into(),
            path: "/tmp/t".into(),
            file: None,
            backlog,
        }
    }

    #[test]
    fn backlog_sources_stay_unless_opted_out() {
        for keep in [None, Some(true)] {
            let mut team = team_with_kinds(&[SourceKind::Backlog, SourceKind::Board]);
            apply_backlog_choice(&ref_with_backlog(keep), &mut team);
            assert_eq!(team.sources.len(), 2, "backlog: {keep:?}");
        }
    }

    #[test]
    fn opting_out_drops_only_backlog_sources() {
        let mut team = team_with_kinds(&[SourceKind::Backlog, SourceKind::Board, SourceKind::Jira]);
        apply_backlog_choice(&ref_with_backlog(Some(false)), &mut team);
        let kinds: Vec<SourceKind> = team.sources.iter().map(|s| s.kind).collect();
        assert_eq!(kinds, vec![SourceKind::Board, SourceKind::Jira]);
    }

    // ── extra_scopes_for ──────────────────────────────────────────────────

    fn team_with_kinds(kinds: &[SourceKind]) -> TeamConfig {
        TeamConfig {
            sources: kinds
                .iter()
                .enumerate()
                .map(|(i, &kind)| SourceConfig {
                    id: format!("s{i}"),
                    kind,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn extra_scopes_default_to_none_for_jira_only_teams() {
        let teams = [team_with_kinds(&[SourceKind::Jira]), team_with_kinds(&[])];
        let extra = extra_scopes_for(&teams);
        assert!(!extra.confluence);
        assert!(!extra.board);
    }

    #[test]
    fn extra_scopes_union_across_teams() {
        let teams = [
            team_with_kinds(&[SourceKind::Confluence]),
            team_with_kinds(&[SourceKind::Board, SourceKind::Jira]),
        ];
        let extra = extra_scopes_for(&teams);
        assert!(extra.confluence);
        assert!(extra.board);
    }

    // ── grafana on-duty config ────────────────────────────────────────────

    fn duty_source(id: &str, kind: SourceKind) -> SourceConfig {
        SourceConfig {
            id: id.into(),
            kind,
            board: matches!(kind, SourceKind::Board | SourceKind::Backlog).then(|| {
                types::BoardFilters {
                    board_id: 7,
                    ..Default::default()
                }
            }),
            ..Default::default()
        }
    }

    fn team_grafana(sources: Vec<SourceConfig>) -> types::TeamGrafanaConfig {
        types::TeamGrafanaConfig {
            schedule: Some(types::ScheduleSelector::Name("primary".into())),
            on_duty_sources: sources,
            ..Default::default()
        }
    }

    #[test]
    fn grafana_block_requires_schedule_and_duty_sources() {
        let mut grafana = team_grafana(vec![duty_source("inc", SourceKind::Jira)]);
        grafana.schedule = None;
        let team = TeamConfig {
            grafana: Some(grafana),
            ..Default::default()
        };
        let err = validate_team_config(&team).expect_err("missing schedule");
        assert!(err.to_string().contains("schedule"), "{err}");

        let team = TeamConfig {
            grafana: Some(team_grafana(vec![])),
            ..Default::default()
        };
        let err = validate_team_config(&team).expect_err("empty duty sources");
        assert!(err.to_string().contains("on_duty_sources"), "{err}");
    }

    #[test]
    fn grafana_duty_sources_validate_like_normal_sources() {
        // A jira-kind source with confluence filters is invalid anywhere.
        let bad = SourceConfig {
            id: "inc".into(),
            confluence: Some(types::ConfluenceFilters::default()),
            ..Default::default()
        };
        let team = TeamConfig {
            grafana: Some(team_grafana(vec![bad])),
            ..Default::default()
        };
        assert!(validate_team_config(&team).is_err());

        let team = TeamConfig {
            grafana: Some(team_grafana(vec![duty_source("inc", SourceKind::Jira)])),
            ..Default::default()
        };
        assert!(validate_team_config(&team).is_ok());
    }

    #[test]
    fn prepend_mode_rejects_duty_ids_colliding_with_normal_sources() {
        let mut grafana = team_grafana(vec![duty_source("mine", SourceKind::Jira)]);
        grafana.mode = types::OnDutyMode::Prepend;
        let team = TeamConfig {
            sources: vec![duty_source("mine", SourceKind::Jira)],
            grafana: Some(grafana),
            ..Default::default()
        };
        let err = validate_team_config(&team).expect_err("collision in prepend mode");
        assert!(err.to_string().contains("'mine'"), "{err}");
        assert!(err.to_string().contains("prepend"), "{err}");

        // Same collision is allowed in replace mode (only one set is active;
        // the cache caveat is documented, not fatal).
        let team = TeamConfig {
            sources: vec![duty_source("mine", SourceKind::Jira)],
            grafana: Some(team_grafana(vec![duty_source("mine", SourceKind::Jira)])),
            ..Default::default()
        };
        assert!(validate_team_config(&team).is_ok());

        // Distinct ids pass in prepend mode.
        let mut grafana = team_grafana(vec![duty_source("duty", SourceKind::Jira)]);
        grafana.mode = types::OnDutyMode::Prepend;
        let team = TeamConfig {
            sources: vec![duty_source("mine", SourceKind::Jira)],
            grafana: Some(grafana),
            ..Default::default()
        };
        assert!(validate_team_config(&team).is_ok());
    }

    #[test]
    fn resolve_team_grafana_combines_user_connection_with_team_block() {
        let user = GrafanaConfig {
            oncall_api_url: Some("https://user.example/oncall".into()),
            instance_url: Some("https://user.grafana.net".into()),
            credential_command: Some("pass show oncall".into()),
            ..Default::default()
        };
        let team = team_grafana(vec![duty_source("inc", SourceKind::Jira)]);
        let (resolved, err) = resolve_team_grafana(Some(&user), Some(&team), "t");
        assert!(err.is_none(), "{err:?}");
        let resolved = resolved.expect("resolved");
        // Connection comes from the user config (company-merged)...
        assert_eq!(resolved.oncall_api_url, "https://user.example/oncall");
        assert_eq!(
            resolved.credential_command.as_deref(),
            Some("pass show oncall")
        );
        assert_eq!(
            resolved.instance_url.as_deref(),
            Some("https://user.grafana.net")
        );
        // ...while schedule and duty sources come from the team block.
        assert_eq!(
            resolved.schedule,
            types::ScheduleSelector::Name("primary".into())
        );
        assert_eq!(resolved.on_duty_sources.len(), 1);
    }

    #[test]
    fn resolve_team_grafana_without_api_url_is_nonfatal() {
        let team = team_grafana(vec![duty_source("inc", SourceKind::Jira)]);
        let (resolved, err) = resolve_team_grafana(None, Some(&team), "t");
        assert!(resolved.is_none());
        let err = err.expect("error string");
        assert!(err.contains("oncall_api_url"), "{err}");
        assert!(err.contains("'t'"), "{err}");
    }

    #[test]
    fn resolve_team_grafana_without_block_is_none() {
        let user = GrafanaConfig {
            oncall_api_url: Some("https://user.example/oncall".into()),
            ..Default::default()
        };
        let (resolved, err) = resolve_team_grafana(Some(&user), None, "t");
        assert!(resolved.is_none());
        assert!(err.is_none());
    }

    #[test]
    fn grafana_company_defaults_fill_only_unset_fields() {
        let company = GrafanaConfig {
            oncall_api_url: Some("https://company.example/oncall".into()),
            credential_store: Some("keyring".into()),
            ..Default::default()
        };
        // No user block at all: company defaults land wholesale.
        let mut user = None;
        apply_grafana_defaults(&mut user, Some(&company));
        let filled = user.expect("filled from company");
        assert_eq!(
            filled.oncall_api_url.as_deref(),
            Some("https://company.example/oncall")
        );
        assert_eq!(filled.credential_store.as_deref(), Some("keyring"));

        // Partial user block: set fields win, unset fields fill in.
        let mut user = Some(GrafanaConfig {
            oncall_api_url: Some("https://mine.example/oncall".into()),
            ..Default::default()
        });
        apply_grafana_defaults(&mut user, Some(&company));
        let merged = user.expect("still present");
        assert_eq!(
            merged.oncall_api_url.as_deref(),
            Some("https://mine.example/oncall")
        );
        assert_eq!(merged.credential_store.as_deref(), Some("keyring"));

        // No company defaults: user config untouched.
        let mut user = None;
        apply_grafana_defaults(&mut user, None);
        assert!(user.is_none());
    }

    #[test]
    fn backlog_opt_out_also_drops_duty_backlog_sources() {
        let mut team = team_with_kinds(&[SourceKind::Jira]);
        team.grafana = Some(team_grafana(vec![
            duty_source("inc", SourceKind::Jira),
            duty_source("bl", SourceKind::Backlog),
        ]));
        apply_backlog_choice(&ref_with_backlog(Some(false)), &mut team);
        let duty = &team.grafana.as_ref().unwrap().on_duty_sources;
        assert_eq!(duty.len(), 1);
        assert_eq!(duty[0].kind, SourceKind::Jira);
    }

    #[test]
    fn extra_scopes_include_duty_sources() {
        let mut team = team_with_kinds(&[SourceKind::Jira]);
        team.grafana = Some(team_grafana(vec![duty_source("db", SourceKind::Board)]));
        let teams = [team];
        let extra = extra_scopes_for(&teams);
        assert!(extra.board);
        assert!(!extra.confluence);
    }

    #[test]
    fn confluence_override_precedence_is_team_over_user_over_jira() {
        let jira = AtlassianConfig {
            base_url: "https://acme.atlassian.net".into(),
            email: Some("me@acme.com".into()),
            ..Default::default()
        };
        let user = AtlassianOverride {
            base_url: Some("https://wiki.acme.com".into()),
            credential_key: Some("user-key".into()),
            ..Default::default()
        };
        let team = AtlassianOverride {
            credential_key: Some("team-key".into()),
            ..Default::default()
        };
        let conf = resolve_team_confluence(&jira, Some(&user), Some(&team));
        assert_eq!(conf.base_url, "https://wiki.acme.com");
        assert_eq!(conf.credential_key.as_deref(), Some("team-key"));
        assert_eq!(conf.email.as_deref(), Some("me@acme.com"));
    }

    // ── Atlassian rename: on-disk compatibility ──────────────────────────

    #[test]
    fn a_config_written_before_the_atlassian_rename_still_loads() {
        // This is the regression that would lock every existing user out.
        let cfg: types::Config = json5::from_str(
            r#"{ jira: { base_url: "https://acme.atlassian.net", default_project: "PROJ" } }"#,
        )
        .expect("legacy `jira:` key parses");
        assert_eq!(cfg.atlassian.base_url, "https://acme.atlassian.net");
        assert_eq!(cfg.atlassian.default_project, "PROJ");
    }

    #[test]
    fn the_new_atlassian_key_loads() {
        let cfg: types::Config = json5::from_str(
            r#"{ atlassian: { base_url: "https://acme.atlassian.net", default_project: "P" } }"#,
        )
        .expect("`atlassian:` key parses");
        assert_eq!(cfg.atlassian.base_url, "https://acme.atlassian.net");
    }

    #[test]
    fn a_config_carrying_both_keys_is_rejected_with_a_hint() {
        // Failing loudly beats silently picking one: a file with both is a
        // half-finished hand-migration, and guessing would hide it.
        let err = json5::from_str::<types::Config>(
            r#"{ atlassian: { base_url: "a", default_project: "P" },
                 jira: { base_url: "b", default_project: "Q" } }"#,
        )
        .expect_err("both keys must not parse");
        let message = err.to_string();
        assert!(
            message.contains("duplicate field"),
            "expected a duplicate-field error, got: {message}"
        );
        // serde names only the canonical spelling, which may not appear in the
        // user's file at all — hence the hint.
        assert!(
            alias_collision_hint(&message).contains("old spelling"),
            "the hint must explain that `jira:` is the old spelling"
        );
    }

    #[test]
    fn an_unrelated_parse_error_gets_no_alias_hint() {
        assert!(alias_collision_hint("expected `,` or `}`").is_empty());
    }

    #[test]
    fn a_team_config_written_before_the_rename_still_loads() {
        let team: types::TeamConfig = json5::from_str(r#"{ jira: { default_project: "TEAM" } }"#)
            .expect("legacy team `jira:` override parses");
        assert_eq!(
            team.atlassian
                .expect("override present")
                .default_project
                .as_deref(),
            Some("TEAM")
        );
    }
}

/// The error every non-gitlab source kind returns for a stray `gitlab` block.
fn gitlab_block_not_allowed(source_id: &str) -> anyhow::Error {
    anyhow!("source '{source_id}': `gitlab` filters are only valid with `kind: \"gitlab\"`")
}

fn validate_source_config(source: &types::SourceConfig) -> Result<()> {
    // Every kind but `gitlab` rejects a `gitlab` block; checked once here so
    // each arm below only spells out its own remaining rules.
    if source.kind != SourceKind::Gitlab && source.gitlab.is_some() {
        return Err(gitlab_block_not_allowed(&source.id));
    }
    // Same for `standup`: a block on the wrong kind is silently ignored
    // otherwise, and a standup schedule that never runs is hard to notice.
    if source.kind != SourceKind::Standup && source.standup.is_some() {
        return Err(anyhow!(
            "source '{}': `standup` settings are only valid with `kind: \"standup\"`",
            source.id
        ));
    }
    match source.kind {
        SourceKind::Jira => {
            if source.confluence.is_some() {
                return Err(anyhow!(
                    "source '{}': `confluence` filters are only valid with `kind: \"confluence\"`",
                    source.id
                ));
            }
            if source.board.is_some() {
                return Err(anyhow!(
                    "source '{}': `board` filters are only valid with `kind: \"board\"` or `kind: \"backlog\"`",
                    source.id
                ));
            }
        }
        SourceKind::Gitlab => validate_gitlab_source(source)?,
        SourceKind::Confluence => {
            if !source.jql.is_empty() {
                return Err(anyhow!(
                    "source '{}': `jql` is not valid for a confluence source",
                    source.id
                ));
            }
            if !source.subsources.is_empty() {
                return Err(anyhow!(
                    "source '{}': `subsources` are not valid for a confluence source",
                    source.id
                ));
            }
            if source.expected_project.is_some() {
                return Err(anyhow!(
                    "source '{}': `expected_project` is not valid for a confluence source",
                    source.id
                ));
            }
            if source.board.is_some() {
                return Err(anyhow!(
                    "source '{}': `board` filters are only valid with `kind: \"board\"` or `kind: \"backlog\"`",
                    source.id
                ));
            }
            if let Some(filters) = &source.confluence {
                validate_confluence_filters(&source.id, filters)?;
            }
        }
        SourceKind::Board => validate_board_source(source)?,
        SourceKind::Backlog => validate_backlog_source(source)?,
        SourceKind::Standup => validate_standup_source(source)?,
    }
    Ok(())
}

/// A standup source selects nothing itself — it derives its items from your
/// activity — so every selection knob is rejected.
fn validate_standup_source(source: &types::SourceConfig) -> Result<()> {
    for (label, present) in [
        ("jql", !source.jql.is_empty()),
        ("subsources", !source.subsources.is_empty()),
        ("expected_project", source.expected_project.is_some()),
    ] {
        if present {
            return Err(anyhow!(
                "source '{}': `{label}` is not valid for a standup source",
                source.id
            ));
        }
    }
    if source.board.is_some() {
        return Err(anyhow!(
            "source '{}': `board` filters are only valid with `kind: \"board\"` or `kind: \"backlog\"`",
            source.id
        ));
    }
    if source.confluence.is_some() {
        return Err(anyhow!(
            "source '{}': `confluence` filters are only valid with `kind: \"confluence\"` \
             (a standup source configures Confluence under `standup.confluence`)",
            source.id
        ));
    }
    if let Some(filters) = &source.standup {
        // Parse the schedule now so a typo is a load-time error rather than a
        // standup that silently finds nothing.
        filters
            .schedule
            .resolve()
            .map_err(|e| anyhow!("source '{}': {e}", source.id))?;
        if let Some(tz) = filters.timezone.as_deref()
            && crate::datetime::parse_tz_offset(tz).is_none()
        {
            return Err(anyhow!(
                "source '{}': `standup.timezone` must be an offset like \"+03\" (got \"{tz}\")",
                source.id
            ));
        }
        if let Some(include) = &filters.include
            && include.is_empty()
        {
            return Err(anyhow!(
                "source '{}': `standup.include` must list at least one backend",
                source.id
            ));
        }
    }
    Ok(())
}

fn validate_gitlab_source(source: &types::SourceConfig) -> Result<()> {
    if !source.jql.is_empty() {
        return Err(anyhow!(
            "source '{}': `jql` is not valid for a gitlab source (use the `gitlab` filter block)",
            source.id
        ));
    }
    if !source.subsources.is_empty() {
        return Err(anyhow!(
            "source '{}': `subsources` are not valid for a gitlab source",
            source.id
        ));
    }
    if source.expected_project.is_some() {
        return Err(anyhow!(
            "source '{}': `expected_project` is not valid for a gitlab source",
            source.id
        ));
    }
    if source.board.is_some() {
        return Err(anyhow!(
            "source '{}': `board` filters are only valid with `kind: \"board\"` or `kind: \"backlog\"`",
            source.id
        ));
    }
    if source.confluence.is_some() {
        return Err(anyhow!(
            "source '{}': `confluence` filters are only valid with `kind: \"confluence\"`",
            source.id
        ));
    }
    if let Some(filters) = &source.gitlab {
        validate_gitlab_filters(&source.id, filters)?;
    }
    Ok(())
}

fn validate_gitlab_filters(source_id: &str, filters: &types::GitlabFilters) -> Result<()> {
    for (label, values) in [
        ("groups", &filters.groups),
        ("projects", &filters.projects),
        ("labels", &filters.labels),
    ] {
        for value in values {
            if value.trim().is_empty() {
                return Err(anyhow!(
                    "source '{source_id}': `{label}` entries must not be empty"
                ));
            }
        }
    }
    // Group/project paths are URL-encoded into a single path segment, so a
    // stray slash silently addresses the wrong namespace.
    for (label, values) in [("groups", &filters.groups), ("projects", &filters.projects)] {
        for value in values {
            if value.starts_with('/') || value.ends_with('/') {
                return Err(anyhow!(
                    "source '{source_id}': `{label}` entry \"{value}\" must be a full path \
                     without leading or trailing '/' (e.g. \"backend/api\")"
                ));
            }
        }
    }
    if let Some(username) = &filters.username
        && username.trim().is_empty()
    {
        return Err(anyhow!(
            "source '{source_id}': `username` must not be empty (omit it to use your own)"
        ));
    }
    Ok(())
}

fn validate_backlog_source(source: &types::SourceConfig) -> Result<()> {
    let Some(board) = &source.board else {
        return Err(anyhow!(
            "source '{}': a `board` block with `board_id` is required for a backlog source",
            source.id
        ));
    };
    if board.board_id == 0 {
        return Err(anyhow!(
            "source '{}': `board_id` must be a positive board id",
            source.id
        ));
    }
    if !source.jql.is_empty() {
        return Err(anyhow!(
            "source '{}': `jql` is not valid for a backlog source (the board's backlog defines the query)",
            source.id
        ));
    }
    if !source.subsources.is_empty() {
        return Err(anyhow!(
            "source '{}': `subsources` are not valid for a backlog source",
            source.id
        ));
    }
    if source.confluence.is_some() {
        return Err(anyhow!(
            "source '{}': `confluence` filters are only valid with `kind: \"confluence\"`",
            source.id
        ));
    }
    if board.sprint.is_some() {
        return Err(anyhow!(
            "source '{}': `sprint` is not valid for a backlog source (the backlog is what's outside sprints)",
            source.id
        ));
    }
    if board.swimlanes.is_some() {
        return Err(anyhow!(
            "source '{}': `swimlanes` are not valid for a backlog source (it renders as a rank-ordered list)",
            source.id
        ));
    }
    Ok(())
}

fn validate_board_source(source: &types::SourceConfig) -> Result<()> {
    let Some(board) = &source.board else {
        return Err(anyhow!(
            "source '{}': a `board` block with `board_id` is required for a board source",
            source.id
        ));
    };
    if board.board_id == 0 {
        return Err(anyhow!(
            "source '{}': `board_id` must be a positive board id",
            source.id
        ));
    }
    if !source.jql.is_empty() {
        return Err(anyhow!(
            "source '{}': `jql` is not valid for a board source (the board's saved filter defines the query)",
            source.id
        ));
    }
    if !source.subsources.is_empty() {
        return Err(anyhow!(
            "source '{}': `subsources` are not valid for a board source",
            source.id
        ));
    }
    if source.confluence.is_some() {
        return Err(anyhow!(
            "source '{}': `confluence` filters are only valid with `kind: \"confluence\"`",
            source.id
        ));
    }
    if let Some(types::SwimlaneConfig::Field { field }) = &board.swimlanes
        && field.is_empty()
    {
        return Err(anyhow!(
            "source '{}': `swimlanes.field` must not be empty",
            source.id
        ));
    }
    if let Some(types::SwimlaneConfig::Queries { lanes, .. }) = &board.swimlanes {
        if lanes.is_empty() {
            return Err(anyhow!(
                "source '{}': `swimlanes.lanes` must not be empty",
                source.id
            ));
        }
        for lane in lanes {
            if lane.name.is_empty() || lane.jql.is_empty() {
                return Err(anyhow!(
                    "source '{}': every swimlane needs a non-empty `name` and `jql`",
                    source.id
                ));
            }
        }
    }
    Ok(())
}

fn validate_confluence_filters(source_id: &str, filters: &types::ConfluenceFilters) -> Result<()> {
    if let Some(status) = filters.status.as_deref()
        && !matches!(status, "incomplete" | "complete" | "any")
    {
        return Err(anyhow!(
            "source '{source_id}': `status` must be \"incomplete\", \"complete\" or \"any\" (got \"{status}\")"
        ));
    }
    for (label, value) in [
        ("due_before", &filters.due_before),
        ("due_after", &filters.due_after),
    ] {
        if let Some(date) = value
            && chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_err()
        {
            return Err(anyhow!(
                "source '{source_id}': `{label}` must be a YYYY-MM-DD date (got \"{date}\")"
            ));
        }
    }
    for page in &filters.pages {
        if page.is_empty() || !page.chars().all(|c| c.is_ascii_digit()) {
            return Err(anyhow!(
                "source '{source_id}': `pages` entries must be numeric page IDs (got \"{page}\")"
            ));
        }
    }
    Ok(())
}

/// Merge the effective Confluence connection config: team confluence override
/// → user confluence config → the team's effective Jira config. Expressed as
/// a `AtlassianConfig` so `credentials::resolve_atlassian_auth` applies unchanged.
fn resolve_team_confluence(
    effective_jira: &AtlassianConfig,
    user: Option<&AtlassianOverride>,
    team: Option<&AtlassianOverride>,
) -> AtlassianConfig {
    let mut conf = effective_jira.clone();
    for overlay in [user, team].into_iter().flatten() {
        apply_confluence_override(&mut conf, overlay);
    }
    conf
}

fn apply_confluence_override(base: &mut AtlassianConfig, overlay: &AtlassianOverride) {
    if let Some(ref v) = overlay.base_url {
        base.base_url.clone_from(v);
    }
    if overlay.email.is_some() {
        base.email.clone_from(&overlay.email);
    }
    if overlay.credential_command.is_some() {
        base.credential_command
            .clone_from(&overlay.credential_command);
    }
    if overlay.credential_store.is_some() {
        base.credential_store.clone_from(&overlay.credential_store);
    }
    if overlay.credential_key.is_some() {
        base.credential_key.clone_from(&overlay.credential_key);
    }
    if overlay.auth_method.is_some() {
        base.auth_method.clone_from(&overlay.auth_method);
    }
    if overlay.oauth_client_id.is_some() {
        base.oauth_client_id.clone_from(&overlay.oauth_client_id);
    }
    if overlay.oauth_client_secret.is_some() {
        base.oauth_client_secret
            .clone_from(&overlay.oauth_client_secret);
    }
}

/// Walk view fields and report non-fatal issues: deprecated config keys and
/// template paths that can't be read or are empty. These surface as warnings
/// instead of failing the team load.
fn collect_team_warnings(team: &TeamConfig, dir: &Path) -> Vec<String> {
    let mut warnings = Vec::new();
    for (view_id, view) in &team.views {
        for section in &view.sections {
            for field in &section.fields {
                if field.uses_legacy_date_flags() {
                    let advice = if field.datetime == Some(true) {
                        "use `type: \"datetime\"`"
                    } else if field.date == Some(true) {
                        "use `type: \"date\"`"
                    } else {
                        "remove them"
                    };
                    warnings.push(format!(
                        "view '{}', field '{}': `date`/`datetime` flags are deprecated, {}",
                        view_id, field.field_id, advice
                    ));
                }
                let paths: Vec<&str> = if let Some(p) = &field.template {
                    vec![p.as_str()]
                } else if let Some(entries) = &field.templates {
                    entries.iter().map(|e| e.path.as_str()).collect()
                } else {
                    continue;
                };
                for rel in paths {
                    let full = dir.join(rel);
                    match std::fs::read_to_string(&full) {
                        Ok(s) if s.trim().is_empty() => {
                            warnings.push(format!(
                                "view '{}', field '{}': template '{}' is empty",
                                view_id, field.field_id, rel
                            ));
                        }
                        Err(e) => {
                            warnings.push(format!(
                                "view '{}', field '{}': cannot read template '{}': {e}",
                                view_id, field.field_id, rel
                            ));
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    warnings
}

/// Merge team Jira override on top of user default.
fn resolve_team_atlassian(default: &AtlassianConfig, team: &TeamConfig) -> AtlassianConfig {
    let Some(ref overlay) = team.atlassian else {
        return default.clone();
    };
    let mut jira = default.clone();
    apply_team_atlassian_override(&mut jira, overlay);
    jira
}

/// Apply a partial team Jira override onto a full `AtlassianConfig`.
/// Only `Some` fields override the base.
pub fn apply_team_atlassian_override(base: &mut AtlassianConfig, overlay: &TeamAtlassianOverride) {
    if let Some(ref v) = overlay.base_url {
        base.base_url.clone_from(v);
    }
    if let Some(ref v) = overlay.default_project {
        base.default_project.clone_from(v);
    }
    if overlay.email.is_some() {
        base.email.clone_from(&overlay.email);
    }
    if overlay.credential_command.is_some() {
        base.credential_command
            .clone_from(&overlay.credential_command);
    }
    if overlay.credential_store.is_some() {
        base.credential_store.clone_from(&overlay.credential_store);
    }
    if overlay.credential_key.is_some() {
        base.credential_key.clone_from(&overlay.credential_key);
    }
    if overlay.auth_method.is_some() {
        base.auth_method.clone_from(&overlay.auth_method);
    }
    if overlay.oauth_client_id.is_some() {
        base.oauth_client_id.clone_from(&overlay.oauth_client_id);
    }
    if overlay.oauth_client_secret.is_some() {
        base.oauth_client_secret
            .clone_from(&overlay.oauth_client_secret);
    }
}

/// Compute the extra granular OAuth scope sets required by the source kinds
/// these teams use (Confluence tasks, Agile boards).
pub fn extra_scopes_for<'a>(
    teams: impl IntoIterator<Item = &'a TeamConfig>,
) -> crate::atlassian::oauth::ExtraScopes {
    let mut extra = crate::atlassian::oauth::ExtraScopes::default();
    for team in teams {
        // Duty sources count too: scopes are minted at `do-next auth` time,
        // but on-call status changes daily.
        let duty_sources = team
            .grafana
            .iter()
            .flat_map(|grafana| &grafana.on_duty_sources);
        for source in team.sources.iter().chain(duty_sources) {
            match source.kind {
                SourceKind::Confluence => extra.confluence = true,
                SourceKind::Board | SourceKind::Backlog => extra.board = true,
                // A standup reads Confluence tasks and page versions, both
                // covered by the existing granular Confluence scope set — no
                // new scope, so no risk to the existing consent screen.
                SourceKind::Standup => {
                    let filters = source.standup.clone().unwrap_or_default();
                    if filters.includes(types::StandupBackend::ConfluenceTasks)
                        || filters.includes(types::StandupBackend::ConfluencePages)
                    {
                        extra.confluence = true;
                    }
                }
                // GitLab authenticates with its own personal access token, not
                // Atlassian OAuth.
                SourceKind::Jira | SourceKind::Gitlab => {}
            }
        }
    }
    extra
}

/// Expand `~` prefix to the user's home directory.
pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(path)
}
