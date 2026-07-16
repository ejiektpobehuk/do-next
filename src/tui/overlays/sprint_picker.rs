use ratatui::{
    Frame,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState},
};

use crate::tui::app::ActionState;
use crate::tui::render::centered_rect;

/// Backlog send-to-sprint picker: the board's active and future sprints in
/// server order, the sprint state rendered as a DIM suffix.
pub fn render_sprint_picker_overlay(f: &mut Frame, app_action: &ActionState) {
    let ActionState::SelectingSprint {
        issue_key,
        sprints,
        selected,
    } = app_action
    else {
        return;
    };

    let area = centered_rect(50, 60, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Move {issue_key} to sprint "));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let items: Vec<ListItem> = sprints
        .iter()
        .map(|sprint| {
            let marker = if sprint.state == "active" {
                "● "
            } else {
                "  "
            };
            ListItem::new(Line::from(vec![
                Span::raw(format!("{marker}{}", sprint.name)),
                Span::styled(
                    format!("  {}", sprint.state),
                    Style::default().add_modifier(Modifier::DIM),
                ),
            ]))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(*selected));

    let list = List::new(items)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶ ");

    f.render_stateful_widget(list, inner, &mut state);
}
