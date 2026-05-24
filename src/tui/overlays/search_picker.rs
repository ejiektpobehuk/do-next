use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};

use crate::tui::app::{
    ActionState, AppState, FilterChoice, FilterKind, FilterPicker, PickerSection,
    picker_visible_indices,
};
use crate::tui::theme;

pub fn render_search_picker_overlay(f: &mut Frame, app: &AppState) {
    let ActionState::Searching {
        picker: Some(ref picker),
        ..
    } = app.action_state
    else {
        return;
    };

    let area = centered_rect(50, 70, f.area());
    f.render_widget(Clear, area);

    let title = match picker.kind {
        FilterKind::Status => " Status ",
        FilterKind::Project => " Project ",
    };

    let space_hint = if picker.kind == FilterKind::Status {
        " cycle  "
    } else {
        " toggle  "
    };
    let close_hint = Line::from(vec![
        Span::raw("┤ "),
        Span::styled("Space", Style::default().fg(Color::Blue)),
        Span::raw(space_hint),
        Span::styled("Enter", Style::default().fg(Color::Green)),
        Span::raw(" apply  "),
        Span::styled("Esc", Style::default().fg(Color::Magenta)),
        Span::raw(" cancel ├──"),
    ])
    .alignment(Alignment::Right)
    .style(Style::default().fg(theme::MUTED));

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            title,
            Style::default().add_modifier(Modifier::BOLD),
        ))
        .title_bottom(close_hint);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // typeahead
            Constraint::Min(1),    // list
            Constraint::Length(1), // footer (loading indicator)
        ])
        .split(inner);

    render_typeahead(f, chunks[0], picker);
    render_list(f, chunks[1], picker);
    render_footer(f, chunks[2], picker);
}

fn render_typeahead(f: &mut Frame, area: Rect, picker: &FilterPicker) {
    let spans = vec![
        Span::styled("/ ", Style::default().fg(Color::Blue)),
        Span::raw(picker.query.clone()),
    ];
    f.render_widget(Paragraph::new(Line::from(spans)), area);
    let prefix_cols = 2u16;
    #[allow(clippy::cast_possible_truncation)]
    let cursor_col = prefix_cols + picker.query_cursor as u16;
    let x = area
        .x
        .saturating_add(cursor_col.min(area.width.saturating_sub(1)));
    f.set_cursor_position((x, area.y));
}

fn render_list(f: &mut Frame, area: Rect, picker: &FilterPicker) {
    let visible = picker_visible_indices(picker);
    if visible.is_empty() {
        let msg = if picker.loading {
            "Loading…"
        } else if picker.items.is_empty() {
            "No items"
        } else {
            "No matches"
        };
        f.render_widget(
            Paragraph::new(Span::styled(
                msg,
                Style::default()
                    .fg(theme::MUTED)
                    .add_modifier(Modifier::ITALIC),
            )),
            area,
        );
        return;
    }

    // Walk visible items, emitting section headers as decorative non-cursor
    // rows interleaved with selectable rows. The header rows shift the cursor
    // index down, so we keep a mapping from `visible` index → `display row`.
    let mut display_rows: Vec<ListItem> = Vec::new();
    let mut cursor_row: Option<usize> = None;
    let mut last_section: Option<PickerSection> = None;

    for (vi, &item_idx) in visible.iter().enumerate() {
        let item = &picker.items[item_idx];
        if last_section != Some(item.section) {
            display_rows.push(section_header_row(item.section));
            last_section = Some(item.section);
        }
        if vi == picker.cursor {
            cursor_row = Some(display_rows.len());
        }
        let choice = picker.selected.get(&item.value).copied();
        let (check, value_style) = match choice {
            Some(FilterChoice::Include) => (
                "[+] ",
                Style::default()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
            ),
            Some(FilterChoice::Exclude) => (
                "[-] ",
                Style::default()
                    .fg(Color::Red)
                    .add_modifier(Modifier::BOLD),
            ),
            None => ("[ ] ", Style::default()),
        };
        display_rows.push(ListItem::new(Line::from(vec![
            Span::raw(check),
            Span::styled(item.label.clone(), value_style),
        ])));
    }

    let mut state = ListState::default();
    state.select(cursor_row);

    let list = List::new(display_rows)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("");

    f.render_stateful_widget(list, area, &mut state);
}

fn section_header_row(section: PickerSection) -> ListItem<'static> {
    let label = match section {
        PickerSection::Team => "── Team ──",
        PickerSection::Other => "── Other ──",
    };
    ListItem::new(Line::from(Span::styled(
        label,
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )))
}

fn render_footer(f: &mut Frame, area: Rect, picker: &FilterPicker) {
    if !picker.loading {
        return;
    }
    let span = Span::styled(
        "loading…",
        Style::default()
            .fg(Color::Blue)
            .add_modifier(Modifier::ITALIC),
    );
    f.render_widget(Paragraph::new(Line::from(span)), area);
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
