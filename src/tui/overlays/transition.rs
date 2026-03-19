use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState},
};

use crate::tui::app::ActionState;

pub fn render_transition_overlay(f: &mut Frame, app_action: &ActionState) {
    let ActionState::SelectingTransition {
        transitions,
        selected,
        issue_key,
    } = app_action
    else {
        return;
    };

    let area = centered_rect(50, 60, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Transition {issue_key} "));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let items: Vec<ListItem> = transitions
        .iter()
        .map(|t| {
            ListItem::new(Line::from(vec![
                Span::raw(&t.name),
                Span::styled(
                    format!("  → {}", t.to.name),
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

/// Returns a centered rect of given percentage within `r`.
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
