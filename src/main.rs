mod config;
mod events;
mod jira;
mod sources;
mod subcommands;
mod tui;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

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
    let (mut config, project_override) = config::load().context("Failed to load configuration")?;

    // Auth reset runs before credential resolution (auth may currently be broken).
    if matches!(&cli.command, Some(Commands::Auth)) {
        tui::onboarding::run_auth_reset(&mut config).context("Auth reset failed")?;
        return Ok(());
    }

    // Run onboarding if config is empty
    if config.jira.base_url.is_empty() {
        config = tui::onboarding::run_onboarding().context("Onboarding failed")?;
    }

    // Resolve authentication
    let auth = config::credentials::resolve_auth(&config.jira)
        .context("Failed to resolve Jira authentication")?;

    // Build Jira client
    let client = jira::JiraClient::new(config.jira.base_url.clone(), auth)
        .context("Failed to create Jira client")?;

    match cli.command {
        Some(Commands::Comment {
            issue_key,
            no_history,
        }) => {
            subcommands::comment::run(&client, issue_key.as_deref(), no_history).await?;
        }
        Some(Commands::Fields {
            issue_key,
            field,
            raw,
        }) => {
            subcommands::fields::run(&client, &issue_key, field.as_deref(), raw).await?;
        }
        Some(Commands::Auth) => unreachable!("handled before credential resolution"),
        None => {
            tui::run(config, client, project_override).await?;
        }
    }

    Ok(())
}
