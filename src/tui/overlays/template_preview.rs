use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Clear, List, ListItem, ListState, Paragraph, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Wrap,
    },
};

use crate::tui::app::{ActionState, LoadedTemplate};
use crate::tui::markdown::markdown_to_lines;
use crate::tui::render::RenderOut;

pub fn render_template_preview_overlay(
    f: &mut Frame,
    app_action: &ActionState,
    render_out: &mut RenderOut,
) {
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
        render_preview(f, &templates[*cursor].content, *scroll, render_out);
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
        Span::raw(" ("),
        Span::styled("a", Style::default().fg(Color::Green)),
        Span::raw(")ccept | "),
        Span::styled("n", Style::default().fg(Color::Yellow)),
        Span::raw(" ("),
        Span::styled("d", Style::default().fg(Color::Yellow)),
        Span::raw(")ecline | ("),
        Span::styled("p", Style::default().fg(Color::Blue)),
        Span::raw(")review | "),
    ];
    if has_multiple {
        hint_spans.push(Span::styled("↑↓", Style::default().fg(Color::Cyan)));
        hint_spans.push(Span::raw(" select | "));
    }
    hint_spans.push(Span::styled("q", Style::default().fg(Color::Magenta)));
    hint_spans.push(Span::raw(" ("));
    hint_spans.push(Span::styled("c", Style::default().fg(Color::Magenta)));
    hint_spans.push(Span::raw(")ancel ├──"));

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

fn render_preview(f: &mut Frame, template_content: &str, scroll: u16, render_out: &mut RenderOut) {
    let area = centered_rect(70, 75, f.area());
    f.render_widget(Clear, area);

    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    let padded = Rect {
        x: inner.x + 2,
        width: inner.width.saturating_sub(2),
        ..inner
    };
    let viewport_h = padded.height as usize;

    let lines = markdown_to_lines(template_content);
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    let content_h = paragraph.line_count(padded.width);
    let max_scroll = u16::try_from(content_h.saturating_sub(viewport_h)).unwrap_or(u16::MAX);
    let display_scroll = scroll.min(max_scroll);
    let scrollable = content_h > viewport_h;

    render_out.confirm_content_h = content_h;
    render_out.confirm_viewport_h = viewport_h;

    let scroll_color = if scrollable {
        Color::Blue
    } else {
        Color::DarkGray
    };
    let hint = Line::from(vec![
        Span::raw("┤ "),
        Span::styled("y", Style::default().fg(Color::Green)),
        Span::raw(" ("),
        Span::styled("a", Style::default().fg(Color::Green)),
        Span::raw(")ccept | "),
        Span::styled("n", Style::default().fg(Color::Yellow)),
        Span::raw(" ("),
        Span::styled("d", Style::default().fg(Color::Yellow)),
        Span::raw(")ecline | "),
        Span::styled("↑↓", Style::default().fg(scroll_color)),
        Span::raw(" | "),
        Span::styled("q", Style::default().fg(Color::Magenta)),
        Span::raw(" back ├──"),
    ])
    .alignment(Alignment::Right);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Template preview ")
        .title_bottom(hint);
    f.render_widget(block, area);

    f.render_widget(paragraph.scroll((display_scroll, 0)), padded);

    if scrollable {
        let mut state = ScrollbarState::new(content_h - viewport_h + 1)
            .viewport_content_length(viewport_h)
            .position(display_scroll as usize);
        let bar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("┐"))
            .end_symbol(Some("┘"))
            .track_symbol(Some("│"))
            .track_style(Style::default())
            .thumb_style(Style::default().fg(Color::Yellow));
        f.render_stateful_widget(bar, area, &mut state);
    }
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
