mod config;
mod events;
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
    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        shell: Shell,
    },
}

#[tokio::main]
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
        tui::onboarding::run_auth_reset(&mut loaded.config).context("Auth reset failed")?;
        return Ok(());
    }

    // Run onboarding if no config at all (first run)
    if loaded.config.jira.base_url.is_empty() && loaded.config.teams.is_empty() {
        loaded = tui::onboarding::run_onboarding().context("Onboarding failed")?;
    }

    // Config exists but no team refs — interactive team setup
    if loaded.config.teams.is_empty() {
        loaded =
            tui::onboarding::run_team_setup(&mut loaded.config).context("Team setup failed")?;
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
    let mut clients: std::collections::HashMap<String, jira::JiraClient> =
        std::collections::HashMap::new();
    for team in &loaded.teams {
        let url = &team.jira.base_url;
        if !clients.contains_key(url) {
            let auth = config::credentials::resolve_auth(&team.jira)
                .with_context(|| format!("Failed to resolve auth for team '{}'", team.id))?;
            let client = jira::JiraClient::new(url.clone(), auth)
                .with_context(|| format!("Failed to create Jira client for team '{}'", team.id))?;
            clients.insert(url.clone(), client);
        }
    }

    // For subcommands, use the first team's client (or default jira).
    let default_client = if let Some(first_team) = loaded.teams.first() {
        clients
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
        Some(Commands::Auth | Commands::Completions { .. }) => {
            unreachable!("handled before credential resolution")
        }
        None => {
            tui::run(loaded, clients).await?;
        }
    }

    Ok(())
}
