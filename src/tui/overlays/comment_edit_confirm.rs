use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::tui::app::ActionState;

pub fn render_comment_edit_confirm_overlay(f: &mut Frame, app_action: &ActionState) {
    let ActionState::ConfirmingCommentEdit {
        issue_key,
        old_text,
        new_text,
        tab,
        ..
    } = app_action
    else {
        return;
    };

    let area = centered_rect(70, 75, f.area());
    f.render_widget(Clear, area);

    let hint = Line::from(vec![
        Span::raw("┤ "),
        Span::styled("↵", Style::default().fg(Color::Green)),
        Span::raw(" confirm  "),
        Span::styled("tab", Style::default().fg(Color::Blue)),
        Span::raw(" switch  "),
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

    let inner = block.inner(area);
    f.render_widget(block, area);

    match tab {
        0 => render_preview(f, inner, new_text),
        _ => render_diff(f, inner, old_text, new_text),
    }
}

fn render_preview(f: &mut Frame, area: Rect, new_text: &str) {
    f.render_widget(Paragraph::new(new_text).wrap(Wrap { trim: false }), area);
}

fn render_diff(f: &mut Frame, area: Rect, old_text: &str, new_text: &str) {
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
    f.render_widget(Paragraph::new(rendered).wrap(Wrap { trim: false }), area);
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
