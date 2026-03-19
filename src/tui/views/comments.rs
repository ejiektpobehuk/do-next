use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};

use crate::jira::types::Issue;
use crate::tui::app::AppState;

pub fn render_comments(f: &mut Frame, area: Rect, issue: &Issue, app: &AppState) -> usize {
    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(vec![
        Span::styled(&issue.key, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" — Comments"),
    ]));
    lines.push(Line::from(""));

    match &issue.fields.comment {
        Some(comment_list) if !comment_list.comments.is_empty() => {
            for comment in &comment_list.comments {
                let author = comment.author.display();
                let date = &comment.created[..10]; // YYYY-MM-DD
                lines.push(Line::from(Span::styled(
                    format!("{author} · {date}"),
                    Style::default().add_modifier(Modifier::BOLD),
                )));
                for body_line in comment.body.lines() {
                    lines.push(Line::from(format!("  {body_line}")));
                }
                lines.push(Line::from(""));
            }
        }
        _ => {
            lines.push(Line::from(Span::styled(
                "(no comments)",
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
