pub mod credentials;
pub mod hidden;
pub mod types;
pub mod updates;

use anyhow::{Context, Result, anyhow};
use std::path::{Path, PathBuf};

use types::{
    Config, ConfluenceConfig, JiraConfig, ResolvedTeam, SourceKind, TeamConfig, TeamJiraOverride,
    TeamRef,
};

/// Result of loading user config + all team configs.
pub struct LoadedConfig {
    pub config: Config,
    pub teams: Vec<ResolvedTeam>,
    /// Non-fatal errors from team configs that failed to load.
    pub load_errors: Vec<String>,
}

/// Load user configuration and resolve all team configs.
pub fn load() -> Result<LoadedConfig> {
    let user_path = user_config_path()?;

    let config: Config = if user_path.exists() {
        load_file(&user_path)?
    } else {
        Config::default()
    };

    let mut teams = Vec::new();
    let mut load_errors = Vec::new();
    for team_ref in &config.teams {
        match load_team_config(team_ref) {
            Ok((team_config, warnings)) => {
                for w in warnings {
                    load_errors.push(format!("team '{}': {w}", team_ref.id));
                }
                let jira = resolve_team_jira(&config.jira, &team_config);
                let confluence = resolve_team_confluence(
                    &jira,
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
                teams.push(ResolvedTeam {
                    id: team_ref.id.clone(),
                    path: team_ref.path.clone(),
                    config: team_config,
                    jira,
                    confluence,
                    open_slack_in_app,
                    slack_team_id,
                });
            }
            Err(e) => {
                load_errors.push(format!("team '{}': {e:#}", team_ref.id));
            }
        }
    }

    Ok(LoadedConfig {
        config,
        teams,
        load_errors,
    })
}

pub fn user_config_path() -> Result<PathBuf> {
    Ok(dirs::config_dir()
        .context("Cannot determine config directory")?
        .join("do-next")
        .join("config.json5"))
}

fn load_file<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read config file: {}", path.display()))?;
    json5::from_str(&content)
        .with_context(|| format!("Failed to parse config file: {}", path.display()))
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
            (r#"sprint: "active","#, types::SprintSelector::Active),
            (r#"sprint: "all","#, types::SprintSelector::All),
            ("sprint: 137,", types::SprintSelector::Id(137)),
            ("", types::SprintSelector::Active), // omitted → default
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
                sprint: types::SprintSelector::Id(137),
                swimlanes: Some(types::SwimlaneConfig::Auto),
            },
            types::BoardFilters {
                board_id: 7,
                sprint: types::SprintSelector::All,
                swimlanes: Some(types::SwimlaneConfig::Queries {
                    lanes: vec![types::QueryLane {
                        name: "Expedite".into(),
                        jql: "priority = Highest".into(),
                    }],
                    everything_else: false,
                }),
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
        let jira = JiraConfig {
            base_url: "https://acme.atlassian.net".into(),
            email: Some("me@acme.com".into()),
            ..Default::default()
        };
        let conf = resolve_team_confluence(&jira, None, None);
        assert_eq!(conf, jira);
    }

    #[test]
    fn confluence_override_precedence_is_team_over_user_over_jira() {
        let jira = JiraConfig {
            base_url: "https://acme.atlassian.net".into(),
            email: Some("me@acme.com".into()),
            ..Default::default()
        };
        let user = ConfluenceConfig {
            base_url: Some("https://wiki.acme.com".into()),
            credential_key: Some("user-key".into()),
            ..Default::default()
        };
        let team = ConfluenceConfig {
            credential_key: Some("team-key".into()),
            ..Default::default()
        };
        let conf = resolve_team_confluence(&jira, Some(&user), Some(&team));
        assert_eq!(conf.base_url, "https://wiki.acme.com");
        assert_eq!(conf.credential_key.as_deref(), Some("team-key"));
        assert_eq!(conf.email.as_deref(), Some("me@acme.com"));
    }
}

fn validate_source_config(source: &types::SourceConfig) -> Result<()> {
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
                    "source '{}': `board` filters are only valid with `kind: \"board\"`",
                    source.id
                ));
            }
        }
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
                    "source '{}': `board` filters are only valid with `kind: \"board\"`",
                    source.id
                ));
            }
            if let Some(filters) = &source.confluence {
                validate_confluence_filters(&source.id, filters)?;
            }
        }
        SourceKind::Board => validate_board_source(source)?,
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
/// a `JiraConfig` so `credentials::resolve_auth` applies unchanged.
fn resolve_team_confluence(
    effective_jira: &JiraConfig,
    user: Option<&ConfluenceConfig>,
    team: Option<&ConfluenceConfig>,
) -> JiraConfig {
    let mut conf = effective_jira.clone();
    for overlay in [user, team].into_iter().flatten() {
        apply_confluence_override(&mut conf, overlay);
    }
    conf
}

fn apply_confluence_override(base: &mut JiraConfig, overlay: &ConfluenceConfig) {
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
fn resolve_team_jira(default: &JiraConfig, team: &TeamConfig) -> JiraConfig {
    let Some(ref overlay) = team.jira else {
        return default.clone();
    };
    let mut jira = default.clone();
    apply_team_jira_override(&mut jira, overlay);
    jira
}

/// Apply a partial team Jira override onto a full `JiraConfig`.
/// Only `Some` fields override the base.
pub fn apply_team_jira_override(base: &mut JiraConfig, overlay: &TeamJiraOverride) {
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

/// Expand `~` prefix to the user's home directory.
pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(path)
}

