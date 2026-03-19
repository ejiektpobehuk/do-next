use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders},
};

use crate::tui::app::{ActionState, AppState, FocusedPanel, ViewMode};

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

    let list_focused = app.focused_panel == FocusedPanel::List;
    let can_move_vertical = if list_focused {
        !app.nav_items.is_empty()
    } else {
        app.selected_issue().is_some()
    };
    let nav_color = |active: bool| {
        if active { Color::Blue } else { Color::DarkGray }
    };

    let in_postmortem_detail = app.focused_panel == FocusedPanel::Detail
        && app.view_mode == ViewMode::Postmortem
        && app.selected_issue().is_some();

    let mut hints: Vec<Span> = vec![Span::raw("┤ ")];
    if in_postmortem_detail {
        let field_idx = app.postmortem_field_idx;
        let pm_cfg = app.config.view_modes.postmortem.as_ref();
        let is_readonly =
            crate::tui::views::postmortem::postmortem_field_is_readonly(pm_cfg, field_idx);
        let enter_label = if is_readonly {
            // Show "open link" hint only when the field value looks like a URL
            let field_id = crate::tui::views::postmortem::postmortem_field_cfg(pm_cfg, field_idx)
                .map_or("", |f| f.field_id.as_str());
            let is_link = app
                .selected_issue()
                .and_then(|i| i.fields.extra.get(field_id))
                .and_then(|v| v.as_str())
                .is_some_and(|s| s.starts_with("http://") || s.starts_with("https://"));
            if is_link { Some("open link") } else { None }
        } else {
            Some("edit field")
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
