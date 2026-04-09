use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders},
};

use crate::tui::app::{ActionState, AppState, DetailFocus, FocusedPanel, ViewMode};

/// Renders hints for modal action states. Returns `true` if a modal state was
/// handled (caller should return early).
fn try_render_modal_hints(f: &mut Frame, area: Rect, action_state: &ActionState) -> bool {
    match action_state {
        ActionState::KeybindingsHelp
        | ActionState::EditingDatetimeField { .. }
        | ActionState::ConfirmingFieldEdit { .. } => {
            f.render_widget(Block::default().borders(Borders::BOTTOM), area);
        }
        ActionState::InlineEditingField { .. } => {
            let title = Line::from(vec![
                Span::raw("┤ "),
                Span::styled("Enter", Style::default().fg(Color::Blue)),
                Span::raw(" save  "),
                Span::styled("Esc", Style::default().fg(Color::Blue)),
                Span::raw(" cancel ├──"),
            ])
            .alignment(Alignment::Right);
            f.render_widget(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .title_bottom(title),
                area,
            );
        }
        ActionState::SelectingFieldOption { .. } | ActionState::SelectingFieldOptions { .. } => {
            let title = Line::from(vec![
                Span::raw("┤ "),
                Span::styled("↕", Style::default().fg(Color::Blue)),
                Span::raw(" navigate  "),
                Span::styled("Enter", Style::default().fg(Color::Blue)),
                Span::raw(" confirm  "),
                Span::styled("Esc", Style::default().fg(Color::Blue)),
                Span::raw(" cancel ├──"),
            ])
            .alignment(Alignment::Right);
            f.render_widget(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .title_bottom(title),
                area,
            );
        }
        _ => return false,
    }
    true
}

pub fn render_hints(f: &mut Frame, area: Rect, app: &AppState) {
    if try_render_modal_hints(f, area, &app.action_state) {
        return;
    }

    if app.overlay.is_some() {
        f.render_widget(Block::default().borders(Borders::BOTTOM), area);
        return;
    }

    let list_focused = app.focused_panel == FocusedPanel::List;
    let can_move_vertical = if list_focused {
        !app.nav_items.is_empty()
    } else {
        app.selected_issue().is_some()
    };
    let nav_color = |active: bool| {
        if active { Color::Blue } else { Color::DarkGray }
    };

    let in_detail_view = app.focused_panel == FocusedPanel::Detail
        && matches!(app.view_mode, ViewMode::Default | ViewMode::Custom(_))
        && app.selected_issue().is_some();

    let mut hints: Vec<Span> = vec![Span::raw("┤ ")];
    if in_detail_view {
        let view_cfg = crate::tui::views::custom::current_view_config(app);
        let selected_issue = app.selected_issue();
        let enter_label = match &app.detail_focus {
            DetailFocus::Comments => Some("view comments"),
            DetailFocus::Attachments => Some("view attachments"),
            DetailFocus::Field(field_idx) => {
                let field_idx = *field_idx;
                let field_cfg =
                    crate::tui::views::custom::view_field_cfg(view_cfg, selected_issue, field_idx);
                let is_readonly = field_cfg.as_ref().and_then(|f| f.readonly).unwrap_or(false);
                if is_readonly {
                    let field_id = field_cfg
                        .as_ref()
                        .map(|f| f.field_id.clone())
                        .unwrap_or_default();
                    let url_str = app
                        .selected_issue()
                        .and_then(|i| i.fields.extra.get(&field_id))
                        .and_then(|v| v.as_str())
                        .filter(|s| s.starts_with("http://") || s.starts_with("https://"));
                    url_str.map(|url| {
                        let team = &app.resolved_teams[app.active_team_idx];
                        let open_with = field_cfg.as_ref().and_then(|f| f.open_with.as_deref());
                        let use_slack = match open_with {
                            Some("browser") => false,
                            Some("slack") => true,
                            _ => {
                                team.open_slack_in_app
                                    && team.slack_team_id.is_some()
                                    && url.contains(".slack.com/")
                            }
                        };
                        if use_slack {
                            "open in Slack"
                        } else {
                            "open link"
                        }
                    })
                } else {
                    Some("edit field")
                }
            }
        };
        if let Some(lbl) = enter_label {
            hints.push(Span::styled("↵", Style::default().fg(Color::Blue)));
            hints.push(Span::raw(format!(" {lbl}")));
            hints.push(Span::raw(" | "));
        }
    }
    hints.push(Span::styled(
        "←",
        Style::default().fg(nav_color(!list_focused)),
    ));
    hints.push(Span::styled(
        "↕",
        Style::default().fg(nav_color(can_move_vertical)),
    ));
    hints.push(Span::styled(
        "→",
        Style::default().fg(nav_color(list_focused)),
    ));
    hints.push(Span::raw(" | "));
    if app.resolved_teams.len() > 1 {
        hints.push(Span::styled("Tab", Style::default().fg(Color::Blue)));
        hints.push(Span::raw(" team | "));
    }
    hints.push(Span::styled("?", Style::default().fg(Color::Blue)));
    hints.push(Span::raw(" | ("));
    hints.push(Span::styled("q", Style::default().fg(Color::Red)));
    hints.push(Span::raw(")uit ├──"));

    let title = Line::from(hints).alignment(Alignment::Right);

    let block = Block::default()
        .borders(Borders::BOTTOM)
        .title_bottom(title);
    f.render_widget(block, area);
}
