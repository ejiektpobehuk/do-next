use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::tui::app::{ActionState, AppState, FocusedPanel};
use crate::tui::detail::render_detail;
use crate::tui::hint_bar::render_hints;
use crate::tui::overlays;

/// Side-channel data written during a render pass, consumed by the event loop.
#[derive(Default)]
pub struct RenderOut {
    /// Virtual (top, bottom) row offsets for each focusable postmortem item.
    /// Index: Comments=0, Attachments=1, Field(i)=2+i.
    pub postmortem_focus_offsets: Vec<(usize, usize)>,
    /// Height of the detail content viewport (inside the detail panel border).
    pub detail_viewport_h: usize,
    /// Total content lines returned by the active detail view renderer.
    pub detail_content_h: usize,
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
    // Layout: top bar (1) | main area (rest) | hint bar (1)
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title bar
            Constraint::Min(0),    // main
            Constraint::Length(1), // hint bar
        ])
        .split(f.area());

    // Title bar
    render_title(f, root[0], app);

    // Main: list (30%) | detail (70%)
    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(root[1]);

    crate::tui::list::render_list(
        f,
        main[0],
        app,
        list_state,
        app.focused_panel == FocusedPanel::List,
    );
    render_detail(
        f,
        main[1],
        app,
        app.focused_panel == FocusedPanel::Detail,
        render_out,
    );

    // Hint bar
    render_hints(f, root[2], app);

    // Sub-view popup overlay (comments / attachments from postmortem)
    if app.overlay.is_some() {
        overlays::sub_view::render_sub_view_overlay(f, app, render_out);
    }

    // Overlays (drawn on top)
    render_action_overlays(f, app);
}

fn render_action_overlays(f: &mut Frame, app: &AppState) {
    match &app.action_state {
        ActionState::SelectingTransition { .. } => {
            overlays::transition::render_transition_overlay(f, &app.action_state);
        }
        ActionState::HidePopup { .. } => {
            overlays::hide::render_hide_overlay(f, &app.action_state, &app.config);
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
        ActionState::OpeningAttachment { .. } => {
            overlays::await_spinner::render_await(f, "Fetching attachment…", app.tick_count);
        }
        ActionState::ConfirmingFieldEdit { .. } => {
            overlays::field_edit_confirm::render_field_edit_confirm_overlay(f, &app.action_state);
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
            );
        }
        ActionState::ConfirmingCommentDelete { .. } => {
            overlays::comment_delete_confirm::render_comment_delete_confirm_overlay(
                f,
                &app.action_state,
            );
        }
        ActionState::InlineEditingField { .. } | ActionState::None => {
            // Rendered inline within the postmortem view — no overlay needed
        }
        ActionState::Error(msg) => {
            render_error_overlay(f, &msg.to_string());
        }
        ActionState::KeybindingsHelp => {
            overlays::keybindings::render_keybindings_overlay(f);
        }
    }
}

fn render_title(f: &mut Frame, area: ratatui::layout::Rect, app: &AppState) {
    let version_span = if app.any_source_loading() {
        let frame =
            usize::try_from(app.tick_count).unwrap_or(0) % crate::tui::list::SPINNER_FRAMES.len();
        Span::styled(
            crate::tui::list::SPINNER_FRAMES[frame],
            Style::default().fg(Color::Yellow),
        )
    } else {
        Span::styled(
            concat!("v", env!("CARGO_PKG_VERSION")),
            Style::default().fg(Color::DarkGray),
        )
    };
    let title = Line::from(vec![
        Span::raw("──── do-next "),
        version_span,
        Span::raw(" "),
    ]);
    let block = Block::default().borders(Borders::TOP).title_top(title);
    f.render_widget(block, area);
}

fn render_error_overlay(f: &mut Frame, msg: &str) {
    use ratatui::widgets::Clear;
    let area = centered_rect(60, 30, f.area());
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Error ")
        .style(Style::default().fg(Color::Red));
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(
        Paragraph::new(format!("{msg}\n\nPress any key to dismiss.")),
        inner,
    );
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
