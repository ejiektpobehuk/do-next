mod config;
mod confluence;
mod events;
mod items;
mod jira;
mod sources;
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
    /// List all fields on a Jira issue (useful for configuring views)
    Fields {
        /// Issue key (e.g. PROJ-123)
        issue_key: String,
        /// Dump the raw JSON value of a specific field ID
        #[arg(long, value_name = "FIELD_ID")]
        field: Option<String>,
        /// Dump the raw editmeta JSON object for the field specified by --field
        #[arg(long, requires = "field")]
        raw: bool,
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
    /// Change which company teams are active
    Teams,
}

#[tokio::main]
#[allow(clippy::too_many_lines)]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    if let Some(ref log_path) = cli.log {
        use simplelog::{Config, LevelFilter, WriteLogger};
        let file = std::fs::File::create(log_path)
            .with_context(|| format!("Failed to create log file: {}", log_path.display()))?;
        WriteLogger::init(LevelFilter::Debug, Config::default(), file)
            .context("Failed to initialise logger")?;
        log::info!("do-next starting, logging to {}", log_path.display());
    }

    // Load config
    let mut loaded = config::load().context("Failed to load configuration")?;

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

    // Auth reset runs before credential resolution (auth may currently be broken).
    if matches!(&cli.command, Some(Commands::Auth)) {
        // Teams with confluence / board sources need the matching granular
        // OAuth scopes (classic Jira scopes don't cover those APIs).
        let extra_scopes = config::extra_scopes_for(loaded.teams.iter().map(|t| &t.config));
        let effective_jira = loaded.config.jira.clone();
        tui::onboarding::run_auth_reset(&effective_jira, &mut loaded.raw, extra_scopes)
            .context("Auth reset failed")?;
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
    // quick subcommands shouldn't block on a network fetch.
    if cli.command.is_none()
        && let Some(company) = &loaded.config.company
    {
        let repo = config::expand_tilde(&company.path);
        let behind = config::updates::fetch_behind_count(&repo).unwrap_or(0);
        if behind > 0 {
            let plural = if behind == 1 { "" } else { "s" };
            let pull = tui::onboarding::prompt_yes_no(
                &format!("Company config has {behind} update{plural}. Pull now? [Y/n]: "),
                true,
            )
            .unwrap_or(false);
            if pull {
                match config::updates::pull_ff_only(&repo) {
                    Ok(()) => {
                        loaded = config::load().context("Failed to reload configuration")?;
                    }
                    Err(e) => {
                        eprintln!("warning: {e:#}; continuing with the current checkout");
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

    // Build one JiraClient per unique base_url across all teams.
    let mut clients = sources::Clients {
        jira: std::collections::HashMap::new(),
        confluence: std::collections::HashMap::new(),
    };
    for team in &loaded.teams {
        let url = &team.jira.base_url;
        if !clients.jira.contains_key(url) {
            let auth = config::credentials::resolve_auth(&team.jira)
                .with_context(|| format!("Failed to resolve auth for team '{}'", team.id))?;
            let client = jira::JiraClient::new(url.clone(), auth)
                .with_context(|| format!("Failed to create Jira client for team '{}'", team.id))?;
            clients.jira.insert(url.clone(), client);
        }
    }

    // Build one ConfluenceClient per unique Confluence base_url, but only for
    // teams that define confluence sources. When the effective Confluence
    // connection equals the team's Jira one, share the auth handle so OAuth
    // refresh stays coordinated (and no second credential lookup runs).
    for team in &loaded.teams {
        let needs_confluence = team
            .config
            .sources
            .iter()
            .any(|s| s.kind == config::types::SourceKind::Confluence);
        if !needs_confluence {
            continue;
        }
        let url = team.confluence.base_url.clone();
        if clients.confluence.contains_key(&url) {
            continue;
        }
        let same_as_jira =
            team.confluence == team.jira && std::env::var("DO_NEXT_CONFLUENCE_API_TOKEN").is_err();
        let client = if same_as_jira {
            let jira_client = clients
                .jira
                .get(&team.jira.base_url)
                .context("No Jira client for shared Confluence auth")?;
            confluence::ConfluenceClient::from_shared(&url, jira_client.auth_handle())
        } else {
            let auth = config::credentials::resolve_confluence_auth(&team.confluence)
                .with_context(|| {
                    format!("Failed to resolve Confluence auth for team '{}'", team.id)
                })?;
            confluence::ConfluenceClient::new(&url, auth)
        }
        .with_context(|| format!("Failed to create Confluence client for team '{}'", team.id))?;
        clients.confluence.insert(url, client);
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
        Some(Commands::Fields {
            issue_key,
            field,
            raw,
        }) => {
            subcommands::fields::run(&default_client, &issue_key, field.as_deref(), raw).await?;
        }
        Some(Commands::Auth | Commands::Company { .. } | Commands::Completions { .. }) => {
            unreachable!("handled before credential resolution")
        }
        None => {
            tui::run(loaded, clients).await?;
        }
    }

    Ok(())
}
