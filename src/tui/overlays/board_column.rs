use ratatui::{
    Frame,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState},
};

use crate::tui::app::ActionState;
use crate::tui::render::centered_rect;

/// Board-mode move-card picker: the board's columns in order, each showing
/// the transition it maps to. Unreachable columns render DIM (the workflow
/// offers no path into them); the current column carries a `●` marker.
pub fn render_board_column_overlay(f: &mut Frame, app_action: &ActionState) {
    let ActionState::SelectingBoardColumn {
        issue_key,
        transitions,
        columns,
        selected,
    } = app_action
    else {
        return;
    };

    let area = centered_rect(50, 60, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Move {issue_key} "))
        .title_bottom(
            Line::from(" t raw transitions ")
                .right_aligned()
                .style(Style::default().add_modifier(Modifier::DIM)),
        );

    let inner = block.inner(area);
    f.render_widget(block, area);

    let items: Vec<ListItem> = columns
        .iter()
        .map(|col| {
            let marker = if col.is_current { "● " } else { "  " };
            match &col.transition_id {
                Some(id) if !col.is_current => {
                    let via = transitions
                        .iter()
                        .find(|t| t.id == *id)
                        .map(|t| format!("  → {}", t.name))
                        .unwrap_or_default();
                    ListItem::new(Line::from(vec![
                        Span::raw(format!("{marker}{}", col.name)),
                        Span::styled(via, Style::default().add_modifier(Modifier::DIM)),
                    ]))
                }
                _ => ListItem::new(Line::from(Span::styled(
                    format!("{marker}{}", col.name),
                    Style::default().add_modifier(Modifier::DIM),
                ))),
            }
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(*selected));

    let list = List::new(items)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶ ");

    f.render_stateful_widget(list, inner, &mut state);
}
