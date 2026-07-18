use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::jira::types::Issue;
use crate::tui::app::{ActionState, AppState, JiraSearchState, SearchFocus};
use crate::tui::search::{RankedHit, SearchFilters};
use crate::tui::theme;
use crate::tui::widgets::scrollbar::render_scrollbar;

pub fn render_search_overlay(f: &mut Frame, app: &AppState, results_state: &mut ListState) {
    let ActionState::Searching {
        ref query,
        cursor,
        ref filters,
        focus,
        ref local_results,
        ref jira_state,
        selected,
        ref picker,
        ..
    } = app.action_state
    else {
        return;
    };
    let picker_open = picker.is_some();

    let area = centered_rect(90, 90, f.area());
    f.render_widget(Clear, area);

    let border_color = if picker_open {
        theme::MUTED
    } else {
        Color::Reset
    };
    let title_style = if picker_open {
        Style::default()
            .fg(theme::MUTED)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    };
    let mut outer = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(" Search ", title_style));
    if !picker_open {
        outer = outer.title_bottom(search_hints_line());
    }
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
            Constraint::Length(3), // filter row
            Constraint::Min(1),    // results
            Constraint::Length(1), // footer
        ])
        .split(left);

    let team_projects = crate::tui::team_project_keys(app);
    let ordered = ordered_hits(local_results, jira_state, &team_projects);

    render_input(
        f,
        left_chunks[0],
        query,
        cursor,
        !picker_open && focus == SearchFocus::Input,
    );
    render_filter_row(f, left_chunks[1], filters, focus, picker_open);
    let results_focused = !picker_open && matches!(focus, SearchFocus::Result(_));
    render_results(
        f,
        left_chunks[2],
        app,
        jira_state,
        &ordered,
        selected,
        results_focused,
        results_state,
    );
    render_footer(f, left_chunks[3], jira_state);

    render_preview(f, right, app, jira_state, &ordered, selected);
}

fn ordered_hits<'a>(
    local: &'a [RankedHit],
    jira: &'a JiraSearchState,
    team_projects: &[String],
) -> Vec<&'a RankedHit> {
    let mut out: Vec<&RankedHit> = local.iter().collect();
    if let JiraSearchState::Loaded { hits, issues } = jira {
        let (in_proj, rest): (Vec<&RankedHit>, Vec<&RankedHit>) = hits.iter().partition(|h| {
            issues
                .iter()
                .find(|i| i.key == h.issue_key)
                .is_some_and(|i| team_projects.iter().any(|p| p == &i.fields.project.key))
        });
        out.extend(in_proj);
        out.extend(rest);
    }
    out
}

fn render_input(f: &mut Frame, area: Rect, query: &str, cursor: usize, focused: bool) {
    let border_color = if focused {
        theme::BORDER_FOCUS
    } else {
        theme::MUTED
    };
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
        let x = inner
            .x
            .saturating_add(cursor_col.min(inner.width.saturating_sub(1)));
        f.set_cursor_position((x, inner.y));
    }
}

fn render_filter_row(
    f: &mut Frame,
    area: Rect,
    filters: &SearchFilters,
    focus: SearchFocus,
    picker_open: bool,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::MUTED))
        .title(" Filters ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let status_focused = !picker_open && focus == SearchFocus::StatusSlot;
    let project_focused = !picker_open && focus == SearchFocus::ProjectSlot;

    let mut spans: Vec<Span> = Vec::new();
    spans.extend(status_slot_spans(
        filters.statuses.len(),
        filters.statuses_exclude.len(),
        status_focused,
    ));
    spans.push(Span::raw("   "));
    spans.extend(filter_slot_spans(
        "2",
        "Project",
        filters.projects.len(),
        project_focused,
    ));

    f.render_widget(
        Paragraph::new(Line::from(spans)).wrap(Wrap { trim: false }),
        inner,
    );
}

fn status_slot_spans(
    include_count: usize,
    exclude_count: usize,
    focused: bool,
) -> Vec<Span<'static>> {
    let active = include_count > 0 || exclude_count > 0;
    let label_style = if active {
        Style::default()
            .fg(Color::Blue)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::MUTED)
    };
    let label_style = if focused {
        label_style.add_modifier(Modifier::REVERSED)
    } else {
        label_style
    };

    let mut spans = vec![
        Span::styled("1 ", Style::default().fg(theme::MUTED)),
        Span::styled("Status", label_style),
    ];
    if active {
        spans.push(Span::raw(" ["));
        if include_count > 0 {
            spans.push(Span::styled(
                format!("+{include_count}"),
                Style::default()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        if include_count > 0 && exclude_count > 0 {
            spans.push(Span::raw(" "));
        }
        if exclude_count > 0 {
            spans.push(Span::styled(
                format!("-{exclude_count}"),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ));
        }
        spans.push(Span::raw("]"));
    }
    spans
}

fn filter_slot_spans(digit: &str, label: &str, count: usize, focused: bool) -> Vec<Span<'static>> {
    let active = count > 0;
    let label_style = if active {
        Style::default()
            .fg(Color::Blue)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::MUTED)
    };
    let label_style = if focused {
        label_style.add_modifier(Modifier::REVERSED)
    } else {
        label_style
    };

    let mut spans = vec![
        Span::styled(format!("{digit} "), Style::default().fg(theme::MUTED)),
        Span::styled(label.to_string(), label_style),
    ];
    if active {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("[{count}]"),
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        ));
    }
    spans
}

#[allow(clippy::too_many_arguments)]
fn render_results(
    f: &mut Frame,
    area: Rect,
    app: &AppState,
    jira: &JiraSearchState,
    ordered: &[&RankedHit],
    selected: usize,
    focused: bool,
    state: &mut ListState,
) {
    let border_color = if focused {
        theme::BORDER_FOCUS
    } else {
        theme::MUTED
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(" Results ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    if ordered.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                "No matches",
                Style::default()
                    .fg(theme::MUTED)
                    .add_modifier(Modifier::ITALIC),
            )),
            inner,
        );
        return;
    }

    let items: Vec<ListItem> = ordered
        .iter()
        .map(|hit| result_item(hit, find_issue(app, hit, jira)))
        .collect();

    // The state persists across frames so the scroll offset survives;
    // rebuilding it each render pinned the selection to the bottom row.
    state.select(Some(selected));

    let list = List::new(items)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, inner, state);

    let total = ordered.len();
    let viewport = area.height.saturating_sub(2) as usize;
    if total > viewport {
        render_scrollbar(f, area, total, viewport, selected, border_color);
    }
}

fn result_item(hit: &RankedHit, issue: Option<&Issue>) -> ListItem<'static> {
    let summary = issue.map(|i| i.fields.summary.clone()).unwrap_or_default();
    ListItem::new(Line::from(vec![
        Span::raw(hit.issue_key.clone()),
        Span::raw("  "),
        Span::raw(summary),
    ]))
}

fn render_footer(f: &mut Frame, area: Rect, jira: &JiraSearchState) {
    let span = match jira {
        JiraSearchState::Idle => Span::styled("jira: idle", Style::default().fg(theme::MUTED)),
        JiraSearchState::Pending { .. } => {
            Span::styled("jira: searching…", Style::default().fg(Color::Blue))
        }
        JiraSearchState::Loaded { .. } => {
            Span::styled("jira: loaded", Style::default().fg(theme::MUTED))
        }
        JiraSearchState::Error(msg) => Span::styled(
            format!("jira: error — {msg}"),
            Style::default().fg(Color::Red),
        ),
    };
    f.render_widget(Paragraph::new(Line::from(span)), area);
}

fn render_preview(
    f: &mut Frame,
    area: Rect,
    app: &AppState,
    jira: &JiraSearchState,
    ordered: &[&RankedHit],
    selected: usize,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::MUTED))
        .title(" Preview ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let selected_hit = ordered.get(selected).copied();
    let Some(hit) = selected_hit else {
        f.render_widget(
            Paragraph::new(Span::styled(
                "No selection",
                Style::default()
                    .fg(theme::MUTED)
                    .add_modifier(Modifier::ITALIC),
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
        Style::default()
            .fg(theme::MUTED)
            .add_modifier(Modifier::BOLD),
    )));
    let desc_text = description_text(issue);
    lines.push(Line::from(desc_text));

    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn find_issue<'a>(
    app: &'a AppState,
    hit: &RankedHit,
    jira: &'a JiraSearchState,
) -> Option<&'a Issue> {
    if let Some(issue) = app
        .issues
        .iter()
        .find(|i| i.key() == hit.issue_key)
        .and_then(crate::items::WorkItem::as_jira)
    {
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

fn search_hints_line() -> Line<'static> {
    Line::from(vec![
        Span::styled("┤ ", Style::default().fg(Color::Reset)),
        Span::styled("Enter", Style::default().fg(Color::Green)),
        Span::raw(" open | "),
        Span::styled("Tab", Style::default().fg(Color::Blue)),
        Span::raw(" cycle | "),
        Span::styled("↕", Style::default().fg(Color::Blue)),
        Span::raw(" nav | "),
        Span::styled("Alt+1", Style::default().fg(Color::Blue)),
        Span::raw(" status | "),
        Span::styled("Alt+2", Style::default().fg(Color::Blue)),
        Span::raw(" project | "),
        Span::styled("Esc", Style::default().fg(Color::Magenta)),
        Span::raw(" close "),
        Span::styled("├──", Style::default().fg(Color::Reset)),
    ])
    .alignment(Alignment::Right)
    .style(Style::default().fg(theme::MUTED))
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
