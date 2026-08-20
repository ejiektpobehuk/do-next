//! `do-next inspect board <ID>` — a board's type, ordered columns and the
//! statuses each column maps to, plus `--as-sources` scaffolding.
//!
//! One `get_board_configuration` call answers what config authoring needs to
//! know about a board: whether `sprint:` and a backlog tab mean anything, and
//! the column→status grouping. It does not answer status *names* — the Agile
//! API returns bare ids there — so the status list is fetched alongside it and
//! joined in.

use std::collections::HashMap;

use anyhow::{Context, Result};
use serde_json::json;

use crate::jira::JiraClient;
use crate::jira::jql::escape_string;
use crate::jira::types::{BoardConfiguration, BoardType, StatusCategory};

/// One column, with its statuses named.
struct Column {
    name: String,
    statuses: Vec<Status>,
}

struct Status {
    id: String,
    /// `None` when the id is missing from the site-wide status list — a status
    /// the token cannot see, or one deleted out from under the board.
    name: Option<String>,
    category: Option<StatusCategory>,
}

impl Status {
    fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or("(unknown status id)")
    }
}

pub async fn run(
    client: &JiraClient,
    board_id: u64,
    as_sources: bool,
    project: Option<&str>,
    assignee: &str,
    json_out: bool,
) -> Result<()> {
    // Independent calls, and the join needs both — so pay for one round trip.
    let (config, statuses) = tokio::join!(
        client.get_board_configuration(board_id),
        client.get_all_statuses_detailed(),
    );
    let config = config.with_context(|| format!("Failed to fetch board {board_id}"))?;
    let statuses = statuses.context("Failed to fetch the status list")?;
    let by_id: HashMap<&str, _> = statuses.iter().map(|s| (s.id.as_str(), s)).collect();

    let columns: Vec<Column> = config
        .column_config
        .columns
        .iter()
        .map(|column| Column {
            name: column.name.clone(),
            statuses: column
                .statuses
                .iter()
                .map(|cs| {
                    let known = by_id.get(cs.id.as_str());
                    Status {
                        id: cs.id.clone(),
                        name: known.map(|s| s.name.clone()),
                        category: known.map(|s| s.category),
                    }
                })
                .collect(),
        })
        .collect();

    if as_sources {
        return emit_sources(
            &config.name,
            config.board_type.label(),
            &columns,
            project,
            assignee,
            json_out,
        );
    }

    if json_out {
        return print_json(&config, &columns);
    }
    print_human(&config, &columns);
    Ok(())
}

/// The board as JSON: the same facts, minus the alignment.
fn print_json(config: &BoardConfiguration, columns: &[Column]) -> Result<()> {
    let value = json!({
        "id": config.id,
        "name": config.name,
        "type": config.board_type.label(),
        "rank_field": config
            .ranking
            .as_ref()
            .and_then(|r| r.rank_custom_field_id)
            .map(|id| format!("customfield_{id}")),
        "columns": columns.iter().map(|c| json!({
            "name": c.name,
            "statuses": c.statuses.iter().map(|s| json!({
                "id": s.id,
                "name": s.name,
                "category": s.category.map(StatusCategory::key),
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    });
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

/// The board as a person reads it: type first, then columns in board order
/// with their statuses named and categorised.
fn print_human(config: &BoardConfiguration, columns: &[Column]) {
    println!(
        "board {} — \"{}\" ({})",
        config.id,
        config.name,
        config.board_type.label()
    );
    if let Some(field) = config.ranking.as_ref().and_then(|r| r.rank_custom_field_id) {
        println!("rank field: customfield_{field}");
    }
    // Team-managed boards report "simple" whatever they support, so the sprint
    // question cannot be answered from the type alone.
    if config.board_type == BoardType::Kanban {
        println!("no sprints — `sprint:` and a backlog tab do not apply");
    }
    println!();
    println!("columns (board order):");

    let id_width = columns
        .iter()
        .flat_map(|c| &c.statuses)
        .map(|s| s.id.chars().count())
        .max()
        .unwrap_or(0);
    let name_width = columns
        .iter()
        .flat_map(|c| &c.statuses)
        .map(|s| s.display_name().chars().count())
        .max()
        .unwrap_or(0);

    for (i, column) in columns.iter().enumerate() {
        println!("  {}. {}", i + 1, column.name);
        if column.statuses.is_empty() {
            println!("       (no mapped statuses)");
        }
        for status in &column.statuses {
            println!(
                "       {:<id_width$}  {:<name_width$}  ({})",
                status.id,
                status.display_name(),
                status
                    .category
                    .map_or("unknown category", StatusCategory::key),
            );
        }
    }

    println!();
    println!(
        "Tip: `--as-sources` turns these columns into a `sources:` array to paste into a team config."
    );
}

/// Emit the columns as a `sources:` array — one source per column, board order
/// preserved because position is priority in a team config.
fn emit_sources(
    board_name: &str,
    board_type: &str,
    columns: &[Column],
    project: Option<&str>,
    assignee: &str,
    json_out: bool,
) -> Result<()> {
    let project = project.context(
        "no project to scope the sources to — pass `--project KEY` \
         (or set the team's `default_project`)",
    )?;

    let mut used: Vec<String> = Vec::new();
    let mut rendered: Vec<(String, &Column, String)> = Vec::new();
    for column in columns {
        if column.statuses.is_empty() {
            continue;
        }
        let id = unique_slug(&column.name, &mut used);
        let jql = column_jql(project, column, assignee);
        rendered.push((id, column, jql));
    }

    if json_out {
        let value = json!({
            "sources": rendered.iter().map(|(id, column, jql)| json!({
                "id": id,
                "display_name": column.name,
                "jql": jql,
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    println!(
        "// Board \"{board_name}\" ({board_type}) — one source per column, board order = priority."
    );
    println!("sources: [");
    for (id, column, jql) in &rendered {
        println!("  {{");
        println!("    id: \"{id}\",");
        println!(
            "    display_name: \"{}\",",
            column.name.replace('"', "\\\"")
        );
        println!("    jql: {},", json5_string(jql));
        println!("  }},");
    }
    println!("]");

    for column in columns.iter().filter(|c| c.statuses.is_empty()) {
        println!("// column \"{}\" maps to no status — skipped", column.name);
    }
    if columns
        .iter()
        .flat_map(|c| &c.statuses)
        .any(|s| s.name.is_none())
    {
        println!("// some statuses are missing from the site-wide status list and were left out");
    }
    Ok(())
}

fn column_jql(project: &str, column: &Column, assignee: &str) -> String {
    let names: Vec<String> = column
        .statuses
        .iter()
        .filter_map(|s| s.name.as_deref())
        .map(|name| format!("\"{}\"", escape_string(name)))
        .collect();

    let mut clauses = vec![format!("project = \"{}\"", escape_string(project))];
    match names.len() {
        0 => {}
        1 => clauses.push(format!("status = {}", names[0])),
        _ => clauses.push(format!("status in ({})", names.join(", "))),
    }
    // `any` is the opt-out: a column-shaped source that is not about one
    // person (a whole-team board view) wants no assignee clause at all.
    if !assignee.eq_ignore_ascii_case("any") {
        clauses.push(format!("assignee = {}", assignee_literal(assignee)));
    }
    format!("{} ORDER BY updated DESC", clauses.join(" AND "))
}

/// JQL functions go in bare (`currentUser()`); anything else is a value and
/// needs quoting — an account id would otherwise be read as a field name.
fn assignee_literal(assignee: &str) -> String {
    if assignee.ends_with(')') {
        assignee.to_string()
    } else {
        format!("\"{}\"", escape_string(assignee))
    }
}

/// A source id from a column name: lowercase, non-alphanumerics collapsed to
/// single dashes, deduped against ids already emitted.
fn unique_slug(name: &str, used: &mut Vec<String>) -> String {
    let mut slug = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let base = slug.trim_matches('-').to_string();
    let base = if base.is_empty() {
        "column".to_string()
    } else {
        base
    };

    let mut candidate = base.clone();
    let mut n = 2;
    while used.contains(&candidate) {
        candidate = format!("{base}-{n}");
        n += 1;
    }
    used.push(candidate.clone());
    candidate
}

/// A JQL string as a JSON5 literal. Single quotes keep the JQL's own double
/// quotes readable, which is the point of a paste-ready snippet; a value
/// containing an apostrophe falls back to a JSON-escaped double-quoted string,
/// which is always valid if less pretty.
fn json5_string(value: &str) -> String {
    if value.contains('\'') {
        serde_json::to_string(value).unwrap_or_else(|_| format!("\"{value}\""))
    } else {
        format!("'{value}'")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(name: &str, statuses: &[(&str, &str)]) -> Column {
        Column {
            name: name.to_string(),
            statuses: statuses
                .iter()
                .map(|(id, name)| Status {
                    id: (*id).to_string(),
                    name: Some((*name).to_string()),
                    category: Some(StatusCategory::New),
                })
                .collect(),
        }
    }

    #[test]
    fn single_status_column_uses_equality() {
        let column = column("In Progress", &[("3", "In Progress")]);
        assert_eq!(
            column_jql("CTO", &column, "currentUser()"),
            r#"project = "CTO" AND status = "In Progress" AND assignee = currentUser() ORDER BY updated DESC"#
        );
    }

    #[test]
    fn multi_status_column_uses_in() {
        let column = column("To Do", &[("1", "To Do"), ("2", "Selected")]);
        assert_eq!(
            column_jql("CTO", &column, "any"),
            r#"project = "CTO" AND status in ("To Do", "Selected") ORDER BY updated DESC"#
        );
    }

    #[test]
    fn account_id_assignee_is_quoted() {
        let column = column("Review", &[("4", "Review")]);
        assert!(column_jql("CTO", &column, "5b10a2").contains(r#"assignee = "5b10a2""#));
    }

    #[test]
    fn slugs_are_unique_and_readable() {
        let mut used = Vec::new();
        assert_eq!(
            unique_slug("In Progress / Review", &mut used),
            "in-progress-review"
        );
        assert_eq!(
            unique_slug("in progress review", &mut used),
            "in-progress-review-2"
        );
        assert_eq!(unique_slug("···", &mut used), "column");
    }

    #[test]
    fn apostrophe_falls_back_to_double_quotes() {
        assert_eq!(
            json5_string("status = \"Bob's\""),
            r#""status = \"Bob's\"""#
        );
        assert_eq!(json5_string("status = \"Done\""), "'status = \"Done\"'");
    }
}
