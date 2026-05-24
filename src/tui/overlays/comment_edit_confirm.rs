use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::tui::app::ActionState;
use crate::tui::markdown::markdown_to_lines;
use crate::tui::render::RenderOut;
use crate::tui::widgets::scrollbar::render_scrollbar;

pub fn render_comment_edit_confirm_overlay(
    f: &mut Frame,
    app_action: &ActionState,
    render_out: &mut RenderOut,
) {
    let ActionState::ConfirmingCommentEdit {
        issue_key,
        old_text,
        new_text,
        tab,
        scroll,
        ..
    } = app_action
    else {
        return;
    };

    let area = centered_rect(70, 75, f.area());
    f.render_widget(Clear, area);

    // Block uses Borders::ALL; inner is `area` shrunk by 1 on each side. We compute it
    // up front so we can measure content height and decide whether ↕ is active before
    // constructing the hint.
    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    let viewport_h = inner.height as usize;

    let (paragraph, content_area) = build_tab_content(*tab, inner, old_text, new_text);
    let content_h = paragraph.line_count(content_area.width);
    let max_scroll = u16::try_from(content_h.saturating_sub(viewport_h)).unwrap_or(u16::MAX);
    let display_scroll = (*scroll).min(max_scroll);
    let scrollable = content_h > viewport_h;

    render_out.confirm_content_h = content_h;
    render_out.confirm_viewport_h = viewport_h;

    let nav_color = |active: bool| {
        if active { Color::Blue } else { Color::DarkGray }
    };
    let hint = Line::from(vec![
        Span::raw("┤ "),
        Span::styled("↵", Style::default().fg(Color::Green)),
        Span::raw(" confirm  "),
        Span::styled("←", Style::default().fg(nav_color(*tab == 1))),
        Span::styled("↕", Style::default().fg(nav_color(scrollable))),
        Span::styled("→", Style::default().fg(nav_color(*tab == 0))),
        Span::raw(" & "),
        Span::styled("tab", Style::default().fg(Color::Blue)),
        Span::raw("  "),
        Span::styled("q", Style::default().fg(Color::Magenta)),
        Span::raw(" cancel ├──"),
    ])
    .alignment(Alignment::Right);

    let (tab_preview_l, tab_preview_r) = if *tab == 0 {
        (
            Span::raw("─"),
            Span::styled(
                " Preview ",
                Style::default().add_modifier(Modifier::REVERSED),
            ),
        )
    } else {
        (
            Span::raw("┤ "),
            Span::styled("Preview ", Style::default().fg(Color::DarkGray)),
        )
    };
    let (tab_diff_l, tab_diff_r) = if *tab == 1 {
        (
            Span::styled(" Diff ", Style::default().add_modifier(Modifier::REVERSED)),
            Span::raw("─"),
        )
    } else {
        (
            Span::styled(" Diff", Style::default().fg(Color::DarkGray)),
            Span::raw(" ├"),
        )
    };
    let tabs = Line::from(vec![
        tab_preview_l,
        tab_preview_r,
        tab_diff_l,
        tab_diff_r,
        Span::raw("─"),
    ])
    .alignment(Alignment::Right);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Confirm comment edit · {issue_key} "))
        .title_top(tabs)
        .title_bottom(hint);
    f.render_widget(block, area);

    f.render_widget(paragraph.scroll((display_scroll, 0)), content_area);

    if scrollable {
        // Ratatui treats `content_length - 1` as the max position (last line at top of
        // viewport). We allow scrolling only until the last line reaches the bottom of
        // the viewport, so pass `max_scroll + 1` here for the thumb to reach the end.
        render_scrollbar(
            f,
            area,
            content_h - viewport_h + 1,
            viewport_h,
            display_scroll as usize,
            Color::Reset,
        );
    }
}

/// Build the active tab's `Paragraph` and the `Rect` it should render into.
/// Preview is padded by 2 on the left to align with the diff prefix.
fn build_tab_content<'a>(
    tab: usize,
    inner: Rect,
    old_text: &'a str,
    new_text: &'a str,
) -> (Paragraph<'a>, Rect) {
    if tab == 0 {
        let padded = Rect {
            x: inner.x + 2,
            width: inner.width.saturating_sub(2),
            ..inner
        };
        let lines = markdown_to_lines(new_text);
        (Paragraph::new(lines).wrap(Wrap { trim: false }), padded)
    } else {
        let lines = diff_lines(old_text, new_text);
        let rendered: Vec<Line> = lines
            .into_iter()
            .map(|dl| match dl {
                DiffLine::Same(s) => Line::from(Span::styled(
                    format!("  {s}"),
                    Style::default().fg(Color::DarkGray),
                )),
                DiffLine::Removed(s) => Line::from(Span::styled(
                    format!("- {s}"),
                    Style::default().fg(Color::Red),
                )),
                DiffLine::Added(s) => Line::from(Span::styled(
                    format!("+ {s}"),
                    Style::default().fg(Color::Green),
                )),
            })
            .collect();
        (Paragraph::new(rendered).wrap(Wrap { trim: false }), inner)
    }
}

enum DiffLine<'a> {
    Same(&'a str),
    Added(&'a str),
    Removed(&'a str),
}

fn diff_lines<'a>(old: &'a str, new: &'a str) -> Vec<DiffLine<'a>> {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let m = old_lines.len();
    let n = new_lines.len();

    let mut dp = vec![vec![0usize; n + 1]; m + 1];
    for i in (0..m).rev() {
        for j in (0..n).rev() {
            dp[i][j] = if old_lines[i] == new_lines[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }

    let mut result = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < m || j < n {
        let lines_match = i < m && j < n && { old_lines[i] == new_lines[j] };
        if lines_match {
            result.push(DiffLine::Same(old_lines[i]));
            i += 1;
            j += 1;
        } else if i < m && (j >= n || dp[i + 1][j] >= dp[i][j + 1]) {
            result.push(DiffLine::Removed(old_lines[i]));
            i += 1;
        } else {
            result.push(DiffLine::Added(new_lines[j]));
            j += 1;
        }
    }
    result
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
