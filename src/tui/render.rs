use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::tui::app::{ActionState, AppState, FocusedPanel};
use crate::tui::detail::render_detail;
use crate::tui::hint_bar::render_hints;
use crate::tui::overlays;
use crate::tui::theme;

/// Side-channel data written during a render pass, consumed by the event loop.
#[derive(Default)]
pub struct RenderOut {
    /// Virtual (top, bottom) row offsets for each focusable detail view item.
    /// Index: Comments=0, Attachments=1, Field(i)=2+i.
    pub detail_focus_offsets: Vec<(usize, usize)>,
    /// Height of the detail content viewport (inside the detail panel border).
    pub detail_viewport_h: usize,
    /// Total content lines returned by the active detail view renderer.
    pub detail_content_h: usize,
    /// Wrapped content height of the active confirm overlay (field/comment edit).
    pub confirm_content_h: usize,
    /// Viewport height of the active confirm overlay.
    pub confirm_viewport_h: usize,
    /// Content height (lines) of the sub-view overlay; written each render.
    pub overlay_content_h: usize,
    /// Viewport height of the sub-view overlay; written each render.
    pub overlay_viewport_h: usize,
    /// Virtual (top, bottom) row offsets for each comment widget; written each render.
    pub overlay_comment_offsets: Vec<(usize, usize)>,
}

pub fn render(
    f: &mut Frame,
    app: &AppState,
    list_state: &mut ratatui::widgets::ListState,
    render_out: &mut RenderOut,
) {
    let show_tabs = app.tab_list().len() > 1;

    // Layout: top bar (1) | [tab bar (1)] | main area (rest) | hint bar (1)
    let root = if show_tabs {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // title bar
                Constraint::Length(1), // tab bar
                Constraint::Min(0),    // main
                Constraint::Length(1), // hint bar
            ])
            .split(f.area())
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // title bar
                Constraint::Min(0),    // main
                Constraint::Length(1), // hint bar
            ])
            .split(f.area())
    };

    let (title_area, main_area, hint_area) = if show_tabs {
        (root[0], root[2], root[3])
    } else {
        (root[0], root[1], root[2])
    };

    // Title bar
    render_title(f, title_area, app);

    // Tab bar (only when multiple teams)
    if show_tabs {
        render_tab_bar(f, root[1], app);
    }

    let popup = app.popup_active();
    if app.fullscreen_detail {
        render_detail(f, main_area, app, !popup, render_out);
    } else if app.active_tab_source_kind() == Some(crate::config::types::SourceKind::Board) {
        crate::tui::board::render_board(f, main_area, app, !popup);
    } else {
        // List tabs and backlog tabs share the list+detail layout — a backlog
        // is just a rank-ordered list of one source.
        // Main: list (30%) | detail (70%)
        let main = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
            .split(main_area);

        crate::tui::list::render_list(
            f,
            main[0],
            app,
            list_state,
            app.focused_panel == FocusedPanel::List && !popup,
        );
        render_detail(
            f,
            main[1],
            app,
            app.focused_panel == FocusedPanel::Detail && !popup,
            render_out,
        );
    }

    // Hint bar
    render_hints(f, hint_area, app);

    // Sub-view popup overlay (comments / attachments)
    if app.overlay.is_some() {
        overlays::sub_view::render_sub_view_overlay(f, app, render_out);
    }

    // Overlays (drawn on top)
    render_action_overlays(f, app, render_out);
}

#[allow(clippy::too_many_lines)]
fn render_action_overlays(f: &mut Frame, app: &AppState, render_out: &mut RenderOut) {
    match &app.action_state {
        ActionState::SelectingTransition { .. } => {
            overlays::transition::render_transition_overlay(f, &app.action_state);
        }
        ActionState::SelectingBoardColumn { .. } => {
            overlays::board_column::render_board_column_overlay(f, &app.action_state);
        }
        ActionState::SelectingSprint { .. } => {
            overlays::sprint_picker::render_sprint_picker_overlay(f, &app.action_state);
        }
        ActionState::LoadingSprints { .. } => {
            overlays::await_spinner::render_await(f, "Fetching sprints…", app.tick_count);
        }
        ActionState::HidePopup { .. } => {
            overlays::hide::render_hide_overlay(f, &app.action_state, app.team_config());
        }
        ActionState::AwaitingAction { description } => {
            overlays::await_spinner::render_await(f, description, app.tick_count);
        }
        ActionState::LoadingTransitions { .. } => {
            overlays::await_spinner::render_await(f, "Fetching transitions…", app.tick_count);
        }
        ActionState::PendingTransition { .. } => {
            overlays::await_spinner::render_await(f, "Applying transition…", app.tick_count);
        }
        ActionState::PendingMoveToSprint { .. } => {
            overlays::await_spinner::render_await(f, "Moving to sprint…", app.tick_count);
        }
        ActionState::PendingHide { .. } => {
            overlays::await_spinner::render_await(f, "Hiding…", app.tick_count);
        }
        ActionState::PendingAssign { .. } => {
            overlays::await_spinner::render_await(f, "Assigning…", app.tick_count);
        }
        ActionState::PendingMove { .. } => {
            overlays::await_spinner::render_await(f, "Moving…", app.tick_count);
        }
        ActionState::PendingComment { .. }
        | ActionState::PendingFieldEdit { .. }
        | ActionState::PendingCommentEdit { .. } => {
            overlays::await_spinner::render_await(f, "Opening editor…", app.tick_count);
        }
        ActionState::LoadingFieldOptions { .. } => {
            overlays::await_spinner::render_await(f, "Fetching options…", app.tick_count);
        }
        ActionState::CommittingFieldEdit { .. } => {
            overlays::await_spinner::render_await(f, "Updating field…", app.tick_count);
        }
        ActionState::CommittingCommentEdit { .. } => {
            overlays::await_spinner::render_await(f, "Updating comment…", app.tick_count);
        }
        ActionState::DeletingComment { .. } => {
            overlays::await_spinner::render_await(f, "Deleting comment…", app.tick_count);
        }
        ActionState::DeletingAttachment { .. } => {
            overlays::await_spinner::render_await(f, "Deleting attachment…", app.tick_count);
        }
        ActionState::OpeningAttachment { .. } => {
            overlays::await_spinner::render_await(f, "Fetching attachment…", app.tick_count);
        }
        ActionState::ConfirmingFieldEdit { .. } => {
            overlays::field_edit_confirm::render_field_edit_confirm_overlay(
                f,
                &app.action_state,
                render_out,
            );
        }
        ActionState::OfferingTemplate { .. } => {
            overlays::template_preview::render_template_preview_overlay(
                f,
                &app.action_state,
                render_out,
            );
        }
        ActionState::SelectingFieldOption { .. } => {
            overlays::field_select::render_field_select_overlay(f, &app.action_state);
        }
        ActionState::SelectingFieldOptions { .. } => {
            overlays::field_multiselect::render_field_multiselect_overlay(f, &app.action_state);
        }
        ActionState::EditingDatetimeField { .. } => {
            overlays::datetime_picker::render_datetime_picker_overlay(f, &app.action_state);
        }
        ActionState::ConfirmingCommentEdit { .. } => {
            overlays::comment_edit_confirm::render_comment_edit_confirm_overlay(
                f,
                &app.action_state,
                render_out,
            );
        }
        ActionState::ConfirmingCommentDelete { selected, .. } => {
            overlays::delete_confirm::render_delete_confirm_overlay(
                f,
                " Delete comment? ",
                *selected,
                ("Yes", "No"),
                "This action cannot be undone.",
            );
        }
        ActionState::ConfirmingAttachmentDelete { selected, .. } => {
            overlays::delete_confirm::render_delete_confirm_overlay(
                f,
                " Delete attachment? ",
                *selected,
                ("Yes", "No"),
                "This action cannot be undone.",
            );
        }
        ActionState::ConfirmingCompleteTask { selected, .. } => {
            overlays::delete_confirm::render_delete_confirm_overlay(
                f,
                " Mark task complete? ",
                *selected,
                ("Yes", "No"),
                "The checkbox on the Confluence page will be ticked.",
            );
        }
        ActionState::PendingCompleteTask { .. } => {
            overlays::await_spinner::render_await(f, "Completing task…", app.tick_count);
        }
        ActionState::InlineEditingField { .. }
        | ActionState::TypingAttachmentPath { .. }
        | ActionState::None => {
            // Rendered inline / within overlay — no separate overlay needed
        }
        ActionState::PendingAttachmentUpload { .. } => {
            overlays::await_spinner::render_await(f, "Uploading…", app.tick_count);
        }
        ActionState::Error { error, scroll } => {
            render_error_overlay(f, &error.to_string(), *scroll, render_out);
        }
        ActionState::KeybindingsHelp => {
            overlays::keybindings::render_keybindings_overlay(f);
        }
        ActionState::Searching { picker, .. } => {
            overlays::search::render_search_overlay(f, app);
            if picker.is_some() {
                overlays::search_picker::render_search_picker_overlay(f, app);
            }
        }
        ActionState::CreatingIssue(_) => {
            overlays::create_issue::render_create_issue_overlay(f, app);
        }
        ActionState::CommittingCreate { .. } | ActionState::AwaitingCreate { .. } => {
            overlays::await_spinner::render_await(f, "Creating issue…", app.tick_count);
        }
        ActionState::IssueCreatedConfirm { key } => {
            overlays::create_issue::render_created_confirm_overlay(f, key);
        }
    }
}

fn render_title(f: &mut Frame, area: ratatui::layout::Rect, app: &AppState) {
    let version_span = if app.any_source_loading() {
        let frame =
            usize::try_from(app.tick_count).unwrap_or(0) % crate::tui::list::SPINNER_FRAMES.len();
        Span::styled(
            crate::tui::list::SPINNER_FRAMES[frame],
            Style::default().fg(theme::MUTED),
        )
    } else {
        Span::styled(
            concat!("v", env!("CARGO_PKG_VERSION")),
            Style::default().fg(theme::MUTED),
        )
    };
    let mut spans = vec![Span::raw("──── do-next "), version_span, Span::raw(" ")];
    if !app.update_warnings.is_empty() {
        let msg = app.update_warnings.join("; ");
        spans.push(Span::styled(
            format!("│ {msg} "),
            Style::default().fg(theme::MUTED),
        ));
    }
    let title = Line::from(spans).style(Style::default().fg(theme::MUTED));
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(theme::MUTED))
        .title_top(title);
    f.render_widget(block, area);
}

fn render_tab_bar(f: &mut Frame, area: ratatui::layout::Rect, app: &AppState) {
    use crate::tui::app::source_config_for;

    let tabs = app.tab_list();
    let active = app.active_tab_index();
    let mut spans = Vec::new();
    for (i, (team_idx, board)) in tabs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        let team = &app.resolved_teams[*team_idx];
        // Board/backlog tabs are labelled by the source's display name and
        // prefixed to read as a distinct kind of tab (▤ board, ≡ backlog).
        let label = board.as_ref().map_or_else(
            || format!(" {} ", team.id),
            |src_id| {
                let src = source_config_for(&team.config, src_id);
                let name = src.map_or(src_id.as_str(), |s| s.display_name());
                let prefix = match src.map(|s| s.kind) {
                    Some(crate::config::types::SourceKind::Backlog) => "≡",
                    _ => "▤",
                };
                format!(" {prefix} {name} ")
            },
        );
        if i == active {
            spans.push(Span::styled(
                label,
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(label, Style::default().fg(Color::DarkGray)));
        }
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_error_overlay(f: &mut Frame, msg: &str, scroll: u16, render_out: &mut RenderOut) {
    use crate::tui::widgets::scrollbar::render_scrollbar;
    use ratatui::{
        layout::{Alignment, Rect},
        widgets::{Clear, Wrap},
    };
    let area = centered_rect(60, 30, f.area());
    f.render_widget(Clear, area);

    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    let padded = Rect {
        x: inner.x + 1,
        width: inner.width.saturating_sub(2),
        ..inner
    };
    let viewport_h = padded.height as usize;

    let paragraph = Paragraph::new(msg.to_string()).wrap(Wrap { trim: false });
    let content_h = paragraph.line_count(padded.width);
    let max_scroll = u16::try_from(content_h.saturating_sub(viewport_h)).unwrap_or(u16::MAX);
    let display_scroll = scroll.min(max_scroll);
    let scrollable = content_h > viewport_h;

    render_out.confirm_content_h = content_h;
    render_out.confirm_viewport_h = viewport_h;

    let mut hint_spans = vec![Span::raw("┤ ")];
    if scrollable {
        hint_spans.push(Span::styled("↕", Style::default().fg(Color::Blue)));
        hint_spans.push(Span::raw(" | "));
    }
    hint_spans.push(Span::styled("q", Style::default().fg(Color::Magenta)));
    hint_spans.push(Span::raw(" close ├──"));
    let hint = Line::from(hint_spans).alignment(Alignment::Right);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Error ")
        .title_bottom(hint)
        .style(Style::default().fg(Color::Red));
    f.render_widget(block, area);
    f.render_widget(paragraph.scroll((display_scroll, 0)), padded);

    if scrollable {
        render_scrollbar(
            f,
            area,
            content_h - viewport_h + 1,
            viewport_h,
            display_scroll as usize,
            Color::Red,
        );
    }
}

pub fn centered_rect(
    percent_x: u16,
    percent_y: u16,
    r: ratatui::layout::Rect,
) -> ratatui::layout::Rect {
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
