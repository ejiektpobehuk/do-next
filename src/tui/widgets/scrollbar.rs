use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState},
};

pub fn render_scrollbar(
    f: &mut Frame,
    area: Rect,
    content_length: usize,
    viewport_length: usize,
    position: usize,
    thumb_color: Color,
) {
    let mut state = ScrollbarState::new(content_length)
        .viewport_content_length(viewport_length)
        .position(position);
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(Some("┐"))
        .end_symbol(Some("┘"))
        .track_symbol(Some("│"))
        .track_style(Style::default())
        .thumb_style(Style::default().fg(thumb_color));
    f.render_stateful_widget(scrollbar, area, &mut state);
}
