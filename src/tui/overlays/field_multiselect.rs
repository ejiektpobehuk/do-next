use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::tui::app::ActionState;

pub fn render_field_multiselect_overlay(f: &mut Frame, app_action: &ActionState) {
    let ActionState::SelectingFieldOptions {
        options,
        selected,
        cursor,
        label,
        description,
        ..
    } = app_action
    else {
        return;
    };

    let area = centered_rect(50, 70, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {label} ─ Space=toggle  Enter=confirm "));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let (hint_area, list_area) = description.as_ref().map_or((None, inner), |desc| {
        let hint_lines = u16::try_from(desc.chars().count())
            .unwrap_or(u16::MAX)
            .div_ceil(inner.width.saturating_sub(2).max(1))
            + 1;
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(hint_lines), Constraint::Fill(1)])
            .split(inner);
        let hint = Paragraph::new(Span::styled(
            desc.as_str(),
            Style::default().add_modifier(Modifier::DIM),
        ))
        .wrap(Wrap { trim: false });
        f.render_widget(hint, chunks[0]);
        (Some(chunks[0]), chunks[1])
    });
    let _ = hint_area;

    let items: Vec<ListItem> = options
        .iter()
        .enumerate()
        .map(|(i, o)| {
            let check = if selected.contains(&i) {
                "[✓] "
            } else {
                "[ ] "
            };
            ListItem::new(Line::from(vec![
                Span::raw(check),
                Span::raw(o.value.as_str()),
            ]))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(*cursor));

    let list = List::new(items)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("  ");

    f.render_stateful_widget(list, list_area, &mut state);
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
