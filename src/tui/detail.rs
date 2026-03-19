use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap},
};

use crate::tui::app::{AppState, NavItem, ViewMode, source_config_for};
use crate::tui::render::RenderOut;
use crate::tui::views;

pub fn render_detail(
    f: &mut Frame,
    area: Rect,
    app: &AppState,
    focused: bool,
    render_out: &mut RenderOut,
) {
    let accent = if focused { Color::Yellow } else { Color::Reset };
    let title = if app.view_mode == ViewMode::Postmortem {
        app.selected_issue().map(|i| {
            Span::styled(
                format!(" {} ", i.key),
                Style::default().add_modifier(Modifier::BOLD),
            )
        })
    } else {
        None
    };
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(accent));
    if let Some(t) = title {
        block = block.title(t);
    }
    let inner = block.inner(area);
    render_out.detail_viewport_h = inner.height as usize;
    f.render_widget(block, area);

    let total_lines = render_detail_content(f, inner, app, render_out);
    render_out.detail_content_h = total_lines;

    if total_lines > 0 {
        let viewport = area.height.saturating_sub(2) as usize;
        let mut scrollbar_state = ScrollbarState::new(total_lines)
            .viewport_content_length(viewport)
            .position(app.detail_scroll);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("┐"))
            .end_symbol(Some("┘"))
            .track_symbol(Some("│"))
            .track_style(Style::default())
            .thumb_style(Style::default().fg(accent));
        f.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
    }
}

fn render_detail_content(
    f: &mut Frame,
    area: Rect,
    app: &AppState,
    render_out: &mut RenderOut,
) -> usize {
    match app.selected_nav_item() {
        Some(NavItem::SourceError(source_id)) => {
            render_source_error(f, area, source_id, app);
            0
        }
        Some(NavItem::SubsourceError(source_id, sub_idx)) => {
            render_subsource_error(f, area, source_id, *sub_idx, app);
            0
        }
        Some(NavItem::Issue(_)) => {
            let Some(issue) = app.selected_issue() else {
                return 0;
            };
            let issue = issue.clone();
            match app.view_mode {
                ViewMode::Default => views::default::render_default(f, area, &issue, app),
                ViewMode::Incident => views::incident::render_incident(f, area, &issue, app),
                ViewMode::Postmortem => {
                    views::postmortem::render_postmortem(f, area, &issue, app, render_out)
                }
                ViewMode::Review => views::review::render_review(f, area, &issue, app),
                ViewMode::Comments => views::comments::render_comments(f, area, &issue, app),
            }
        }
        None => {
            let msg = if app.any_source_loading() {
                "Loading issues…"
            } else if app.issues.is_empty() {
                "No issues found."
            } else {
                ""
            };
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    msg,
                    Style::default().add_modifier(Modifier::DIM),
                ))),
                area,
            );
            0
        }
    }
}

fn render_subsource_error(
    f: &mut Frame,
    area: Rect,
    source_id: &str,
    sub_idx: usize,
    app: &AppState,
) {
    let Some(errors) = app.subsource_errors.get(source_id) else {
        return;
    };
    let Some((_, e)) = errors.iter().find(|(i, _)| *i == sub_idx) else {
        return;
    };

    let src_cfg = source_config_for(&app.config, source_id);
    let src_name = src_cfg.map_or_else(|| source_id.to_string(), |s| s.display_name().to_string());
    let badge = src_cfg
        .and_then(|s| s.subsources.get(sub_idx))
        .and_then(|s| s.badge.as_deref())
        .unwrap_or("subsource");
    let jql = match src_cfg {
        Some(s) if !s.jql.is_empty() => s.subsources.get(sub_idx).map_or_else(
            || s.jql.clone(),
            |sub| format!("({}) AND ({})", s.jql, sub.jql_filter),
        ),
        _ => "(unknown)".to_string(),
    };

    let block = Block::default().borders(Borders::NONE).title(Span::styled(
        format!(" {src_name} [{badge}] — fetch failed "),
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let lines = vec![
        Line::from(Span::styled(
            format!("{e:#}"),
            Style::default().fg(Color::Red),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!("JQL: {jql}"),
            Style::default().add_modifier(Modifier::DIM),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Run `do-next --log /tmp/do-next.log` for the full log.",
            Style::default().add_modifier(Modifier::DIM),
        )),
    ];
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn render_source_error(f: &mut Frame, area: Rect, source_id: &str, app: &AppState) {
    use crate::tui::app::SourceState;
    let Some(SourceState::Error(e)) = app.sources.get(source_id) else {
        return;
    };
    let src_cfg = source_config_for(&app.config, source_id);
    let src_name = src_cfg.map_or_else(|| source_id.to_string(), |s| s.display_name().to_string());
    let jql = src_cfg.map_or("(unknown)", |s| s.jql.as_str());

    let block = Block::default().borders(Borders::NONE).title(Span::styled(
        format!(" {src_name} — fetch failed "),
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let lines = vec![
        Line::from(Span::styled(
            format!("{e:#}"),
            Style::default().fg(Color::Red),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!("JQL: {jql}"),
            Style::default().add_modifier(Modifier::DIM),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Run `do-next --log /tmp/do-next.log` for the full log.",
            Style::default().add_modifier(Modifier::DIM),
        )),
    ];
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}
