use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};

use crate::jira::types::Issue;
use crate::tui::app::AppState;

pub fn render_default(f: &mut Frame, area: Rect, issue: &Issue, app: &AppState) -> usize {
    let mut lines: Vec<Line> = Vec::new();

    // Key + summary
    lines.push(Line::from(vec![
        Span::styled(&issue.key, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::raw(&issue.fields.summary),
    ]));

    // Status
    lines.push(field_line("Status", &issue.fields.status.name));

    // Priority
    let priority = issue
        .fields
        .priority
        .as_ref()
        .map_or_else(|| "—".into(), |p| format!("{} {}", p.symbol(), p.name));
    lines.push(field_line("Priority", &priority));

    // Assignee
    let assignee = issue
        .fields
        .assignee
        .as_ref()
        .map_or_else(|| "Unassigned".into(), |a| a.display().to_string());
    lines.push(field_line("Assignee", &assignee));

    // Project
    lines.push(field_line(
        "Project",
        &format!(
            "{} ({})",
            issue.fields.project.name, issue.fields.project.key
        ),
    ));

    // Browser link
    let url = format!("{}/browse/{}", app.config.jira.base_url, issue.key);
    lines.push(field_line("Link", &url));

    lines.push(Line::from(""));

    // Description
    if let Some(ref desc) = issue.fields.description {
        let text = json_to_text(desc);
        if !text.is_empty() {
            for l in text.replace('\r', "").lines() {
                lines.push(Line::from(l.to_string()));
            }
        }
    }

    let total = lines.len();
    let scroll = u16::try_from(app.detail_scroll).unwrap_or(u16::MAX);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        area,
    );
    total
}

fn field_line(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label:<12}"),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Span::raw(value.to_string()),
    ])
}

/// Best-effort plain text extraction from Jira description (string or ADF JSON).
pub fn json_to_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Object(_) => {
            // Jira Server usually returns plain strings; ADF is Cloud.
            // Walk the content array if present.
            extract_adf_text(value)
        }
        _ => String::new(),
    }
}

fn extract_adf_text(node: &serde_json::Value) -> String {
    let mut out = String::new();
    if let Some(text) = node.get("text").and_then(|t| t.as_str()) {
        out.push_str(text);
    }
    if let Some(content) = node.get("content").and_then(|c| c.as_array()) {
        for child in content {
            out.push_str(&extract_adf_text(child));
        }
        out.push('\n');
    }
    out
}
