use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::jira::types::Issue;
use crate::tui::app::{ActionState, AppState, JiraSearchState, SearchFocus};
use crate::tui::search::{ChipSet, HitOrigin, RankedHit};
use crate::tui::theme;

pub fn render_search_overlay(f: &mut Frame, app: &AppState) {
    let ActionState::Searching {
        ref query,
        cursor,
        active_chips,
        focus,
        ref local_results,
        ref jira_state,
        selected,
        ..
    } = app.action_state
    else {
        return;
    };

    let area = centered_rect(90, 90, f.area());
    f.render_widget(Clear, area);

    let outer = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(" Search ", Style::default().add_modifier(Modifier::BOLD)));
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(inner);

    let left = columns[0];
    let right = columns[1];

    let left_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // input
            Constraint::Length(3), // chips
            Constraint::Min(1),    // results
            Constraint::Length(1), // footer
        ])
        .split(left);

    render_input(f, left_chunks[0], query, cursor, focus == SearchFocus::Input);
    render_chips(f, left_chunks[1], active_chips, focus);
    render_results(f, left_chunks[2], local_results, jira_state, selected, focus);
    render_footer(f, left_chunks[3], local_results, jira_state);

    render_preview(f, right, app, local_results, jira_state, selected);
}

fn render_input(f: &mut Frame, area: Rect, query: &str, cursor: usize, focused: bool) {
    let border_color = if focused { theme::BORDER_FOCUS } else { theme::MUTED };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(" Query ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let spans = vec![
        Span::styled("/ ", Style::default().fg(Color::Blue)),
        Span::raw(query),
    ];
    f.render_widget(Paragraph::new(Line::from(spans)), inner);

    if focused && inner.height > 0 {
        let prefix_cols = 2; // "/ "
        #[allow(clippy::cast_possible_truncation)]
        let cursor_col = prefix_cols + cursor as u16;
        let x = inner.x.saturating_add(cursor_col.min(inner.width.saturating_sub(1)));
        f.set_cursor_position((x, inner.y));
    }
}

const CHIP_LABELS: [&str; 5] = ["Mine", "Unassigned", "In Review", "Active Sprint", "Global"];

fn render_chips(f: &mut Frame, area: Rect, chips: ChipSet, focus: SearchFocus) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::MUTED))
        .title(" Filters ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut spans: Vec<Span> = Vec::new();
    for (idx, label) in CHIP_LABELS.iter().enumerate() {
        let active = chip_active(chips, idx);
        let focused = matches!(focus, SearchFocus::Chip(i) if i == idx);
        let mut style = Style::default();
        if active {
            style = style.fg(Color::Blue).add_modifier(Modifier::BOLD);
        } else {
            style = style.fg(theme::MUTED);
        }
        if focused {
            style = style.add_modifier(Modifier::REVERSED);
        }
        spans.push(Span::styled(format!(" {} {} ", idx + 1, label), style));
        spans.push(Span::raw(" "));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).wrap(Wrap { trim: false }),
        inner,
    );
}

const fn chip_active(chips: ChipSet, idx: usize) -> bool {
    match idx {
        0 => chips.mine,
        1 => chips.unassigned,
        2 => chips.in_review,
        3 => chips.active_sprint,
        4 => chips.global,
        _ => false,
    }
}

fn render_results(
    f: &mut Frame,
    area: Rect,
    local: &[RankedHit],
    jira: &JiraSearchState,
    selected: usize,
    focus: SearchFocus,
) {
    let focused = matches!(focus, SearchFocus::Result(_));
    let border_color = if focused { theme::BORDER_FOCUS } else { theme::MUTED };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(" Results ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut items: Vec<ListItem> = Vec::new();
    for hit in local {
        items.push(result_item(hit));
    }
    if let JiraSearchState::Loaded { hits, .. } = jira {
        for hit in hits {
            items.push(result_item(hit));
        }
    }

    if items.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                "No matches",
                Style::default().fg(theme::MUTED).add_modifier(Modifier::ITALIC),
            )),
            inner,
        );
        return;
    }

    let mut state = ListState::default();
    state.select(Some(selected));

    let list = List::new(items)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, inner, &mut state);
}

fn result_item(hit: &RankedHit) -> ListItem<'static> {
    let origin = match hit.origin {
        HitOrigin::Local => Span::styled(" local ", Style::default().fg(theme::MUTED)),
        HitOrigin::Jira => Span::styled(" jira  ", Style::default().fg(Color::Blue)),
    };
    let spans = vec![
        Span::raw(hit.issue_key.clone()),
        Span::raw("  "),
        origin,
    ];
    ListItem::new(Line::from(spans))
}

fn render_footer(f: &mut Frame, area: Rect, local: &[RankedHit], jira: &JiraSearchState) {
    let mut spans: Vec<Span> = Vec::new();
    spans.push(Span::styled(
        format!("{} local", local.len()),
        Style::default().fg(theme::MUTED),
    ));
    spans.push(Span::raw(" · "));
    match jira {
        JiraSearchState::Idle => {
            spans.push(Span::styled("jira: idle", Style::default().fg(theme::MUTED)));
        }
        JiraSearchState::Pending { .. } => {
            spans.push(Span::styled(
                "jira: searching…",
                Style::default().fg(Color::Blue),
            ));
        }
        JiraSearchState::Loaded { hits, .. } => {
            spans.push(Span::styled(
                format!("jira: {} results", hits.len()),
                Style::default().fg(theme::MUTED),
            ));
        }
        JiraSearchState::Error(msg) => {
            spans.push(Span::styled(
                format!("jira: error — {msg}"),
                Style::default().fg(Color::Red),
            ));
        }
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_preview(
    f: &mut Frame,
    area: Rect,
    app: &AppState,
    local: &[RankedHit],
    jira: &JiraSearchState,
    selected: usize,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::MUTED))
        .title(" Preview ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let selected_hit = current_hit(local, jira, selected);
    let Some(hit) = selected_hit else {
        f.render_widget(
            Paragraph::new(Span::styled(
                "No selection",
                Style::default().fg(theme::MUTED).add_modifier(Modifier::ITALIC),
            )),
            inner,
        );
        return;
    };

    let issue = find_issue(app, hit, jira);
    let Some(issue) = issue else {
        f.render_widget(
            Paragraph::new(Span::styled(
                "Issue unavailable",
                Style::default().fg(theme::MUTED),
            )),
            inner,
        );
        return;
    };

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            issue.key.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::raw(issue.fields.summary.clone()),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("Status: ", Style::default().fg(theme::MUTED)),
        Span::raw(issue.fields.status.name.clone()),
    ]));
    let assignee = issue
        .fields
        .assignee
        .as_ref()
        .map_or_else(|| "Unassigned".to_string(), |a| a.display().to_string());
    lines.push(Line::from(vec![
        Span::styled("Assignee: ", Style::default().fg(theme::MUTED)),
        Span::raw(assignee),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Project: ", Style::default().fg(theme::MUTED)),
        Span::raw(issue.fields.project.name.clone()),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Description",
        Style::default().fg(theme::MUTED).add_modifier(Modifier::BOLD),
    )));
    let desc_text = description_text(issue);
    lines.push(Line::from(desc_text));

    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        inner,
    );
}

fn current_hit<'a>(
    local: &'a [RankedHit],
    jira: &'a JiraSearchState,
    selected: usize,
) -> Option<&'a RankedHit> {
    if selected < local.len() {
        return local.get(selected);
    }
    let jira_idx = selected - local.len();
    match jira {
        JiraSearchState::Loaded { hits, .. } => hits.get(jira_idx),
        _ => None,
    }
}

fn find_issue<'a>(app: &'a AppState, hit: &RankedHit, jira: &'a JiraSearchState) -> Option<&'a Issue> {
    if let Some(issue) = app.issues.iter().find(|i| i.key == hit.issue_key) {
        return Some(issue);
    }
    if let JiraSearchState::Loaded { issues, .. } = jira {
        return issues.iter().find(|i| i.key == hit.issue_key);
    }
    None
}

fn description_text(issue: &Issue) -> String {
    use crate::jira::adf::adf_to_markdown;
    issue.fields.description.as_ref().map_or_else(
        || "(no description)".into(),
        |v| {
            let md = adf_to_markdown(v);
            if md.trim().is_empty() {
                "(no description)".into()
            } else {
                md
            }
        },
    )
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
