use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use crate::tui::app::ActionState;

pub fn render_comment_delete_confirm_overlay(f: &mut Frame, app_action: &ActionState) {
    let ActionState::ConfirmingCommentDelete { selected, .. } = app_action else {
        return;
    };

    let area = crate::tui::render::centered_rect(40, 20, f.area());
    f.render_widget(Clear, area);

    let hint = Line::from(vec![
        Span::raw("┤ "),
        Span::styled("←→", Style::default().fg(Color::Blue)),
        Span::raw(" select  "),
        Span::styled("↵", Style::default().fg(Color::Green)),
        Span::raw(" confirm ├──"),
    ])
    .alignment(Alignment::Right);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red))
        .title(" Delete comment? ")
        .title_bottom(hint);

    let inner = block.inner(area);
    f.render_widget(block, area);

    let yes_style = if *selected == 0 {
        Style::default()
            .fg(Color::Red)
            .add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let no_style = if *selected == 1 {
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let buttons = Line::from(vec![
        Span::styled("  Yes  ", yes_style),
        Span::raw("   "),
        Span::styled("  No  ", no_style),
    ]);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(inner);

    f.render_widget(Paragraph::new("This action cannot be undone."), layout[0]);
    f.render_widget(Paragraph::new(buttons), layout[1]);
}
