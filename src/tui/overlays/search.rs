use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::jira::types::Issue;
use crate::tui::app::{ActionState, AppState, JiraSearchState, SearchFocus};
use crate::tui::search::{ChipSet, RankedHit};
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

    let team_projects = crate::tui::team_project_keys(app);
    let ordered = ordered_hits(local_results, jira_state, &team_projects);

    render_input(f, left_chunks[0], query, cursor, focus == SearchFocus::Input);
    render_chips(f, left_chunks[1], active_chips, focus);
    render_results(f, left_chunks[2], app, jira_state, &ordered, selected, focus);
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

const CHIP_LABELS: [&str; 4] = ["Mine", "Unassigned", "In Review", "Active Sprint"];

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
        _ => false,
    }
}

fn render_results(
    f: &mut Frame,
    area: Rect,
    app: &AppState,
    jira: &JiraSearchState,
    ordered: &[&RankedHit],
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

    if ordered.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                "No matches",
                Style::default().fg(theme::MUTED).add_modifier(Modifier::ITALIC),
            )),
            inner,
        );
        return;
    }

    let items: Vec<ListItem> = ordered
        .iter()
        .map(|hit| result_item(hit, find_issue(app, hit, jira)))
        .collect();

    let mut state = ListState::default();
    state.select(Some(selected));

    let list = List::new(items)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, inner, &mut state);
}

fn result_item(hit: &RankedHit, issue: Option<&Issue>) -> ListItem<'static> {
    let summary = issue
        .map(|i| i.fields.summary.clone())
        .unwrap_or_default();
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
