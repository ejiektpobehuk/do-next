use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::tui::app::ActionState;
use crate::tui::markdown::markdown_to_lines;

pub fn render_template_preview_overlay(f: &mut Frame, app_action: &ActionState) {
    let ActionState::OfferingTemplate {
        template_content,
        previewing,
        scroll,
        ..
    } = app_action
    else {
        return;
    };

    if *previewing {
        render_preview(f, template_content, *scroll);
    } else {
        render_dialog(f);
    }
}

fn render_dialog(f: &mut Frame) {
    let area = centered_rect(50, 20, f.area());
    f.render_widget(Clear, area);

    let hint = Line::from(vec![
        Span::raw("┤ "),
        Span::styled("y", Style::default().fg(Color::Green)),
        Span::raw(" accept | "),
        Span::styled("n", Style::default().fg(Color::Yellow)),
        Span::raw(" decline | "),
        Span::styled("p", Style::default().fg(Color::Blue)),
        Span::raw(" preview | "),
        Span::styled("q", Style::default().fg(Color::Magenta)),
        Span::raw(" cancel ├──"),
    ])
    .alignment(Alignment::Right);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Template available ")
        .title_bottom(hint);

    let inner = block.inner(area);
    f.render_widget(block, area);

    let padded = Rect {
        x: inner.x + 2,
        width: inner.width.saturating_sub(2),
        ..inner
    };
    f.render_widget(
        Paragraph::new("Use a template to prepopulate the field?"),
        padded,
    );
}

fn render_preview(f: &mut Frame, template_content: &str, scroll: u16) {
    let area = centered_rect(70, 75, f.area());
    f.render_widget(Clear, area);

    let hint = Line::from(vec![
        Span::raw("┤ "),
        Span::styled("↵", Style::default().fg(Color::Green)),
        Span::raw(" accept | "),
        Span::styled("n", Style::default().fg(Color::Yellow)),
        Span::raw(" decline | "),
        Span::styled("↑↓", Style::default().fg(Color::Blue)),
        Span::raw(" scroll | "),
        Span::styled("q", Style::default().fg(Color::Magenta)),
        Span::raw(" back ├──"),
    ])
    .alignment(Alignment::Right);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Template preview ")
        .title_bottom(hint);

    let inner = block.inner(area);
    f.render_widget(block, area);

    let padded = Rect {
        x: inner.x + 2,
        width: inner.width.saturating_sub(2),
        ..inner
    };
    let lines = markdown_to_lines(template_content);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        padded,
    );
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
