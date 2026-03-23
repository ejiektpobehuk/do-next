use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};

use crate::jira::types::Issue;
use crate::tui::app::AppState;

pub fn render_attachments(f: &mut Frame, area: Rect, issue: &Issue, app: &AppState) -> usize {
    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(vec![
        Span::styled(&issue.key, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" — Attachments"),
    ]));
    lines.push(Line::from(""));

    match &issue.fields.attachment {
        Some(attachments) if !attachments.is_empty() => {
            for attachment in attachments {
                let author = attachment.author.display();
                let date = &attachment.created[..10]; // YYYY-MM-DD
                lines.push(Line::from(Span::styled(
                    format!("{} · {} · {date}", attachment.filename, author),
                    Style::default().add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(""));
            }
        }
        _ => {
            lines.push(Line::from(Span::styled(
                "(no attachments)",
                Style::default().add_modifier(Modifier::DIM),
            )));
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
