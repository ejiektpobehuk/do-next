//! `do-next inspect …` — read-only lookups against Jira.
//!
//! Every one of these answers a question that comes up while writing a team
//! config, and whose only other answer is a guess: the exact spelling of a
//! status, whether a project key is real, which account id belongs to a
//! person. Nothing here mutates anything, and `--json` makes the answers
//! usable from a script.

mod board;

use anyhow::{Context, Result};
use serde_json::json;

use crate::jira::JiraClient;
use crate::jira::types::StatusCategory;

#[derive(clap::Subcommand)]
pub enum What {
    /// A board's type, ordered columns and each column's statuses
    Board {
        /// Numeric board id (the `rapidView=<id>` in the board URL)
        board_id: u64,
        /// Print the columns as a `sources:` array ready to paste into a team config
        #[arg(long)]
        as_sources: bool,
        /// Project key the generated sources are scoped to (default: the team's `default_project`)
        #[arg(long, value_name = "KEY")]
        project: Option<String>,
        /// Assignee clause for the generated sources; `any` omits it
        #[arg(long, value_name = "JQL", default_value = "currentUser()")]
        assignee: String,
    },
    /// Projects visible to you, with their keys
    Projects,
    /// Statuses with their category — site-wide, or the ones a project uses
    Statuses {
        /// Project key; omit for every status on the site
        project: Option<String>,
    },
    /// A board's sprints
    Sprints {
        /// Numeric board id (the `rapidView=<id>` in the board URL)
        board_id: u64,
        /// Sprint states to list, comma-separated: active, future, closed
        #[arg(long, default_value = "active,future")]
        state: String,
    },
    /// Users assignable in a project, with the account ids config files need
    Users {
        /// Name fragment to search for; omit to list everyone assignable
        query: Option<String>,
        /// Project to search in (default: the team's `default_project`)
        #[arg(long, value_name = "KEY")]
        project: Option<String>,
    },
    /// Labels in use on this Jira site
    Labels,
    /// Issue link types, with both directions
    LinkTypes,
    /// Fields on an issue, with their ids and values
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
    /// Fields a project's create form offers, per issue type
    ///
    /// The one field lookup that needs no existing issue — which is what makes
    /// it usable while a project is still being configured.
    CreateFields {
        /// Project key (default: the team's `default_project`)
        project: Option<String>,
        /// Only this issue type (name, case-insensitive)
        #[arg(long, value_name = "NAME")]
        issue_type: Option<String>,
    },
}

pub async fn run(
    client: &JiraClient,
    default_project: &str,
    json_out: bool,
    what: &What,
) -> Result<()> {
    match what {
        What::Board {
            board_id,
            as_sources,
            project,
            assignee,
        } => {
            board::run(
                client,
                *board_id,
                *as_sources,
                project.as_deref().or_else(|| non_empty(default_project)),
                assignee,
                json_out,
            )
            .await
        }
        What::Projects => projects(client, json_out).await,
        What::Statuses { project } => statuses(client, project.as_deref(), json_out).await,
        What::Sprints { board_id, state } => sprints(client, *board_id, state, json_out).await,
        What::Users { query, project } => {
            let project = resolve_project(project.as_deref(), default_project)?;
            users(client, project, query.as_deref().unwrap_or(""), json_out).await
        }
        What::Labels => labels(client, json_out).await,
        What::LinkTypes => link_types(client, json_out).await,
        What::Fields {
            issue_key,
            field,
            raw,
        } => {
            crate::subcommands::fields::run(client, issue_key, field.as_deref(), *raw, json_out)
                .await
        }
        What::CreateFields {
            project,
            issue_type,
        } => {
            let project = resolve_project(project.as_deref(), default_project)?;
            create_fields(client, project, issue_type.as_deref(), json_out).await
        }
    }
}

fn non_empty(value: &str) -> Option<&str> {
    (!value.is_empty()).then_some(value)
}

fn resolve_project<'a>(explicit: Option<&'a str>, default_project: &'a str) -> Result<&'a str> {
    explicit
        .or_else(|| non_empty(default_project))
        .context("no project given and the team has no `default_project` — pass one as an argument")
}

async fn projects(client: &JiraClient, json_out: bool) -> Result<()> {
    let projects = client
        .search_projects()
        .await
        .context("Failed to fetch projects")?;

    if json_out {
        let value = json!(
            projects
                .iter()
                .map(|p| json!({ "key": p.key, "name": p.name }))
                .collect::<Vec<_>>()
        );
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    let width = projects
        .iter()
        .map(|p| p.key.chars().count())
        .max()
        .unwrap_or(3);
    println!("{:<width$}  NAME", "KEY");
    for project in &projects {
        println!("{:<width$}  {}", project.key, project.name);
    }
    println!("\n{} project(s)", projects.len());
    Ok(())
}

async fn statuses(client: &JiraClient, project: Option<&str>, json_out: bool) -> Result<()> {
    // A project's statuses come back as bare names, so the site-wide list is
    // fetched either way — it is what carries the categories.
    let all = client
        .get_all_statuses_detailed()
        .await
        .context("Failed to fetch statuses")?;

    let rows: Vec<(Option<String>, String, StatusCategory)> = match project {
        None => all
            .iter()
            .map(|s| (Some(s.id.clone()), s.name.clone(), s.category))
            .collect(),
        Some(key) => {
            let names = client
                .get_project_statuses(key)
                .await
                .with_context(|| format!("Failed to fetch statuses for project {key}"))?;
            names
                .into_iter()
                .map(|name| {
                    let known = all.iter().find(|s| s.name == name);
                    (
                        known.map(|s| s.id.clone()),
                        name,
                        known.map_or(StatusCategory::Undefined, |s| s.category),
                    )
                })
                .collect()
        }
    };

    if json_out {
        let value = json!(
            rows.iter()
                .map(|(id, name, category)| json!({
                    "id": id,
                    "name": name,
                    "category": category.key(),
                }))
                .collect::<Vec<_>>()
        );
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    let id_width = rows
        .iter()
        .map(|(id, ..)| id.as_deref().unwrap_or("—").chars().count())
        .max()
        .unwrap_or(2);
    let name_width = rows
        .iter()
        .map(|(_, name, _)| name.chars().count())
        .max()
        .unwrap_or(4);
    println!("{:<id_width$}  {:<name_width$}  CATEGORY", "ID", "NAME");
    for (id, name, category) in &rows {
        println!(
            "{:<id_width$}  {:<name_width$}  {}",
            id.as_deref().unwrap_or("—"),
            name,
            category.key()
        );
    }
    println!("\n{} status(es)", rows.len());
    println!("Categories are what `statusCategory != Done` compares against.");
    Ok(())
}

async fn sprints(client: &JiraClient, board_id: u64, state: &str, json_out: bool) -> Result<()> {
    let sprints = client
        .get_board_sprints(board_id, state)
        .await
        .with_context(|| format!("Failed to fetch sprints for board {board_id}"))?;

    let Some(sprints) = sprints else {
        if json_out {
            println!("{}", serde_json::to_string_pretty(&json!(null))?);
        } else {
            println!(
                "board {board_id} has no sprints (kanban, or a sprint-less team-managed board)"
            );
        }
        return Ok(());
    };

    if json_out {
        let value = json!(
            sprints
                .iter()
                .map(|s| json!({ "id": s.id, "name": s.name, "state": s.state }))
                .collect::<Vec<_>>()
        );
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    println!("{:<8}  {:<8}  NAME", "ID", "STATE");
    for sprint in &sprints {
        println!("{:<8}  {:<8}  {}", sprint.id, sprint.state, sprint.name);
    }
    println!("\n{} sprint(s) in state(s) {state}", sprints.len());
    println!("A numeric id goes straight into a board source's `sprint:`.");
    Ok(())
}

async fn users(client: &JiraClient, project: &str, query: &str, json_out: bool) -> Result<()> {
    let users = client
        .search_assignable_users(project, query)
        .await
        .with_context(|| format!("Failed to search users assignable in {project}"))?;

    if json_out {
        let value = json!(
            users
                .iter()
                .map(|u| json!({
                    "account_id": u.account_id,
                    "display_name": u.display_name,
                    "name": u.name,
                }))
                .collect::<Vec<_>>()
        );
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    let id_width = users
        .iter()
        .map(|u| u.account_id.as_deref().unwrap_or("—").chars().count())
        .max()
        .unwrap_or(10);
    println!("{:<id_width$}  NAME", "ACCOUNT ID");
    for user in &users {
        println!(
            "{:<id_width$}  {}",
            user.account_id.as_deref().unwrap_or("—"),
            user.display()
        );
    }
    println!("\n{} user(s) assignable in {project}", users.len());
    Ok(())
}

async fn labels(client: &JiraClient, json_out: bool) -> Result<()> {
    let labels = client
        .all_labels()
        .await
        .context("Failed to fetch labels")?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&json!(labels))?);
        return Ok(());
    }
    for label in &labels {
        println!("{label}");
    }
    println!("\n{} label(s)", labels.len());
    Ok(())
}

async fn link_types(client: &JiraClient, json_out: bool) -> Result<()> {
    let types = client
        .issue_link_types()
        .await
        .context("Failed to fetch issue link types")?;

    if json_out {
        let value = json!(
            types
                .iter()
                .map(|t| json!({ "name": t.name, "inward": t.inward, "outward": t.outward }))
                .collect::<Vec<_>>()
        );
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    let width = types
        .iter()
        .map(|t| t.name.chars().count())
        .max()
        .unwrap_or(4);
    println!("{:<width$}  INWARD / OUTWARD", "NAME");
    for link in &types {
        println!("{:<width$}  {} / {}", link.name, link.inward, link.outward);
    }
    println!("\n{} link type(s)", types.len());
    Ok(())
}

async fn create_fields(
    client: &JiraClient,
    project: &str,
    issue_type: Option<&str>,
    json_out: bool,
) -> Result<()> {
    let types = client
        .get_create_issuetypes(project)
        .await
        .with_context(|| format!("Failed to fetch issue types creatable in {project}"))?;

    let selected: Vec<_> = types
        .iter()
        .filter(|t| issue_type.is_none_or(|want| t.name.eq_ignore_ascii_case(want)))
        .collect();

    if selected.is_empty() {
        let available: Vec<&str> = types.iter().map(|t| t.name.as_str()).collect();
        anyhow::bail!(
            "no issue type matches in {project}; available: {}",
            available.join(", ")
        );
    }

    let mut per_type = Vec::new();
    for issue_type in &selected {
        let fields = client
            .get_create_fields(project, &issue_type.id)
            .await
            .with_context(|| {
                format!(
                    "Failed to fetch create fields for {project}/{}",
                    issue_type.name
                )
            })?;
        per_type.push((*issue_type, fields));
    }

    if json_out {
        let value = json!(
            per_type
                .iter()
                .map(|(issue_type, fields)| json!({
                    "issue_type": issue_type.name,
                    "issue_type_id": issue_type.id,
                    "fields": fields.iter().map(|f| json!({
                        "id": field_str(f, "fieldId"),
                        "name": field_str(f, "name"),
                        "required": f.get("required").and_then(serde_json::Value::as_bool).unwrap_or(false),
                        "schema": field_schema(f),
                    })).collect::<Vec<_>>(),
                }))
                .collect::<Vec<_>>()
        );
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    for (issue_type, fields) in &per_type {
        println!("{} (id {})", issue_type.name, issue_type.id);
        let id_width = fields
            .iter()
            .map(|f| field_str(f, "fieldId").chars().count())
            .max()
            .unwrap_or(8);
        let name_width = fields
            .iter()
            .map(|f| field_str(f, "name").chars().count())
            .max()
            .unwrap_or(4);
        for field in fields {
            let required = field
                .get("required")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            println!(
                "  {:<id_width$}  {:<name_width$}  {:<9} {}",
                field_str(field, "fieldId"),
                field_str(field, "name"),
                if required { "required" } else { "" },
                field_schema(field),
            );
        }
        println!();
    }
    Ok(())
}

fn field_str<'a>(field: &'a serde_json::Value, key: &str) -> &'a str {
    field
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("—")
}

/// The field's type as the create form sees it: the custom type when there is
/// one (that is what distinguishes two `string` fields), else the base type.
fn field_schema(field: &serde_json::Value) -> String {
    let schema = field.get("schema");
    let base = schema
        .and_then(|s| s.get("type"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("—");
    let custom = schema
        .and_then(|s| s.get("custom"))
        .and_then(serde_json::Value::as_str)
        .and_then(|c| c.rsplit(':').next());
    custom.map_or_else(|| base.to_string(), |custom| format!("{base} ({custom})"))
}
