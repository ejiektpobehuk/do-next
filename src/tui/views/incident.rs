use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};

use crate::jira::types::Issue;
use crate::tui::app::{AppState, source_config_for};
use crate::tui::views::default::json_to_text;

pub fn render_incident(f: &mut Frame, area: Rect, issue: &Issue, app: &AppState) -> usize {
    let mut lines: Vec<Line> = Vec::new();

    // Key + status (incident emphasises status)
    lines.push(Line::from(vec![
        Span::styled(
            issue.key.clone(),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            issue.fields.status.name.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ]));

    // Wrong project warning — derived from the source config's expected_project
    let source_cfg = issue
        .source_id
        .as_deref()
        .and_then(|id| source_config_for(&app.config, id));
    let expected_project = source_cfg.and_then(|s| s.expected_project.as_deref());
    let wrong_project = expected_project.is_some_and(|ep| issue.fields.project.key != ep);
    if wrong_project {
        lines.push(Line::from(Span::styled(
            format!(
                "⚠ Wrong project: {} (expected {})",
                issue.fields.project.key,
                expected_project.unwrap_or("?")
            ),
            Style::default().fg(Color::Yellow),
        )));
    } else {
        lines.push(Line::from(""));
    }

    // Slack thread link (from custom field)
    let slack_field = app
        .config
        .view_modes
        .incident
        .as_ref()
        .and_then(|c| c.slack_thread_field.as_deref());
    if let Some(field_id) = slack_field {
        let link = issue
            .fields
            .extra
            .get(field_id)
            .and_then(|v| v.as_str())
            .unwrap_or("—");
        lines.push(Line::from(vec![
            Span::styled("Slack  ", Style::default().add_modifier(Modifier::DIM)),
            Span::raw(link.to_string()),
        ]));
    } else {
        lines.push(Line::from(""));
    }

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
