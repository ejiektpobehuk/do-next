use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::tui::app::{ActionState, LoadedTemplate};
use crate::tui::markdown::markdown_to_lines;

pub fn render_template_preview_overlay(f: &mut Frame, app_action: &ActionState) {
    let ActionState::OfferingTemplate {
        templates,
        cursor,
        previewing,
        scroll,
        ..
    } = app_action
    else {
        return;
    };

    if *previewing {
        render_preview(f, &templates[*cursor].content, *scroll);
    } else {
        render_dialog(f, templates, *cursor);
    }
}

fn render_dialog(f: &mut Frame, templates: &[LoadedTemplate], cursor: usize) {
    let has_multiple = templates.len() > 1;
    let height_percent = if has_multiple { 30 } else { 20 };
    let area = centered_rect(50, height_percent, f.area());
    f.render_widget(Clear, area);

    let mut hint_spans = vec![
        Span::raw("┤ "),
        Span::styled("y", Style::default().fg(Color::Green)),
        Span::raw(" accept | "),
        Span::styled("n", Style::default().fg(Color::Yellow)),
        Span::raw(" decline | "),
        Span::styled("p", Style::default().fg(Color::Blue)),
        Span::raw(" preview | "),
    ];
    if has_multiple {
        hint_spans.push(Span::styled("↑↓", Style::default().fg(Color::Cyan)));
        hint_spans.push(Span::raw(" select | "));
    }
    hint_spans.push(Span::styled("q", Style::default().fg(Color::Magenta)));
    hint_spans.push(Span::raw(" cancel ├──"));

    let hint = Line::from(hint_spans).alignment(Alignment::Right);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Template available ")
        .title_bottom(hint);

    let inner = block.inner(area);
    f.render_widget(block, area);

    let padded = Rect {
        x: inner.x + 2,
        width: inner.width.saturating_sub(4),
        ..inner
    };

    if has_multiple {
        // Show prompt + selectable list
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Fill(1)])
            .split(padded);

        f.render_widget(
            Paragraph::new("Select a template to prepopulate the field:"),
            chunks[0],
        );

        let items: Vec<ListItem> = templates
            .iter()
            .map(|t| ListItem::new(Line::from(t.name.as_str())))
            .collect();

        let mut state = ListState::default();
        state.select(Some(cursor));

        let list = List::new(items)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("▶ ");

        f.render_stateful_widget(list, chunks[1], &mut state);
    } else {
        f.render_widget(
            Paragraph::new("Use a template to prepopulate the field?"),
            padded,
        );
    }
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
