use anyhow::Result;
use crossterm::cursor::MoveUp;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::style::{Color, Print, ResetColor, SetForegroundColor};
use crossterm::terminal::{Clear, ClearType, disable_raw_mode, enable_raw_mode};
use std::io;
use std::io::Write;

use crate::config::types::{Config, JiraConfig};

#[derive(PartialEq)]
enum CredentialChoice {
    File,
    Keyring,
    Command,
    Skip,
}

struct CredentialStatus {
    env_set: bool,
    file_exists: bool,
    keyring_found: bool,
    command: Option<String>,
}

enum ConfigStyle {
    Minimal,
    Template,
}

/// Run the interactive first-run wizard.
/// Returns a fully configured Config (credentials stored per user's choice).
pub fn run_onboarding() -> Result<Config> {
    println!("Welcome to do-next! Let's set up your configuration.\n");

    let base_url = prompt("Jira base URL (e.g. https://jira.example.com): ", None)?;
    let default_project = prompt("Default project key (e.g. PTMT): ", None)?;

    println!();
    let choice = prompt_credential_choice(None, None)?;

    let mut jira_config = JiraConfig {
        base_url: base_url.clone(),
        default_project,
        ..Default::default()
    };

    let config_dir = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine config directory"))?
        .join("do-next");
    std::fs::create_dir_all(&config_dir)?;

    match choice {
        CredentialChoice::File => {
            print_pat_instructions(&base_url);
            let token = prompt_masked("Personal Access Token: ")?;

            let creds_path = config_dir.join("credentials.json5");
            let creds_content = format!("{{ jira: {{ token: \"{token}\" }} }}\n");
            std::fs::write(&creds_path, &creds_content)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&creds_path, std::fs::Permissions::from_mode(0o600))?;
            }
            println!("Credentials written to {}", creds_path.display());
        }

        CredentialChoice::Keyring => {
            check_keyring_available(&base_url)?;
            let entry = keyring::Entry::new("do-next", &base_url)
                .map_err(|e| anyhow::anyhow!("Failed to access keyring: {e}"))?;

            let already_exists = match entry.get_password() {
                Ok(_) => true,
                Err(keyring::Error::NoEntry) => false,
                Err(e) => return Err(anyhow::anyhow!("Keyring error: {e}")),
            };

            if already_exists {
                println!("A token is already stored in the keyring for this URL.");
                let reuse = prompt_yes_no("Use the existing token? [Y/n]: ", true)?;
                if !reuse {
                    print_pat_instructions(&base_url);
                    let token = prompt_masked("Personal Access Token: ")?;
                    entry
                        .set_password(&token)
                        .map_err(|e| anyhow::anyhow!("Failed to store token in keyring: {e}"))?;
                    println!("Token updated in system keyring.");
                }
            } else {
                print_pat_instructions(&base_url);
                let token = prompt_masked("Personal Access Token: ")?;
                entry
                    .set_password(&token)
                    .map_err(|e| anyhow::anyhow!("Failed to store token in keyring: {e}"))?;
                println!("Token stored in system keyring.");
            }

            jira_config.credential_store = Some("keyring".into());
        }

        CredentialChoice::Command => {
            println!("Enter the shell command whose stdout is your Jira token.");
            println!("Examples:  pass show jira/do-next");
            println!("           op read 'op://Private/Jira/credential'");
            println!();
            let cmd = prompt("Credential command: ", None)?;
            jira_config.credential_command = Some(cmd);
        }

        CredentialChoice::Skip => {
            println!();
            println!("Set the following environment variable before running do-next:");
            println!("  DO_NEXT_JIRA_TOKEN=<your-token>");
            println!();
        }
    }

    let config = Config {
        jira: jira_config.clone(),
        ..Default::default()
    };

    println!();
    let config_style = prompt_config_style()?;

    let config_path = config_dir.join("config.json5");
    let json5_content = match config_style {
        ConfigStyle::Minimal => json5::to_string(&config)?,
        ConfigStyle::Template => {
            template_config(&base_url, &config.jira.default_project, &jira_config)
        }
    };
    std::fs::write(&config_path, json5_content)?;
    println!("Config written to {}", config_path.display());

    Ok(config)
}

/// Reconfigure authentication for an existing install without overwriting other config.
pub fn run_auth_reset(config: &mut Config) -> Result<()> {
    if config.jira.base_url.is_empty() {
        return Err(anyhow::anyhow!(
            "No configuration found. Run do-next first to complete initial setup."
        ));
    }

    println!(
        "Reconfiguring Jira authentication for {}",
        config.jira.base_url
    );
    println!();

    let status = probe_credential_status(&config.jira);
    let current = detect_configured_method(&config.jira);
    let choice = prompt_credential_choice(Some(&current), Some(&status))?;

    let config_dir = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine config directory"))?
        .join("do-next");

    // Clear existing auth fields; each branch sets only what it needs.
    config.jira.credential_command = None;
    config.jira.credential_store = None;

    match choice {
        CredentialChoice::File => {
            print_pat_instructions(&config.jira.base_url);
            let token = prompt_masked("Personal Access Token: ")?;

            let creds_path = config_dir.join("credentials.json5");
            let creds_content = format!("{{ jira: {{ token: \"{token}\" }} }}\n");
            std::fs::write(&creds_path, &creds_content)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&creds_path, std::fs::Permissions::from_mode(0o600))?;
            }
            println!("Credentials written to {}", creds_path.display());
        }

        CredentialChoice::Keyring => {
            let key = config
                .jira
                .credential_key
                .as_deref()
                .unwrap_or(&config.jira.base_url)
                .to_string();
            check_keyring_available(&key)?;
            let entry = keyring::Entry::new("do-next", &key)
                .map_err(|e| anyhow::anyhow!("Failed to access keyring: {e}"))?;

            let already_exists = match entry.get_password() {
                Ok(_) => true,
                Err(keyring::Error::NoEntry) => false,
                Err(e) => return Err(anyhow::anyhow!("Keyring error: {e}")),
            };

            if already_exists {
                println!("A token is already stored in the keyring for this URL.");
                let reuse = prompt_yes_no("Use the existing token? [Y/n]: ", true)?;
                if !reuse {
                    print_pat_instructions(&config.jira.base_url.clone());
                    let token = prompt_masked("Personal Access Token: ")?;
                    entry
                        .set_password(&token)
                        .map_err(|e| anyhow::anyhow!("Failed to store token in keyring: {e}"))?;
                    println!("Token updated in system keyring.");
                }
            } else {
                print_pat_instructions(&config.jira.base_url.clone());
                let token = prompt_masked("Personal Access Token: ")?;
                entry
                    .set_password(&token)
                    .map_err(|e| anyhow::anyhow!("Failed to store token in keyring: {e}"))?;
                println!("Token stored in system keyring.");
            }

            config.jira.credential_store = Some("keyring".into());
        }

        CredentialChoice::Command => {
            println!("Enter the shell command whose stdout is your Jira token.");
            println!("Examples:  pass show jira/do-next");
            println!("           op read 'op://Private/Jira/credential'");
            println!();
            let cmd = prompt("Credential command: ", None)?;
            config.jira.credential_command = Some(cmd);
        }

        CredentialChoice::Skip => {
            println!();
            println!("Set the following environment variable before running do-next:");
            println!("  DO_NEXT_JIRA_TOKEN=<your-token>");
            println!();
        }
    }

    // Write updated config back (serializing to minimal JSON5; comments are not preserved).
    let config_path = config_dir.join("config.json5");
    if config_path.exists() {
        println!("Note: config file will be rewritten in minimal format (comments removed).");
    }
    std::fs::create_dir_all(&config_dir)?;
    let json5_content = json5::to_string(&config)?;
    std::fs::write(&config_path, json5_content)?;
    println!("Config updated at {}", config_path.display());

    Ok(())
}

fn prompt_config_style() -> Result<ConfigStyle> {
    println!("How would you like to save the config file?");
    println!();
    println!("  [1] Minimal            only the settings you just entered");
    println!("  [2] Annotated template all options as commented-out examples");
    println!();
    print!("Choice [1-2]: ");
    io::stdout().flush()?;

    enable_raw_mode()?;
    loop {
        match crossterm::event::read() {
            Ok(Event::Key(KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                ..
            })) => {
                if c == 'c' && modifiers.contains(KeyModifiers::CONTROL) {
                    disable_raw_mode()?;
                    println!();
                    return Err(anyhow::anyhow!("Cancelled"));
                }
                match c {
                    '1' => {
                        disable_raw_mode()?;
                        println!("1");
                        return Ok(ConfigStyle::Minimal);
                    }
                    '2' => {
                        disable_raw_mode()?;
                        println!("2");
                        return Ok(ConfigStyle::Template);
                    }
                    _ => {}
                }
            }
            Ok(_) => {}
            Err(e) => {
                disable_raw_mode()?;
                println!();
                return Err(e.into());
            }
        }
    }
}

fn template_config(
    base_url: &str,
    default_project: &str,
    jira_config: &crate::config::types::JiraConfig,
) -> String {
    let cred_line = match &jira_config.credential_command {
        Some(cmd) => format!("    credential_command: \"{cmd}\",\n"),
        None if jira_config.credential_store.as_deref() == Some("keyring") => {
            "    credential_store: \"keyring\",\n".to_string()
        }
        None => String::new(),
    };

    let cred_comments = if jira_config.credential_command.is_some() {
        "    // credential_store: \"keyring\",\n    // credential_key: \"jira.example.com\",  // optional label\n".to_string()
    } else if jira_config.credential_store.as_deref() == Some("keyring") {
        "    // credential_key: \"jira.example.com\",  // optional label\n    // credential_command: \"pass show jira/do-next\",\n".to_string()
    } else {
        "    // credential_store: \"keyring\",\n    // credential_command: \"pass show jira/do-next\",\n    // Env: DO_NEXT_JIRA_TOKEN=<your-token>\n".to_string()
    };

    format!(
        r#"{{
  jira: {{
    base_url: "{base_url}",
    default_project: "{default_project}",

    // Authentication (first found wins):
    //   1. Env:              DO_NEXT_JIRA_TOKEN=<token>
    //   2. External command: credential_command: "..."
    //   3. System keyring:   credential_store: "keyring"
    //   4. Credentials file: ~/.config/do-next/credentials.json5
{cred_line}{cred_comments}  }},

  // Sources in priority order (first = highest priority).
  // Each source is self-contained: JQL, display, badges, subsources.
  sources: [
    // {{
    //   id: "incidents_in_progress",
    //   display_name: "Incidents in progress",
    //   jql: "assignee = currentUser() AND type = Incident AND status = \"In Progress\"",
    //   expected_project: "{default_project}",
    //   view_mode: "incident",
    //   indication: {{ symbol: "!", color: "red" }},
    // }},
    // {{
    //   id: "asap_tasks",
    //   display_name: "ASAP tasks",
    //   jql: "project = {default_project} AND priority = Highest AND status = \"To Do\"",
    //   indication: {{ symbol: "★", color: "yellow" }},
    //   subsources: [
    //     {{ jql_filter: "assignee = currentUser()" }},
    //     {{ jql_filter: "assignee is EMPTY", badge: "unassigned" }},
    //   ],
    // }},
    // {{
    //   id: "postmortem",
    //   display_name: "Filling up the postmortem",
    //   jql: "assignee = currentUser() AND type = Incident AND status = \"Mitigated\"",
    //   expected_project: "{default_project}",
    //   view_mode: "postmortem",
    //   indication: {{ symbol: "📋", color: "blue" }},
    // }},
    // {{
    //   id: "tasks_i_review",
    //   display_name: "Tasks I'm reviewing",
    //   jql: "filter = 12345",
    //   view_mode: "review",
    //   indication: {{ symbol: "👀", color: "cyan" }},
    //   badges: ["assignee"],
    // }},
    // {{
    //   id: "my_stale_in_review",
    //   display_name: "My tasks stale in review",
    //   jql: "assignee = currentUser() AND status = \"Ready for review\" ORDER BY updated ASC",
    //   indication: {{ symbol: "⏱", color: "magenta" }},
    //   allow_hide_for_a_day: true,
    //   badges: ["stale"],
    // }},
    // {{
    //   id: "my_active_tasks",
    //   display_name: "My active tasks",
    //   jql: "assignee = currentUser() AND status = \"In Progress\"",
    //   indication: {{ symbol: "▶", color: "green" }},
    // }},
    // {{
    //   id: "teammate_tasks_to_review",
    //   display_name: "Teammate's tasks I can review",
    //   jql: "project = {default_project} AND status = \"Ready for review\"",
    //   view_mode: "review",
    //   indication: {{ symbol: "✓", color: "cyan" }},
    //   subsources: [
    //     {{ jql_filter: "reviewer = currentUser()", badge: "reviewing" }},
    //     {{ jql_filter: "reviewer is EMPTY", badge: "unassigned" }},
    //   ],
    // }},
    // {{
    //   id: "regular_by_priority",
    //   display_name: "Regular tasks by priority",
    //   jql: "project = {default_project}",
    //   indication: {{ symbol: "·", color: "default" }},
    //   subsources: [
    //     {{ jql_filter: "assignee = currentUser()" }},
    //     {{ jql_filter: "assignee is EMPTY", badge: "unassigned" }},
    //   ],
    // }},
  ],

  list: {{
    // default_indication: {{ symbol: "•", color: "default" }},
  }},

  // hide_for_a_day: {{
  //   duration_hours: 24,
  //   suggested_solutions: [
  //     {{ label: "Ping reviewer (e.g. in Slack)" }},
  //     {{ label: "Set up a call with reviewer" }},
  //   ],
  // }},

  // view_modes: {{
  //   incident: {{ slack_thread_field: "customfield_12345" }},
  //   postmortem: {{
  //     sections: ["Summary", "Timeline", "Root cause", "Impact", "Action items"],
  //     body_field: "description",
  //   }},
  //   review: {{
  //     provider: "gitlab",
  //     link_method: "branch_name",
  //     base_url: "https://gitlab.example.com",
  //     mr_field: "customfield_67890",
  //   }},
  // }},

  // cache: {{
  //   enabled: true,
  //   max_age_seconds: 300,
  // }},
}}
"#
    )
}

fn print_pat_instructions(base_url: &str) {
    println!();
    println!("Personal Access Token");
    println!("  A PAT lets do-next read and act on Jira issues on your behalf.");
    println!("  To create one, open this URL in your browser:");
    println!(
        "    {}/secure/ViewProfile.jspa",
        base_url.trim_end_matches('/')
    );
    println!("  Then navigate to \"Personal Access Tokens\" and click \"Create token\".");
    println!();
    println!("  Input masked with *. Backspace on an empty field hides the input.");
    println!();
}

fn check_keyring_available(key: &str) -> Result<()> {
    let entry = keyring::Entry::new("do-next", key)
        .map_err(|e| anyhow::anyhow!("System keyring is not accessible: {e}"))?;
    match entry.get_password() {
        Ok(_) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(anyhow::anyhow!("System keyring is not accessible: {e}")),
    }
}

fn probe_credential_status(jira: &JiraConfig) -> CredentialStatus {
    let env_set = std::env::var("DO_NEXT_JIRA_TOKEN").is_ok()
        || std::env::var("DO_NEXT_JIRA_USERNAME").is_ok();

    let file_exists =
        dirs::config_dir().is_some_and(|d| d.join("do-next").join("credentials.json5").exists());

    let keyring_key = jira.credential_key.as_deref().unwrap_or(&jira.base_url);
    let keyring_found = keyring::Entry::new("do-next", keyring_key)
        .map(|e| e.get_password().is_ok())
        .unwrap_or(false);

    CredentialStatus {
        env_set,
        file_exists,
        keyring_found,
        command: jira.credential_command.clone(),
    }
}

fn detect_configured_method(jira: &JiraConfig) -> CredentialChoice {
    if jira.credential_command.is_some() {
        CredentialChoice::Command
    } else if jira.credential_store.as_deref() == Some("keyring") {
        CredentialChoice::Keyring
    } else {
        CredentialChoice::File
    }
}

const CRED_LABELS: [&str; 4] = [
    "System keyring  ",
    "External command",
    "Skip            ",
    "Credentials file",
];

const KEYRING_DESCRIPTION: &str = if cfg!(target_os = "macos") {
    "macOS Keychain"
} else if cfg!(target_os = "windows") {
    "Windows Credential Manager"
} else {
    "Linux Secret Service"
};

const CRED_DESCRIPTIONS: [&str; 4] = [
    KEYRING_DESCRIPTION,
    "fetch via shell command (pass, bitwarden CLI, …)",
    "set DO_NEXT_JIRA_TOKEN env manually",
    "~/.config/do-next/credentials.json5 (chmod 600)",
];

fn render_credential_options(
    selected: usize,
    current_idx: Option<usize>,
    tags: &[String; 4],
    confirmed: bool,
) -> Result<()> {
    for i in 0..4 {
        let marker = if current_idx == Some(i) {
            "  ← current"
        } else {
            ""
        };
        // Use \r\n so the cursor returns to column 0 in raw mode.
        if i == selected && confirmed {
            crossterm::execute!(
                io::stdout(),
                SetForegroundColor(Color::Green),
                Print("  ✓ "),
                ResetColor,
                Print(format!(
                    "{}   {}{}{}\r\n",
                    CRED_LABELS[i], CRED_DESCRIPTIONS[i], tags[i], marker
                )),
            )?;
        } else if i == selected {
            print!(
                "  > {}   {}{}{}\r\n",
                CRED_LABELS[i], CRED_DESCRIPTIONS[i], tags[i], marker
            );
        } else {
            print!(
                "    {}   {}{}{}\r\n",
                CRED_LABELS[i], CRED_DESCRIPTIONS[i], tags[i], marker
            );
        }
    }
    Ok(())
}

fn build_credential_tags(status: Option<&CredentialStatus>) -> [String; 4] {
    [
        // Keyring (0)
        status
            .map_or("", |s| {
                if s.keyring_found {
                    "  [entry found]"
                } else {
                    "  [no entry]"
                }
            })
            .to_string(),
        // Command (1)
        status
            .map(|s| {
                s.command.as_ref().map_or_else(
                    || "  [not set]".to_string(),
                    |cmd| {
                        let short = if cmd.len() > 25 {
                            format!("{}…", &cmd[..25])
                        } else {
                            cmd.clone()
                        };
                        format!("  [set: {short}]")
                    },
                )
            })
            .unwrap_or_default(),
        // Skip (2)
        status
            .map_or("", |s| if s.env_set { "  [set]" } else { "  [not set]" })
            .to_string(),
        // File (3)
        status
            .map_or("", |s| {
                if s.file_exists {
                    "  [token found]"
                } else {
                    "  [not found]"
                }
            })
            .to_string(),
    ]
}

fn run_credential_selection_loop(
    mut selected: usize,
    current_idx: Option<usize>,
    tags: &[String; 4],
) -> Result<CredentialChoice> {
    loop {
        match crossterm::event::read() {
            Ok(Event::Key(KeyEvent {
                code, modifiers, ..
            })) => {
                let nav = match code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        selected = selected.saturating_sub(1);
                        true
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        selected = (selected + 1).min(3);
                        true
                    }
                    _ => false,
                };

                if nav {
                    crossterm::execute!(io::stdout(), MoveUp(4), Clear(ClearType::FromCursorDown))?;
                    render_credential_options(selected, current_idx, tags, false)?;
                    io::stdout().flush()?;
                    continue;
                }

                match code {
                    KeyCode::Enter => {
                        crossterm::execute!(
                            io::stdout(),
                            MoveUp(4),
                            Clear(ClearType::FromCursorDown)
                        )?;
                        render_credential_options(selected, current_idx, tags, true)?;
                        io::stdout().flush()?;
                        disable_raw_mode()?;
                        println!();
                        return Ok(match selected {
                            0 => CredentialChoice::Keyring,
                            1 => CredentialChoice::Command,
                            2 => CredentialChoice::Skip,
                            _ => CredentialChoice::File,
                        });
                    }
                    KeyCode::Esc | KeyCode::Char('q') => {
                        disable_raw_mode()?;
                        println!();
                        return Err(anyhow::anyhow!("Cancelled"));
                    }
                    KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                        disable_raw_mode()?;
                        println!();
                        return Err(anyhow::anyhow!("Cancelled"));
                    }
                    _ => {}
                }
            }
            Ok(_) => {}
            Err(e) => {
                disable_raw_mode()?;
                println!();
                return Err(e.into());
            }
        }
    }
}

fn prompt_credential_choice(
    current: Option<&CredentialChoice>,
    status: Option<&CredentialStatus>,
) -> Result<CredentialChoice> {
    let tags = build_credential_tags(status);

    let current_idx = current.map(|c| match c {
        CredentialChoice::Keyring => 0,
        CredentialChoice::Command => 1,
        CredentialChoice::Skip => 2,
        CredentialChoice::File => 3,
    });

    let selected = current_idx.unwrap_or(0);

    println!("How would you like to store your Jira token?");
    println!();
    render_credential_options(selected, current_idx, &tags, false)?;
    io::stdout().flush()?;

    enable_raw_mode()?;
    run_credential_selection_loop(selected, current_idx, &tags)
}

/// Prompt for a yes/no answer. `default` sets which is chosen on bare Enter.
fn prompt_yes_no(message: &str, default: bool) -> Result<bool> {
    print!("{message}");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(match input.trim().to_lowercase().as_str() {
        "y" | "yes" => true,
        "n" | "no" => false,
        _ => default,
    })
}

fn prompt(message: &str, default: Option<&str>) -> Result<String> {
    print!("{message}");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let trimmed = input.trim().to_string();
    if trimmed.is_empty()
        && let Some(d) = default
    {
        return Ok(d.to_string());
    }
    Ok(trimmed)
}

fn prompt_masked(message: &str) -> Result<String> {
    print!("{message}");
    io::stdout().flush()?;

    enable_raw_mode()?;
    let mut token = String::new();
    let mut echo = true;
    loop {
        match crossterm::event::read() {
            Ok(Event::Key(KeyEvent {
                code: KeyCode::Enter,
                ..
            })) => break,
            Ok(Event::Key(KeyEvent {
                code: KeyCode::Char('c'),
                modifiers,
                ..
            })) if modifiers.contains(KeyModifiers::CONTROL) => {
                disable_raw_mode()?;
                println!();
                return Err(anyhow::anyhow!("Cancelled"));
            }
            Ok(Event::Key(KeyEvent {
                code: KeyCode::Char(c),
                ..
            })) => {
                token.push(c);
                if echo {
                    print!("*");
                    io::stdout().flush()?;
                }
            }
            Ok(Event::Key(KeyEvent {
                code: KeyCode::Backspace,
                ..
            })) => {
                if token.pop().is_some() {
                    if echo {
                        print!("\x08 \x08");
                        io::stdout().flush()?;
                    }
                } else if echo {
                    echo = false;
                    print!("[no echo]");
                    io::stdout().flush()?;
                }
            }
            Ok(_) => {}
            Err(e) => {
                disable_raw_mode()?;
                println!();
                return Err(e.into());
            }
        }
    }
    disable_raw_mode()?;
    println!();
    Ok(token)
}
