use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

pub fn render_keybindings_overlay(f: &mut Frame) {
    let area = crate::tui::render::centered_rect(70, 80, f.area());
    f.render_widget(Clear, area);

    let close_hint = Line::from(vec![
        Span::raw("┤ "),
        Span::styled("q", Style::default().fg(Color::Magenta)),
        Span::raw(" close ├──"),
    ])
    .alignment(Alignment::Right);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Keybindings ")
        .title_bottom(close_hint)
        .style(Style::default());
    let inner = block.inner(area);
    f.render_widget(block, area);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(inner);

    f.render_widget(Paragraph::new(left_column()), cols[0]);
    f.render_widget(Paragraph::new(right_column()), cols[1]);
}

fn section_header(title: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!("── {title} "),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    ))
}

fn key_line(key: &'static str, desc: &'static str) -> Line<'static> {
    key_line_colored(key, desc, Color::Blue)
}

fn key_line_red(key: &'static str, desc: &'static str) -> Line<'static> {
    key_line_colored(key, desc, Color::Red)
}

fn key_line_magenta(key: &'static str, desc: &'static str) -> Line<'static> {
    key_line_colored(key, desc, Color::Magenta)
}

fn key_line_colored(key: &'static str, desc: &'static str, color: Color) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(format!("{key:<14}"), Style::default().fg(color)),
        Span::raw(desc),
    ])
}

fn left_column() -> Vec<Line<'static>> {
    vec![
        section_header("Navigation"),
        key_line("↑ / k", "move up"),
        key_line("↓ / j", "move down"),
        key_line("← / h", "move left"),
        key_line("→ / l", "move right"),
        key_line("gg / G", "first / last"),
        key_line("PgUp / PgDn", "scroll"),
        Line::raw(""),
        section_header("View"),
        key_line("v", "cycle view modes"),
        key_line("Enter", "edit field / open comments / attachments"),
        Line::raw(""),
        section_header("General"),
        key_line("?", "this help"),
        key_line("q / Esc", "go back"),
        key_line_magenta("q / Esc", "close a popup"),
        key_line_red("q / Ctrl+C", "quit"),
    ]
}

fn right_column() -> Vec<Line<'static>> {
    vec![
        section_header("Actions"),
        key_line("o", "open in browser"),
        key_line("t", "transition"),
        key_line("c", "comment"),
        key_line("i", "hide for a day"),
        key_line("a", "assign to me"),
        key_line("m", "move to project"),
    ]
}
