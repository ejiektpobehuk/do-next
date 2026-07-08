use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders},
};

use crate::tui::app::{ActionState, AppState, DetailFocus, FocusedPanel, ViewMode};
use crate::tui::theme;

/// Renders hints for modal action states. Returns `true` if a modal state was
/// handled (caller should return early).
fn try_render_modal_hints(f: &mut Frame, area: Rect, action_state: &ActionState) -> bool {
    match action_state {
        ActionState::KeybindingsHelp
        | ActionState::EditingDatetimeField { .. }
        | ActionState::ConfirmingFieldEdit { .. }
        | ActionState::Searching { .. }
        | ActionState::CreatingIssue(_)
        | ActionState::IssueCreatedConfirm { .. } => {
            f.render_widget(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .border_style(Style::default().fg(theme::MUTED)),
                area,
            );
        }
        ActionState::InlineEditingField { .. } => {
            let title = Line::from(vec![
                Span::raw("┤ "),
                Span::styled("Enter", Style::default().fg(Color::Green)),
                Span::raw(" save  "),
                Span::styled("Esc", Style::default().fg(Color::Magenta)),
                Span::raw(" cancel ├──"),
            ])
            .alignment(Alignment::Right)
            .style(Style::default().fg(theme::MUTED));
            f.render_widget(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .border_style(Style::default().fg(theme::MUTED))
                    .title_bottom(title),
                area,
            );
        }
        ActionState::SelectingFieldOption { .. } | ActionState::SelectingFieldOptions { .. } => {
            let title = Line::from(vec![
                Span::raw("┤ "),
                Span::styled("↕", Style::default().fg(Color::Blue)),
                Span::raw(" navigate  "),
                Span::styled("Enter", Style::default().fg(Color::Green)),
                Span::raw(" confirm  "),
                Span::styled("Esc", Style::default().fg(Color::Magenta)),
                Span::raw(" cancel ├──"),
            ])
            .alignment(Alignment::Right)
            .style(Style::default().fg(theme::MUTED));
            f.render_widget(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .border_style(Style::default().fg(theme::MUTED))
                    .title_bottom(title),
                area,
            );
        }
        _ => return false,
    }
    true
}

#[allow(clippy::too_many_lines)]
pub fn render_hints(f: &mut Frame, area: Rect, app: &AppState) {
    if try_render_modal_hints(f, area, &app.action_state) {
        return;
    }

    if app.overlay.is_some() {
        f.render_widget(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(theme::MUTED)),
            area,
        );
        return;
    }

    if app.board_view.is_some() && !app.fullscreen_detail {
        let title = Line::from(vec![
            Span::raw("┤ "),
            Span::styled("←→", Style::default().fg(Color::Blue)),
            Span::raw(" column  "),
            Span::styled("↕", Style::default().fg(Color::Blue)),
            Span::raw(" card/lane  "),
            Span::styled("↵", Style::default().fg(Color::Blue)),
            Span::raw(" open  "),
            Span::styled("t", Style::default().fg(Color::Blue)),
            Span::raw(" move  "),
            Span::styled("P", Style::default().fg(Color::Blue)),
            Span::raw(" preload  "),
            Span::styled("Tab", Style::default().fg(Color::Blue)),
            Span::raw(" switch  "),
            Span::styled("?", Style::default().fg(Color::Blue)),
            Span::raw(" ├──"),
        ])
        .alignment(Alignment::Right)
        .style(Style::default().fg(theme::MUTED));
        f.render_widget(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(theme::MUTED))
                .title_bottom(title),
            area,
        );
        return;
    }

    let list_focused = app.focused_panel == FocusedPanel::List;
    let can_move_vertical = if list_focused {
        !app.nav_items.is_empty()
    } else {
        app.selected_item().is_some()
    };
    let nav_color = |active: bool| {
        if active { Color::Blue } else { Color::DarkGray }
    };

    let in_detail_view = app.focused_panel == FocusedPanel::Detail
        && matches!(app.view_mode, ViewMode::Default | ViewMode::Custom(_))
        && app.selected_item().is_some();

    let mut hints: Vec<Span> = vec![Span::raw("┤ ")];
    if in_detail_view {
        let view_cfg = crate::tui::views::custom::current_view_config(app);
        let selected_item = app.selected_item();
        let enter_label = match &app.detail_focus {
            DetailFocus::Comments => Some("view comments"),
            DetailFocus::Attachments => Some("view attachments"),
            DetailFocus::Field(field_idx) => {
                let field_idx = *field_idx;
                let field_cfg =
                    crate::tui::views::custom::view_field_cfg(view_cfg, selected_item, field_idx);
                let is_readonly = field_cfg.as_ref().and_then(|f| f.readonly).unwrap_or(false)
                    || selected_item.is_some_and(|i| !i.supports_field_edit());
                if is_readonly {
                    let field_id = field_cfg
                        .as_ref()
                        .map(|f| f.field_id.clone())
                        .unwrap_or_default();
                    let url_str = app
                        .selected_item()
                        .and_then(|i| i.field(&field_id))
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
    hints.push(Span::raw(" | "));
    if app.board_view.is_some() && app.fullscreen_detail {
        // Detail opened from a board: q/Esc step back to the board.
        hints.push(Span::styled("q/Esc", Style::default().fg(Color::Blue)));
        hints.push(Span::raw(" board ├──"));
    } else {
        hints.push(Span::raw("("));
        hints.push(Span::styled("q", Style::default().fg(Color::Red)));
        hints.push(Span::raw(")uit ├──"));
    }

    let title = Line::from(hints)
        .alignment(Alignment::Right)
        .style(Style::default().fg(theme::MUTED));

    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(theme::MUTED))
        .title_bottom(title);
    f.render_widget(block, area);
}
