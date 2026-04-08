use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, List, ListItem, ListState, Scrollbar, ScrollbarOrientation, ScrollbarState,
    },
};

use crate::config::types::SourceConfig;
use crate::tui::app::{AppState, NavItem, SourceState, source_config_for};

pub const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn render_list(
    f: &mut Frame,
    area: Rect,
    app: &AppState,
    list_state: &mut ListState,
    focused: bool,
) {
    let mut items: Vec<ListItem> = Vec::new();
    // Index into `app.nav_items` for each visual row (None for non-navigable rows).
    let mut item_nav_indices: Vec<Option<usize>> = Vec::new();

    let mut issue_pos = 0usize;

    // Build display items by iterating source order
    for (source_id, state) in &app.sources {
        let src_cfg = source_config_for(app.team_config(), source_id);

        // Add source separator
        let sep_text = source_separator_text(source_id, src_cfg);
        items.push(ListItem::new(Line::from(Span::styled(
            sep_text,
            Style::default().add_modifier(Modifier::DIM),
        ))));
        item_nav_indices.push(None);

        match state {
            SourceState::Loaded(_) => {
                push_loaded_source_items(
                    &mut items,
                    &mut item_nav_indices,
                    app,
                    source_id,
                    src_cfg,
                    &mut issue_pos,
                );
            }
            SourceState::Error(msg) => {
                let nav_pos = app
                    .nav_items
                    .iter()
                    .position(|n| *n == NavItem::SourceError(source_id.clone()));
                let is_selected = nav_pos == Some(app.nav_idx);
                let msg = msg.to_string();
                let short = msg.chars().take(40).collect::<String>();
                let style = if is_selected {
                    Style::default()
                        .fg(Color::Red)
                        .add_modifier(Modifier::REVERSED)
                } else {
                    Style::default().fg(Color::Red)
                };
                items.push(ListItem::new(Line::from(Span::styled(
                    format!("  ✗ {short}"),
                    style,
                ))));
                item_nav_indices.push(nav_pos);
            }
            SourceState::Pending | SourceState::Loading => {
                let frame = usize::try_from(app.tick_count).unwrap_or(0) % SPINNER_FRAMES.len();
                let spinner = SPINNER_FRAMES[frame];
                items.push(ListItem::new(Line::from(Span::styled(
                    format!("  {spinner} Loading…"),
                    Style::default().fg(Color::DarkGray),
                ))));
                item_nav_indices.push(None);
            }
        }
    }

    // Sync list_state selection to the visual row that matches the current nav_idx.
    let list_selected = item_nav_indices
        .iter()
        .position(|opt| *opt == Some(app.nav_idx));
    *list_state = ListState::default();
    if let Some(pos) = list_selected {
        list_state.select(Some(pos));
    }

    let total = items.len();

    let accent = if focused { Color::Yellow } else { Color::Reset };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(accent));
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    f.render_stateful_widget(list, area, list_state);

    let viewport = area.height.saturating_sub(2) as usize;
    if total > viewport {
        let mut scrollbar_state = ScrollbarState::new(total)
            .viewport_content_length(viewport)
            .position(list_state.selected().unwrap_or(0));
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("┐"))
            .end_symbol(Some("┘"))
            .track_symbol(Some("│"))
            .track_style(Style::default())
            .thumb_style(Style::default().fg(accent));
        f.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
    }
}

fn push_loaded_source_items(
    items: &mut Vec<ListItem<'static>>,
    item_nav_indices: &mut Vec<Option<usize>>,
    app: &AppState,
    source_id: &str,
    src_cfg: Option<&SourceConfig>,
    issue_pos: &mut usize,
) {
    let start = *issue_pos;
    while *issue_pos < app.issues.len()
        && app.issues[*issue_pos].source_id.as_deref() == Some(source_id)
    {
        *issue_pos += 1;
    }
    let source_issues = &app.issues[start..*issue_pos];

    if source_issues.is_empty() {
        items.push(ListItem::new(Line::from(Span::styled(
            "  (no issues)",
            Style::default().add_modifier(Modifier::DIM),
        ))));
        item_nav_indices.push(None);
    }

    for (i, issue) in source_issues.iter().enumerate() {
        let abs_idx = start + i;
        let sub = issue.subsource_idx;
        let is_sub_break = i > 0 && sub != source_issues[i - 1].subsource_idx;
        if is_sub_break {
            items.push(ListItem::new(Line::from(Span::raw(""))));
            item_nav_indices.push(None);
        }
        let nav_pos = app
            .nav_items
            .iter()
            .position(|n| *n == NavItem::Issue(abs_idx));
        let is_selected = nav_pos == Some(app.nav_idx);
        let row = issue_row(issue, src_cfg, app, is_selected);
        items.push(ListItem::new(row));
        item_nav_indices.push(nav_pos);
    }

    // Subsource error rows (after successfully loaded issues).
    let Some(errors) = app.subsource_errors.get(source_id) else {
        return;
    };
    for (sub_idx, error) in errors {
        let nav_pos = app
            .nav_items
            .iter()
            .position(|n| *n == NavItem::SubsourceError(source_id.to_owned(), *sub_idx));
        let is_selected = nav_pos == Some(app.nav_idx);
        let badge = src_cfg
            .and_then(|s| s.subsources.get(*sub_idx))
            .and_then(|s| s.badge.as_deref())
            .unwrap_or("subsource");
        let msg = error.to_string();
        let short = msg.chars().take(40).collect::<String>();
        let style = if is_selected {
            Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::REVERSED)
        } else {
            Style::default().fg(Color::Red)
        };
        items.push(ListItem::new(Line::from(Span::styled(
            format!("  ✗ [{badge}] {short}"),
            style,
        ))));
        item_nav_indices.push(nav_pos);
    }
}

fn source_separator_text(source_id: &str, src_cfg: Option<&SourceConfig>) -> String {
    if let Some(cfg) = src_cfg {
        if let Some(ref ind) = cfg.indication
            && let Some(ref text) = ind.separator_text
        {
            return text.clone();
        }
        format!("── {} ──", cfg.display_name())
    } else {
        format!("── {source_id} ──")
    }
}

fn issue_row(
    issue: &crate::jira::types::Issue,
    src_cfg: Option<&SourceConfig>,
    app: &AppState,
    selected: bool,
) -> Line<'static> {
    let indication = src_cfg
        .and_then(|s| s.indication.clone())
        .or_else(|| app.team_config().list.default_indication.clone())
        .unwrap_or_default();

    let color = parse_color(&indication.color);
    let symbol = indication.symbol;

    let key = format!("{:<12}", issue.key);
    let priority = issue
        .fields
        .priority
        .as_ref()
        .map_or("·", super::super::jira::types::PriorityField::symbol);
    let status = format!("{:>16}", truncate(&issue.fields.status.name, 16));

    let badges = issue_badges(issue, src_cfg);
    let summary_raw = format!("{} {}", issue.fields.summary, badges);
    let summary = truncate(&summary_raw, 60).to_string();

    let base_style = if selected {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default().fg(color)
    };

    Line::from(vec![
        Span::styled(format!("{symbol} "), base_style),
        Span::styled(key, base_style),
        Span::styled(format!("{priority} "), base_style),
        Span::styled(summary, base_style),
        Span::styled(
            format!(" {status}"),
            Style::default().add_modifier(Modifier::DIM),
        ),
    ])
}

fn issue_badges(issue: &crate::jira::types::Issue, src_cfg: Option<&SourceConfig>) -> String {
    let mut badges = Vec::new();
    let Some(cfg) = src_cfg else {
        return String::new();
    };

    // Wrong project badge
    if let Some(ref expected) = cfg.expected_project
        && issue.fields.project.key != *expected
    {
        badges.push("[!proj]".to_string());
    }

    // Source-level badges declared in config
    for badge in &cfg.badges {
        match badge.as_str() {
            "stale" => badges.push("[stale]".to_string()),
            "assignee" => {
                if let Some(ref a) = issue.fields.assignee {
                    badges.push(format!("@{}", a.display()));
                }
            }
            _ => {}
        }
    }

    // Subsource-level badge (unassigned, reviewing, etc.)
    if let Some(sub_cfg) = cfg.subsources.get(issue.subsource_idx)
        && let Some(ref badge) = sub_cfg.badge
    {
        match badge.as_str() {
            "unassigned" => badges.push("[unassigned]".to_string()),
            "reviewing" => badges.push("[reviewing]".to_string()),
            other => badges.push(format!("[{other}]")),
        }
    }

    badges.join(" ")
}

fn parse_color(name: &str) -> Color {
    match name {
        "red" => Color::Red,
        "yellow" => Color::Yellow,
        "green" => Color::Green,
        "blue" => Color::Blue,
        "cyan" => Color::Cyan,
        "magenta" => Color::Magenta,
        _ => Color::Reset,
    }
}

fn truncate(s: &str, max_chars: usize) -> &str {
    if s.chars().count() <= max_chars {
        return s;
    }
    // Find byte boundary at max_chars - 1 chars to append "…"
    let mut idx = 0;
    for (i, _) in s.char_indices().take(max_chars.saturating_sub(1)) {
        idx = i;
    }
    &s[..idx]
}
