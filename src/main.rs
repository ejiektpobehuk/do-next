mod config;
mod confluence;
mod datetime;
mod events;
mod gitlab;
mod grafana;
mod http;
mod items;
mod jira;
mod oauth;
mod sources;
mod standup;
mod startup;
mod subcommands;
mod tui;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;

#[derive(Parser)]
#[command(name = "do-next", about = "Pick your next Jira task")]
struct Cli {
    /// Write debug log to this file (e.g. --log /tmp/do-next.log)
    #[arg(long, value_name = "FILE")]
    log: Option<std::path::PathBuf>,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Add a comment to a Jira issue
    Comment {
        /// Issue key (e.g. PROJ-123). Omit to comment on active task.
        issue_key: Option<String>,
        /// Skip showing comment history before composing
        #[arg(long)]
        no_history: bool,
    },
    /// Validate the configuration and report what it resolved to
    ///
    /// Offline by default: no credentials are touched, so it works headless.
    Check {
        /// Also run every source's query against Jira and report hit counts
        #[arg(long)]
        online: bool,
    },
    /// Read-only lookups against Jira — the values a team config needs
    Inspect {
        /// Which team's Jira connection to query (default: the first team)
        #[arg(long, global = true, value_name = "TEAM_ID")]
        team: Option<String>,
        /// Print machine-readable JSON instead of a table
        #[arg(long, global = true)]
        json: bool,
        #[command(subcommand)]
        what: subcommands::inspect::What,
    },
    /// Reconfigure Jira authentication
    Auth,
    /// Manage the company config repo (shared connection + team catalog)
    Company {
        #[command(subcommand)]
        action: CompanyAction,
    },
    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        shell: Shell,
    },
}

#[derive(Subcommand)]
enum CompanyAction {
    /// Join a company config repo (git URL or local path)
    Join { source: String },
    /// Change which company teams are active and toggle their backlog tabs
    Teams,
}

/// Pre-resolved Confluence credentials, keyed by base URL. A URL missing from
/// the map shares its team's Jira auth handle instead (see
/// [`confluence_shares_jira`]).
type ConfluenceAuths = std::collections::HashMap<String, jira::auth::Auth>;

/// True when a team's sources include Confluence. Normal *and* on-duty sources
/// count: the duty view can be toggled on at runtime (`D`), so its clients must
/// exist up front.
fn needs_confluence(team: &config::types::ResolvedTeam) -> bool {
    let duty_sources = team
        .grafana
        .iter()
        .flat_map(|grafana| &grafana.on_duty_sources);
    team.normal_sources
        .iter()
        .chain(duty_sources)
        .any(|s| s.kind == config::types::SourceKind::Confluence)
}

/// True when a team's effective Confluence connection is its Jira one, so the
/// two clients can share an auth handle — keeping OAuth refresh coordinated and
/// saving a second credential lookup.
fn confluence_shares_jira(team: &config::types::ResolvedTeam) -> bool {
    team.confluence == team.jira && std::env::var("DO_NEXT_CONFLUENCE_API_TOKEN").is_err()
}

/// The Jira connection an `inspect` lookup runs against, plus the project key
/// its project-scoped arguments default to.
///
/// A lookup asks about one Jira site, so it follows one team: `--team` names
/// it, and without one the first team stands in — the same team whose client
/// the other subcommands use.
fn inspect_target(
    loaded: &config::LoadedConfig,
    clients: &sources::Clients,
    default_client: &jira::JiraClient,
    team: Option<&str>,
) -> Result<(jira::JiraClient, String)> {
    let Some(wanted) = team else {
        let project = loaded.teams.first().map_or_else(
            || loaded.config.jira.default_project.clone(),
            |t| t.jira.default_project.clone(),
        );
        return Ok((default_client.clone(), project));
    };

    let team = loaded
        .teams
        .iter()
        .find(|t| t.id == wanted)
        .with_context(|| {
            let ids: Vec<&str> = loaded.teams.iter().map(|t| t.id.as_str()).collect();
            format!("no team '{wanted}' (configured: {})", ids.join(", "))
        })?;
    let client = clients
        .jira
        .get(&team.jira.base_url)
        .cloned()
        .with_context(|| format!("No Jira client for team '{}'", team.id))?;
    Ok((client, team.jira.default_project.clone()))
}

#[tokio::main]
#[allow(clippy::too_many_lines)]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Startup progress goes to stderr; shell completions are consumed by the
    // shell (`eval "$(do-next completions zsh)"`), so they get no decoration.
    startup::set_enabled(!matches!(cli.command, Some(Commands::Completions { .. })));

    if let Some(ref log_path) = cli.log {
        use simplelog::{Config, LevelFilter, WriteLogger};
        let file = std::fs::File::create(log_path)
            .with_context(|| format!("Failed to create log file: {}", log_path.display()))?;
        WriteLogger::init(LevelFilter::Debug, Config::default(), file)
            .context("Failed to initialise logger")?;
        log::info!("do-next starting, logging to {}", log_path.display());
    }

    // Load config. On failure the step is dropped without a verdict, leaving
    // the error to stand on its own.
    let step = startup::Step::start("loading config");
    let mut loaded = config::load().context("Failed to load configuration")?;
    let team_count = loaded.teams.len();
    if team_count == 0 {
        if loaded.load_errors.is_empty() {
            step.skip("no teams configured yet");
        } else {
            // The errors themselves are printed by the bail further down.
            step.warn("config loaded, but no team could be resolved");
        }
    } else {
        step.done(format!(
            "config loaded ({team_count} team{})",
            if team_count == 1 { "" } else { "s" }
        ));
    }

    // Shell completions — no config needed.
    if let Some(Commands::Completions { shell }) = &cli.command {
        clap_complete::generate(
            *shell,
            &mut Cli::command(),
            "do-next",
            &mut std::io::stdout(),
        );
        return Ok(());
    }

    // `check` runs before everything interactive: it must never prompt, never
    // rewrite the config, and — offline — never reach for a credential store,
    // which is what lets it run headless. `--online` resolves the Jira auth it
    // needs itself, for the same reason.
    if let Some(Commands::Check { online }) = &cli.command {
        return subcommands::check::run(&loaded, *online).await;
    }

    // Auth reset runs before credential resolution (auth may currently be broken).
    if matches!(&cli.command, Some(Commands::Auth)) {
        // Teams with confluence / board sources need the matching granular
        // OAuth scopes (classic Jira scopes don't cover those APIs).
        let extra_scopes = config::extra_scopes_for(loaded.teams.iter().map(|t| &t.config));
        let effective_jira = loaded.config.jira.clone();
        tui::onboarding::run_auth_reset(&effective_jira, &mut loaded.raw, extra_scopes)
            .context("Auth reset failed")?;
        // Teams using the on-call view also carry a Grafana OnCall token;
        // offer to (re)configure it — this is the rotation path.
        let grafana_teams = grafana::grafana_api_urls(&loaded.teams);
        if !grafana_teams.is_empty()
            && tui::onboarding::prompt_yes_no(
                "\nAlso configure the Grafana OnCall token (on-call view)? [y/N]: ",
                false,
            )?
        {
            for target in &grafana_teams {
                tui::onboarding::grafana::setup_grafana_token(target, &mut loaded.raw)
                    .await
                    .context("Grafana token setup failed")?;
            }
        }
        // Same for teams with GitLab sources — the merge-request credentials.
        let gitlab_teams = gitlab::gitlab_api_urls(&loaded.teams);
        if !gitlab_teams.is_empty()
            && tui::onboarding::prompt_yes_no(
                "\nAlso set up GitLab sign-in (merge requests)? [y/N]: ",
                false,
            )?
        {
            for target in &gitlab_teams {
                tui::onboarding::gitlab::setup_gitlab_token(target, &mut loaded.raw)
                    .await
                    .context("GitLab credential setup failed")?;
            }
        }
        return Ok(());
    }

    // Company management also runs before credential resolution: joining sets
    // auth up itself, and team selection must work while auth is broken.
    // Both operate on the raw on-disk config, never the merged view.
    if let Some(Commands::Company { action }) = &cli.command {
        match action {
            CompanyAction::Join { source } => {
                tui::onboarding::run_company_join_command(&mut loaded.raw, source)
                    .context("Company join failed")?;
            }
            CompanyAction::Teams => {
                tui::onboarding::run_company_teams_command(&mut loaded.raw)
                    .context("Company team selection failed")?;
            }
        }
        return Ok(());
    }

    // Company config repo: offer to pull upstream updates before the TUI
    // starts so manifest/team changes apply to this run. TUI launch only —
    // quick subcommands shouldn't block on a network fetch. The fetch runs
    // under its own deadline and every failure is reported and stepped over:
    // a repo hosted behind a VPN that is currently off must not hold the app
    // hostage, it just means this run uses the checkout as it stands.
    if cli.command.is_none()
        && let Some(company) = &loaded.config.company
    {
        use config::updates::{STARTUP_FETCH_TIMEOUT, check_repo};

        let repo = config::expand_tilde(&company.path);
        let step = startup::Step::start("checking company config for updates (git fetch)");
        let status = check_repo(&repo, STARTUP_FETCH_TIMEOUT);
        match status {
            None => step.skip("company config is not a git checkout — nothing to update"),
            Some(status) => {
                let behind = status.behind;
                let plural = if behind == 1 { "" } else { "s" };
                match (behind, status.unreachable_reason()) {
                    (0, None) => step.done("company config up to date"),
                    (0, Some(why)) => step.warn(format!(
                        "cannot reach the company config remote ({why}) — \
                         using the current checkout"
                    )),
                    // Pending updates we know about only from the last
                    // successful fetch: pulling them needs the remote, so
                    // report and move on rather than prompt for a doomed pull.
                    (_, Some(why)) => step.warn(format!(
                        "company config was {behind} commit{plural} behind at the last check, \
                         remote unreachable now ({why}) — using the current checkout"
                    )),
                    (_, None) => {
                        // Report before prompting: the verdict explains the question.
                        step.warn(format!("company config is {behind} commit{plural} behind"));
                        let pull = tui::onboarding::prompt_yes_no(
                            &format!(
                                "Company config has {behind} update{plural}. Pull now? [Y/n]: "
                            ),
                            true,
                        )
                        .unwrap_or(false);
                        if pull {
                            match config::updates::pull_ff_only(&repo) {
                                Ok(()) => {
                                    let step = startup::Step::start("reloading config");
                                    loaded =
                                        config::load().context("Failed to reload configuration")?;
                                    step.done("config reloaded");
                                }
                                Err(e) => {
                                    eprintln!(
                                        "warning: {e:#}; continuing with the current checkout"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Run onboarding if no config at all (first run). A configured company
    // whose manifest failed to load must NOT fall into onboarding (it would
    // overwrite the user's config) — its errors surface via the bail below.
    if loaded.config.jira.base_url.is_empty()
        && loaded.config.teams.is_empty()
        && loaded.config.company.is_none()
    {
        loaded = tui::onboarding::run_onboarding().context("Onboarding failed")?;
    }

    // Config exists but no team refs (manual or company) — interactive team
    // setup. Operates on the raw config: it rewrites config.json5, which must
    // never absorb merged company values.
    if !config::has_team_refs(&loaded.config) {
        loaded = tui::onboarding::run_team_setup(&mut loaded.raw).context("Team setup failed")?;
    }

    // Team refs exist but every team failed to load — surface errors and bail.
    // Falling through to onboarding here would corrupt the user's existing config.
    if loaded.teams.is_empty() {
        for e in &loaded.load_errors {
            eprintln!("error: {e}");
        }
        anyhow::bail!("no teams loaded successfully; fix the errors above and retry");
    }

    // Grafana OnCall check: teams with a `grafana` block get their sources
    // replaced by `on_duty_sources` while the user is on call. TUI launch
    // only — subcommands shouldn't block on a network check. Must run before
    // client construction so Confluence needs and fetches see the effective
    // source set.
    if cli.command.is_none() {
        // Teams that want the on-call view (Grafana) or have GitLab sources but
        // no token anywhere get an interactive setup offer first (like the Jira
        // auth prompts). Declining skips for this launch only; `do-next auth`
        // works any time.
        //
        // Both probes are independent keyring lookups, so they run together: a
        // slow or locked secret service is waited on once instead of twice.
        // Only the *reads* overlap — the setup offers below stay strictly
        // serial, because two prompts cannot share one terminal, and each may
        // rewrite the config the next check reads.
        let has_grafana = loaded.teams.iter().any(|t| t.grafana.is_some());
        let has_gitlab = loaded
            .teams
            .iter()
            .any(config::types::ResolvedTeam::uses_gitlab);

        let (missing_grafana, missing_gitlab) = {
            let (group, [grafana_slot, gitlab_slot]) = startup::Group::start([
                has_grafana.then_some("reading Grafana OnCall token"),
                has_gitlab.then_some("reading GitLab credentials"),
            ]);
            let teams = &loaded.teams;
            let probes = std::thread::scope(|scope| {
                let grafana_probe = scope.spawn(|| {
                    let missing = grafana::teams_missing_token(teams);
                    if missing.is_empty() {
                        grafana_slot.done("Grafana OnCall token found");
                    } else {
                        grafana_slot.warn("no Grafana OnCall token stored");
                    }
                    missing
                });
                let gitlab_probe = scope.spawn(|| {
                    let missing = gitlab::teams_missing_token(teams);
                    if missing.is_empty() {
                        gitlab_slot.done("GitLab credentials found");
                    } else {
                        gitlab_slot.warn("no GitLab credentials stored");
                    }
                    missing
                });
                (grafana_probe.join(), gitlab_probe.join())
            });
            // Dropping the group collapses the block, so the setup offers below
            // start on a clean line.
            drop(group);
            (
                probes
                    .0
                    .map_err(|_| anyhow::anyhow!("Grafana token probe panicked"))?,
                probes
                    .1
                    .map_err(|_| anyhow::anyhow!("GitLab credential probe panicked"))?,
            )
        };

        if !missing_grafana.is_empty() {
            let mut configured = false;
            for target in &missing_grafana {
                match tui::onboarding::grafana::setup_grafana_token(target, &mut loaded.raw).await {
                    Ok(tui::onboarding::SetupOutcome::Configured) => configured = true,
                    Ok(_) => {}
                    // A failed setup must not block the launch; the on-call
                    // check below reports the still-missing token.
                    Err(e) => eprintln!("warning: Grafana token setup failed: {e:#}"),
                }
            }
            if configured {
                let step = startup::Step::start("reloading config");
                loaded = config::load().context("Failed to reload configuration")?;
                step.done("config reloaded");
            }
        }

        // A declined or failed GitLab setup must not block the launch either —
        // the GitLab sources report the missing token as their own error rows.
        if !missing_gitlab.is_empty() {
            let mut configured = false;
            for target in &missing_gitlab {
                match tui::onboarding::gitlab::setup_gitlab_token(target, &mut loaded.raw).await {
                    Ok(tui::onboarding::SetupOutcome::Configured) => configured = true,
                    Ok(_) => {}
                    Err(e) => eprintln!("warning: GitLab credential setup failed: {e:#}"),
                }
            }
            if configured {
                let step = startup::Step::start("reloading config");
                loaded = config::load().context("Failed to reload configuration")?;
                step.done("config reloaded");
            }
        }

        // The on-call query is a network round trip per team; skipped entirely
        // when no team configures Grafana, which the report says out loud.
        if has_grafana {
            let step = startup::Step::start("asking Grafana OnCall who is on duty");
            let duty_errors = grafana::apply_on_duty_sources(&mut loaded.teams).await;
            let on_duty = loaded.teams.iter().filter(|t| t.on_duty).count();
            if on_duty > 0 {
                step.done(format!(
                    "on call — duty sources active for {on_duty} team{}",
                    if on_duty == 1 { "" } else { "s" }
                ));
            } else if duty_errors.is_empty() {
                step.done("not on call — normal sources");
            } else {
                step.warn("on-call check failed — normal sources");
            }
            loaded.load_errors.extend(duty_errors);
        }
    }

    // Every credential below is an independent blocking lookup — a keyring
    // read, a `credential_command` subprocess, an OAuth token file — so all
    // three run at once and the launch waits out the slowest, not their sum.
    // Only resolution is parallel: building the clients afterwards is pure CPU,
    // and keeping it serial is what lets Confluence borrow Jira's auth handle.
    let mut clients = sources::Clients {
        jira: std::collections::HashMap::new(),
        confluence: std::collections::HashMap::new(),
        gitlab: std::collections::HashMap::new(),
    };
    let needs_gitlab = loaded
        .teams
        .iter()
        .any(config::types::ResolvedTeam::uses_gitlab);
    let resolved = {
        let (group, [jira_slot, confluence_slot, gitlab_slot]) = startup::Group::start([
            Some("resolving Jira credentials"),
            loaded
                .teams
                .iter()
                .any(needs_confluence)
                .then_some("resolving Confluence credentials"),
            needs_gitlab.then_some("resolving GitLab credentials"),
        ]);
        let teams = &loaded.teams;
        let resolved = std::thread::scope(|scope| {
            // One auth per unique Jira base_url across all teams.
            let jira = scope.spawn(|| -> Result<Vec<(String, String, jira::auth::Auth)>> {
                let mut auths: Vec<(String, String, jira::auth::Auth)> = Vec::new();
                for team in teams {
                    let url = &team.jira.base_url;
                    if auths.iter().any(|(seen, _, _)| seen == url) {
                        continue;
                    }
                    let auth =
                        config::credentials::resolve_auth(&team.jira).with_context(|| {
                            format!("Failed to resolve auth for team '{}'", team.id)
                        })?;
                    auths.push((url.clone(), team.id.clone(), auth));
                }
                let count = auths.len();
                jira_slot.done(format!(
                    "Jira ready ({count} instance{})",
                    if count == 1 { "" } else { "s" }
                ));
                Ok(auths)
            });

            // One auth per unique Confluence base_url, but only for teams that
            // define Confluence sources and whose Confluence connection differs
            // from their Jira one — the rest share Jira's auth handle, which
            // costs no lookup at all. The dedup order mirrors the construction
            // loop below, so the same team wins each URL.
            let confluence = scope.spawn(|| -> Result<ConfluenceAuths> {
                let mut needed: Vec<String> = Vec::new();
                let mut own: std::collections::HashMap<String, jira::auth::Auth> =
                    std::collections::HashMap::new();
                for team in teams {
                    if !needs_confluence(team) {
                        continue;
                    }
                    let url = team.confluence.base_url.clone();
                    if needed.contains(&url) {
                        continue;
                    }
                    needed.push(url.clone());
                    if confluence_shares_jira(team) {
                        continue;
                    }
                    let auth = config::credentials::resolve_confluence_auth(&team.confluence)
                        .with_context(|| {
                            format!("Failed to resolve Confluence auth for team '{}'", team.id)
                        })?;
                    own.insert(url, auth);
                }
                let count = needed.len();
                if count == 0 {
                    // No team asked for Confluence — nothing happened, say nothing.
                    confluence_slot.clear();
                } else {
                    confluence_slot.done(format!(
                        "Confluence ready ({count} instance{})",
                        if count == 1 { "" } else { "s" }
                    ));
                }
                Ok(own)
            });

            // One token per unique GitLab instance, for teams whose normal or
            // on-duty sources include a GitLab source (the `D` toggle can add
            // duty sources at runtime). A missing token is not fatal: the
            // source reports it, so the rest of the list still loads.
            let gitlab = scope.spawn(|| {
                let mut auths: Vec<(String, String, gitlab::auth::GitlabAuth)> = Vec::new();
                let mut warnings = Vec::new();
                for team in teams {
                    if !team.uses_gitlab() {
                        continue;
                    }
                    let url = team.gitlab.base_url.clone();
                    if auths.iter().any(|(seen, _, _)| *seen == url) {
                        continue;
                    }
                    let how = if team.gitlab.uses_oauth() {
                        "sign in"
                    } else {
                        "set it up"
                    };
                    match config::credentials::resolve_gitlab_auth(&team.gitlab) {
                        Ok(Some(auth)) => auths.push((url, team.id.clone(), auth)),
                        Ok(None) => warnings.push(format!(
                            "team '{}': no GitLab credentials for {url} \
                             (run `do-next auth` to {how})",
                            team.id
                        )),
                        Err(e) => warnings.push(format!(
                            "team '{}': GitLab credential resolution failed: {e:#}",
                            team.id
                        )),
                    }
                }
                if !warnings.is_empty() {
                    gitlab_slot.warn("GitLab credentials incomplete");
                } else if auths.is_empty() {
                    gitlab_slot.clear();
                } else {
                    let count = auths.len();
                    gitlab_slot.done(format!(
                        "GitLab ready ({count} instance{})",
                        if count == 1 { "" } else { "s" }
                    ));
                }
                (auths, warnings)
            });

            (jira.join(), confluence.join(), gitlab.join())
        });
        // Collapse the block before any warning or error below is printed.
        drop(group);
        resolved
    };
    let jira_auths = resolved
        .0
        .map_err(|_| anyhow::anyhow!("Jira credential resolution panicked"))??;
    let mut confluence_auths = resolved
        .1
        .map_err(|_| anyhow::anyhow!("Confluence credential resolution panicked"))??;
    let (gitlab_auths, gitlab_warnings) = resolved
        .2
        .map_err(|_| anyhow::anyhow!("GitLab credential resolution panicked"))?;

    for (url, team_id, auth) in jira_auths {
        let client = jira::JiraClient::new(url.clone(), auth)
            .with_context(|| format!("Failed to create Jira client for team '{team_id}'"))?;
        clients.jira.insert(url, client);
    }

    for team in &loaded.teams {
        if !needs_confluence(team) {
            continue;
        }
        let url = team.confluence.base_url.clone();
        if clients.confluence.contains_key(&url) {
            continue;
        }
        // Present in the map means this URL resolved its own credentials above;
        // absent means it shares Jira's auth handle, so OAuth refresh stays
        // coordinated across the two clients.
        let client = if let Some(auth) = confluence_auths.remove(&url) {
            confluence::ConfluenceClient::new(&url, auth)
        } else {
            let jira_client = clients
                .jira
                .get(&team.jira.base_url)
                .context("No Jira client for shared Confluence auth")?;
            confluence::ConfluenceClient::from_shared(&url, jira_client.auth_handle())
        }
        .with_context(|| format!("Failed to create Confluence client for team '{}'", team.id))?;
        clients.confluence.insert(url, client);
    }

    for (url, team_id, auth) in gitlab_auths {
        let client = gitlab::GitlabClient::new(&url, auth)
            .with_context(|| format!("Failed to create GitLab client for team '{team_id}'"))?;
        clients.gitlab.insert(url, client);
    }

    for warning in gitlab_warnings {
        eprintln!("warning: {warning}");
    }

    // For subcommands, use the first team's client (or default jira).
    let default_client = if let Some(first_team) = loaded.teams.first() {
        clients
            .jira
            .get(&first_team.jira.base_url)
            .cloned()
            .context("No Jira client available")?
    } else {
        // No teams at all — use default jira config
        let auth = config::credentials::resolve_auth(&loaded.config.jira)
            .context("Failed to resolve Jira authentication")?;
        jira::JiraClient::new(loaded.config.jira.base_url.clone(), auth)
            .context("Failed to create Jira client")?
    };

    match cli.command {
        Some(Commands::Comment {
            issue_key,
            no_history,
        }) => {
            subcommands::comment::run(&default_client, issue_key.as_deref(), no_history).await?;
        }
        Some(Commands::Inspect { team, json, what }) => {
            let (client, project) =
                inspect_target(&loaded, &clients, &default_client, team.as_deref())?;
            subcommands::inspect::run(&client, &project, json, &what).await?;
        }
        Some(
            Commands::Auth
            | Commands::Check { .. }
            | Commands::Company { .. }
            | Commands::Completions { .. },
        ) => {
            unreachable!("handled before credential resolution")
        }
        None => {
            tui::run(loaded, clients).await?;
        }
    }

    Ok(())
}
