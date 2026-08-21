//! `do-next check [--online]` — validate the configuration.
//!
//! Two levels, because they cost different things. The offline pass parses
//! `config.json5` and every team config and prints what resolved, touching no
//! credential store — so it runs headless, which is the whole point when the
//! keyring needs a terminal. The online pass adds one cheap Jira round trip
//! per source, which is what actually catches a mistyped status, a wrong
//! project key or malformed JQL: offline they are all just strings.

use std::collections::{HashMap, HashSet};

use anyhow::Result;

use crate::config::{
    self,
    types::{ResolvedTeam, SourceConfig, SourceKind, SprintSelector, SwimlaneConfig},
};
use crate::jira::JiraClient;

pub async fn run(loaded: &config::LoadedConfig, online: bool) -> Result<()> {
    let mut failures = report_offline(loaded);

    if online {
        failures += report_online(loaded).await;
    } else {
        println!("\n(offline check — add `--online` to run every source against Jira)");
    }

    if failures > 0 {
        anyhow::bail!(
            "config check found {failures} problem{}",
            if failures == 1 { "" } else { "s" }
        );
    }
    println!("\nno problems found");
    Ok(())
}

/// Print what the config resolved to. Returns the number of problems found:
/// load errors the loader collected plus the structural checks below.
fn report_offline(loaded: &config::LoadedConfig) -> usize {
    match config::user_config_path() {
        Ok(path) => println!("config:  {}", path.display()),
        Err(e) => println!("config:  <unknown location: {e}>"),
    }
    if let Some(company) = &loaded.config.company {
        let teams: Vec<&str> = company
            .teams
            .iter()
            .map(config::types::CompanyTeamSelection::id)
            .collect();
        println!(
            "company: {} (teams: {})",
            config::expand_tilde(&company.path).display(),
            if teams.is_empty() {
                "none selected".to_string()
            } else {
                teams.join(", ")
            }
        );
    }

    let mut problems = 0;
    for team in &loaded.teams {
        println!();
        println!("team '{}' — {}", team.id, team.path);
        println!(
            "  atlassian:  {} (project {}, auth {})",
            or_unset(&team.atlassian.base_url),
            or_unset(&team.atlassian.default_project),
            team.atlassian.auth_method.as_deref().unwrap_or("basic"),
        );
        // Duty sources count as well: the on-call view can be toggled on at
        // runtime, so its Confluence connection has to hold up too.
        if team.uses_confluence() {
            let shared = team.confluence == team.atlassian;
            println!(
                "  confluence: {}{}",
                or_unset(&team.confluence.base_url),
                if shared {
                    " (shares the Atlassian auth)"
                } else {
                    ""
                }
            );
        }
        if team.uses_gitlab() {
            println!(
                "  gitlab:     {} ({})",
                team.gitlab.base_url,
                team.gitlab.auth_method.as_deref().unwrap_or("token")
            );
        }
        if let Some(grafana) = &team.grafana {
            println!(
                "  grafana:    {} (mode {})",
                grafana.oncall_api_url,
                match grafana.mode {
                    config::types::OnDutyMode::Replace => "replace",
                    config::types::OnDutyMode::Prepend => "prepend",
                }
            );
        }
        if !team.config.views.is_empty() {
            let mut views: Vec<&str> = team.config.views.keys().map(String::as_str).collect();
            views.sort_unstable();
            println!("  views:      {}", views.join(", "));
        }

        print_sources("sources", &team.normal_sources);
        if let Some(grafana) = &team.grafana
            && !grafana.on_duty_sources.is_empty()
        {
            print_sources("on-duty sources", &grafana.on_duty_sources);
        }

        let team_problems = team_problems(team);
        if !team_problems.is_empty() {
            println!("  problems:");
            for p in &team_problems {
                println!("    ! {p}");
            }
            problems += team_problems.len();
        }
    }

    if loaded.teams.is_empty() {
        println!("\nno team resolved");
    }

    if !loaded.load_errors.is_empty() {
        println!();
        println!(
            "{} load error{}:",
            loaded.load_errors.len(),
            if loaded.load_errors.len() == 1 {
                ""
            } else {
                "s"
            }
        );
        for e in &loaded.load_errors {
            println!("  ! {e}");
        }
        problems += loaded.load_errors.len();
    }

    problems
}

fn print_sources(label: &str, sources: &[SourceConfig]) {
    if sources.is_empty() {
        println!("  {label}: none");
        return;
    }
    println!("  {label} (priority order):");
    let width = sources
        .iter()
        .map(|s| s.id.chars().count())
        .max()
        .unwrap_or(0);
    for (i, source) in sources.iter().enumerate() {
        println!(
            "    {:>2}. {:<width$}  {:<10} {}",
            i + 1,
            source.id,
            kind_label(source.kind),
            source_summary(source),
        );
        for (j, sub) in source.subsources.iter().enumerate() {
            println!(
                "        {:<width$}  {:<10} sub {}: {}",
                "",
                "",
                j + 1,
                sub.jql_filter,
            );
        }
    }
}

const fn kind_label(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Jira => "jira",
        SourceKind::Confluence => "confluence",
        SourceKind::Board => "board",
        SourceKind::Backlog => "backlog",
        SourceKind::Gitlab => "gitlab",
        SourceKind::Standup => "standup",
    }
}

/// One line describing what this source actually fetches — the fields that
/// decide its result set, not every field it carries.
fn source_summary(source: &SourceConfig) -> String {
    match source.kind {
        SourceKind::Jira => {
            if source.jql.is_empty() {
                "(no jql)".to_string()
            } else {
                source.jql.clone()
            }
        }
        SourceKind::Board => {
            let Some(board) = &source.board else {
                return "(no `board` block)".to_string();
            };
            let sprint = match board.sprint.unwrap_or_default() {
                SprintSelector::Active => "sprint active".to_string(),
                SprintSelector::All => "all issues".to_string(),
                SprintSelector::Id(id) => format!("sprint {id}"),
            };
            let lanes = match &board.swimlanes {
                None => String::new(),
                Some(SwimlaneConfig::Auto) => ", swimlanes auto".to_string(),
                Some(SwimlaneConfig::Field { field }) => format!(", swimlanes by {field}"),
                Some(SwimlaneConfig::Queries { lanes, .. }) => {
                    format!(", {} query lane(s)", lanes.len())
                }
            };
            format!("board {}, {sprint}{lanes}", board.board_id)
        }
        SourceKind::Backlog => source.board.as_ref().map_or_else(
            || "(no `board` block)".to_string(),
            |board| format!("board {} backlog", board.board_id),
        ),
        SourceKind::Confluence => {
            let Some(filters) = &source.confluence else {
                return "my incomplete tasks (defaults)".to_string();
            };
            let mut parts = Vec::new();
            if !filters.spaces.is_empty() {
                parts.push(format!("spaces {}", filters.spaces.join(",")));
            }
            if !filters.pages.is_empty() {
                parts.push(format!("pages {}", filters.pages.join(",")));
            }
            parts.push(format!(
                "assignee {}",
                filters.assignee.as_deref().unwrap_or("me")
            ));
            parts.push(format!(
                "status {}",
                filters.status.as_deref().unwrap_or("incomplete")
            ));
            parts.join(", ")
        }
        SourceKind::Gitlab => {
            let Some(filters) = &source.gitlab else {
                return "(no `gitlab` block)".to_string();
            };
            let scope = if filters.groups.is_empty() && filters.projects.is_empty() {
                "instance-wide".to_string()
            } else {
                let mut s = filters.groups.clone();
                s.extend(filters.projects.clone());
                s.join(",")
            };
            let role = match filters.role {
                config::types::GitlabRole::Reviewer => "reviewer",
                config::types::GitlabRole::Assignee => "assignee",
                config::types::GitlabRole::Author => "author",
                config::types::GitlabRole::Any => "anyone",
            };
            format!("{role} / {} in {scope}", filters.state.as_str())
        }
        SourceKind::Standup => {
            let schedule = source
                .standup
                .as_ref()
                .map(|f| f.schedule.clone())
                .unwrap_or_default();
            format!("{} at {}", schedule.days.join("/"), schedule.time)
        }
    }
}

/// Structural checks the loader does not make — the ones whose only other
/// symptom is an error row inside the running TUI.
fn team_problems(team: &ResolvedTeam) -> Vec<String> {
    let mut problems = Vec::new();
    let duty = team
        .grafana
        .iter()
        .flat_map(|g| g.on_duty_sources.iter().map(|s| ("on-duty source", s)));

    for (label, source) in team
        .normal_sources
        .iter()
        .map(|s| ("source", s))
        .chain(duty)
    {
        let at = format!("{label} '{}'", source.id);
        match source.kind {
            SourceKind::Jira if source.jql.is_empty() => {
                problems.push(format!("{at}: kind `jira` with an empty `jql`"));
            }
            SourceKind::Board | SourceKind::Backlog if source.board.is_none() => {
                problems.push(format!(
                    "{at}: kind `{}` needs a `board` block with a `board_id`",
                    kind_label(source.kind)
                ));
            }
            SourceKind::Gitlab if source.gitlab.is_none() => {
                problems.push(format!("{at}: kind `gitlab` needs a `gitlab` block"));
            }
            _ => {}
        }

        // A parent `jql` that already sorts cannot be wrapped in parentheses
        // and ANDed with a subsource filter — Jira rejects the result.
        if !source.subsources.is_empty() && source.jql.to_uppercase().contains("ORDER BY") {
            problems.push(format!(
                "{at}: `jql` contains ORDER BY while subsources are defined — \
                 the combined query would be invalid"
            ));
        }

        if let Some(view) = &source.view_mode
            && !team.config.views.contains_key(view)
        {
            problems.push(format!(
                "{at}: view_mode '{view}' is not defined in this team's `views`"
            ));
        }
    }

    let mut seen: HashSet<&str> = HashSet::new();
    for source in &team.normal_sources {
        if !seen.insert(source.id.as_str()) {
            problems.push(format!("duplicate source id '{}'", source.id));
        }
    }

    problems
}

const fn or_unset(value: &str) -> &str {
    if value.is_empty() { "<unset>" } else { value }
}

/// Run every Jira-backed source against Jira. Returns the failure count.
///
/// Credentials are resolved here rather than by the caller: `check` returns
/// before the launch path builds its clients, precisely so the offline mode
/// never reaches for the keyring.
async fn report_online(loaded: &config::LoadedConfig) -> usize {
    println!();
    println!("online checks:");

    let mut failures = 0;
    let mut clients: HashMap<String, JiraClient> = HashMap::new();

    for team in &loaded.teams {
        println!("  team '{}'", team.id);

        let url = team.atlassian.base_url.clone();
        if !clients.contains_key(&url) {
            match config::credentials::resolve_atlassian_auth(&team.atlassian)
                .and_then(|auth| JiraClient::new(url.clone(), auth))
            {
                Ok(client) => {
                    clients.insert(url.clone(), client);
                }
                Err(e) => {
                    println!("    ! Atlassian auth unavailable: {e:#}");
                    failures += 1;
                    continue;
                }
            }
        }
        let client = &clients[&url];

        let duty = team
            .grafana
            .iter()
            .flat_map(|g| g.on_duty_sources.iter().map(|s| ("on-duty ", s)));
        for (prefix, source) in team.normal_sources.iter().map(|s| ("", s)).chain(duty) {
            failures += check_source(client, prefix, source).await;
        }
    }

    failures
}

/// Probe one source. Prints a line per query it makes; returns how many of
/// them failed.
async fn check_source(client: &JiraClient, prefix: &str, source: &SourceConfig) -> usize {
    let label = format!("{prefix}{}", source.id);
    match source.kind {
        SourceKind::Jira => {
            if source.jql.is_empty() {
                report(&label, Err(&anyhow::anyhow!("empty `jql`")));
                return 1;
            }
            if source.subsources.is_empty() {
                let outcome = count(client, &source.jql).await;
                let failed = usize::from(outcome.is_err());
                report(&label, outcome.as_deref());
                return failed;
            }
            let mut failures = 0;
            for (i, sub) in source.subsources.iter().enumerate() {
                let jql = format!("({}) AND ({})", source.jql, sub.jql_filter);
                let outcome = count(client, &jql).await;
                failures += usize::from(outcome.is_err());
                report(&format!("{label} · sub {}", i + 1), outcome.as_deref());
            }
            failures
        }
        SourceKind::Board | SourceKind::Backlog => {
            let Some(board) = &source.board else {
                report(&label, Err(&anyhow::anyhow!("no `board` block")));
                return 1;
            };
            let mut failures = 0;
            match client.get_board_configuration(board.board_id).await {
                Ok(config) => {
                    let detail = format!(
                        "board {} \"{}\" ({}, {} columns)",
                        config.id,
                        config.name,
                        config.board_type.label(),
                        config.column_config.columns.len()
                    );
                    report(&label, Ok(detail.as_str()));
                }
                Err(e) => {
                    report(&label, Err(&e));
                    failures += 1;
                }
            }
            // Query swimlanes are JQL too, and a broken lane is invisible
            // until the board silently renders laneless.
            if let Some(SwimlaneConfig::Queries { lanes, .. }) = &board.swimlanes {
                for lane in lanes {
                    let outcome = count(client, &lane.jql).await;
                    failures += usize::from(outcome.is_err());
                    report(
                        &format!("{label} · lane '{}'", lane.name),
                        outcome.as_deref(),
                    );
                }
            }
            failures
        }
        SourceKind::Confluence | SourceKind::Gitlab | SourceKind::Standup => {
            println!(
                "    {:<28} skipped — {} sources are not checked online yet",
                label,
                kind_label(source.kind)
            );
            0
        }
    }
}

/// Hit count for a query — or, where no count is to be had, whether the query
/// is valid at all.
///
/// The count endpoint is asked first because a number is the more useful
/// answer, but it is never the verdict: an instance that lacks it, or answers
/// it oddly, must not make every source in the config look broken. A failure
/// there falls through to the ordinary search path, whose error — if any — is
/// the one worth printing, because that is the path the app itself uses.
async fn count(client: &JiraClient, jql: &str) -> Result<String> {
    match client.approximate_count(jql).await {
        Ok(Some(n)) => return Ok(format!("{n} hit{}", if n == 1 { "" } else { "s" })),
        Ok(None) => {}
        Err(e) => log::debug!("approximate-count unavailable, falling back to a search: {e:#}"),
    }
    let keys = client.fetch_jql_page_keys(jql, 1).await?;
    Ok(format!(
        "valid ({}; no count available on this instance)",
        if keys.is_empty() {
            "no hits on the first page"
        } else {
            "has hits"
        }
    ))
}

fn report(label: &str, outcome: Result<&str, &anyhow::Error>) {
    match outcome {
        Ok(detail) => println!("    {label:<28} {detail}"),
        Err(e) => println!("    {label:<28} ERROR {e:#}"),
    }
}
