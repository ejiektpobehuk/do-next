use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};

use crate::config::types::TeamConfig;
use crate::tui::app::ActionState;

pub fn render_hide_overlay(f: &mut Frame, action: &ActionState, config: &TeamConfig) {
    let ActionState::HidePopup {
        issue_key,
        selected_solution,
    } = action
    else {
        return;
    };

    let area = centered_rect(60, 70, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Hide {issue_key} for a day "));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(inner);

    // Header message
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Before hiding, consider one of these:",
            Style::default().fg(Color::Yellow),
        ))),
        chunks[0],
    );

    // Solutions list
    let solutions = &config.hide_for_a_day.suggested_solutions;
    let items: Vec<ListItem> = solutions
        .iter()
        .map(|s| {
            let mut spans = vec![Span::raw(&s.label)];
            if s.link.is_some() {
                spans.push(Span::styled(
                    "  [link]",
                    Style::default().add_modifier(Modifier::DIM),
                ));
            }
            if s.copy_template.is_some() {
                spans.push(Span::styled(
                    "  [copy]",
                    Style::default().add_modifier(Modifier::DIM),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(*selected_solution));

    let list = List::new(items)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶ ");

    f.render_stateful_widget(list, chunks[1], &mut state);
}

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
