use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
    },
};
use ratatui_image::StatefulImage;

use std::fmt::Write as _;

use crate::jira::adf::json_to_text;
use crate::jira::types::{Attachment, Comment, Issue};
use crate::tui::markdown::markdown_to_lines;
use crate::tui::app::{ActionState, AppState, SubView};
use crate::tui::render::RenderOut;

pub fn render_sub_view_overlay(f: &mut Frame, app: &AppState, render_out: &mut RenderOut) {
    let Some(sub_view) = &app.overlay else {
        return;
    };
    let Some(issue) = app.selected_issue() else {
        return;
    };

    let area = centered_rect(80, 85, f.area());
    f.render_widget(Clear, area);

    let label = match sub_view {
        SubView::Comments => "Comments",
        SubView::Attachments => "Attachments",
    };
    let title = format!(" {} — {label} ", issue.key);

    let has_items = match sub_view {
        SubView::Comments => issue
            .fields
            .comment
            .as_ref()
            .is_some_and(|c| !c.comments.is_empty()),
        SubView::Attachments => issue
            .fields
            .attachment
            .as_deref()
            .is_some_and(|a| !a.is_empty()),
    };
    // Returns spans for a "(k)ey" style hint.
    // Active: only the letter is coloured; inactive: the whole hint is DarkGray.
    let key_hint = |key: &'static str, rest: &'static str, active: bool| -> Vec<Span<'static>> {
        if active {
            vec![
                Span::raw("("),
                Span::styled(key, Style::default().fg(Color::Blue)),
                Span::raw(format!("){rest}")),
            ]
        } else {
            vec![Span::styled(
                format!("({key}){rest}"),
                Style::default().fg(Color::DarkGray),
            )]
        }
    };

    let back_hint = if matches!(sub_view, SubView::Comments) {
        let mut spans = vec![Span::raw("┤ ")];
        spans.extend(key_hint("n", "ew", true));
        spans.push(Span::raw(" | "));
        spans.extend(key_hint("e", "dit", has_items));
        spans.push(Span::raw(" | "));
        spans.extend(key_hint("d", "el", has_items));
        spans.push(Span::raw(" | "));
        spans.push(Span::styled("q", Style::default().fg(Color::Magenta)));
        spans.push(Span::raw(" ├──"));
        Line::from(spans).alignment(Alignment::Right)
    } else {
        let nav_color = if has_items {
            Color::Green
        } else {
            Color::DarkGray
        };
        let mut spans = vec![Span::raw("┤ ")];
        spans.extend(key_hint("n", "ew", true));
        spans.push(Span::raw(" | "));
        spans.extend(key_hint("d", "el", has_items));
        spans.push(Span::raw(" | "));
        spans.push(Span::styled(
            "↕",
            Style::default().fg(if has_items {
                Color::Blue
            } else {
                Color::DarkGray
            }),
        ));
        spans.push(Span::styled("→", Style::default().fg(nav_color)));
        spans.push(Span::raw(" | "));
        spans.push(Span::styled("q", Style::default().fg(Color::Magenta)));
        spans.push(Span::raw(" ├──"));
        Line::from(spans).alignment(Alignment::Right)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default())
        .title(title.as_str())
        .title_bottom(back_hint);

    let inner = block.inner(area);
    f.render_widget(block, area);

    let viewport_h = inner.height as usize;
    render_out.overlay_viewport_h = viewport_h;

    match sub_view {
        SubView::Comments => {
            render_comments(f, inner, app, issue, viewport_h, render_out);
        }
        SubView::Attachments => {
            render_attachments(f, inner, app, issue, render_out);
        }
    }
}

fn render_attachments(
    f: &mut Frame,
    inner: Rect,
    app: &AppState,
    issue: &Issue,
    render_out: &mut RenderOut,
) {
    // When user is typing a path, carve out an input box (and optional completions) at the bottom.
    let content_rect = if let ActionState::TypingAttachmentPath {
        ref path,
        ref completions,
        completion_idx,
        ..
    } = app.action_state
    {
        render_attachment_input_overlay(f, inner, path, completions, completion_idx)
    } else {
        inner
    };

    let panels = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(content_rect);
    let left_rect = panels[0];
    let right_rect = panels[1];

    let left_block = Block::default().borders(Borders::RIGHT);
    let left_inner = left_block.inner(left_rect);
    f.render_widget(left_block, left_rect);

    let left_h = left_inner.height as usize;
    render_out.overlay_viewport_h = left_h;

    let attachments = issue.fields.attachment.as_deref().unwrap_or(&[]);

    if attachments.is_empty() {
        render_out.overlay_content_h = 1;
        let line = Line::from(Span::styled(
            "(no attachments)",
            Style::default().add_modifier(Modifier::DIM),
        ));
        f.render_widget(Paragraph::new(vec![line]), left_inner);
    } else {
        let count = attachments.len();
        render_out.overlay_content_h = count;

        let scroll = app.overlay_scroll;
        let focused = app.overlay_focused_attachment;

        // Left panel: filename list
        let list_lines: Vec<Line> = attachments
            .iter()
            .enumerate()
            .skip(scroll)
            .take(left_h)
            .map(|(i, att)| {
                if i == focused {
                    Line::from(Span::styled(
                        att.filename.as_str(),
                        Style::default().add_modifier(Modifier::REVERSED),
                    ))
                } else {
                    Line::from(att.filename.as_str())
                }
            })
            .collect();
        f.render_widget(Paragraph::new(list_lines), left_inner);

        // Right panel: preview or metadata for focused attachment
        if let Some(att) = attachments.get(focused) {
            let att_id = &att.id;
            let meta_footer = build_meta_footer(att);

            let has_preview = app.attachment_images.contains_key(att_id)
                || app.attachment_text_previews.contains_key(att_id);

            // Split right panel into preview area + 1-line meta footer when showing a preview
            let (preview_rect, footer_rect) = if has_preview && right_rect.height > 1 {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(0), Constraint::Length(1)])
                    .split(right_rect);
                (chunks[0], Some(chunks[1]))
            } else {
                (right_rect, None)
            };

            if let Some(footer) = footer_rect {
                f.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        meta_footer,
                        Style::default().add_modifier(Modifier::DIM),
                    ))),
                    footer,
                );
            }

            // Priority 1: inline image
            if let Some(rc_protocol) = app.attachment_images.get(att_id) {
                let mut protocol = rc_protocol.borrow_mut();
                let img_widget = StatefulImage::<ratatui_image::protocol::StatefulProtocol>::new();
                f.render_stateful_widget(img_widget, preview_rect, &mut *protocol);
            }
            // Priority 2: text preview
            else if let Some(text) = app.attachment_text_previews.get(att_id) {
                f.render_widget(
                    Paragraph::new(text.as_str()).wrap(Wrap { trim: false }),
                    preview_rect,
                );
            }
            // Priority 3: fetching in progress
            else if app.attachment_fetching_id.as_deref() == Some(att_id.as_str()) {
                let line = Line::from(Span::styled(
                    "fetching…",
                    Style::default().add_modifier(Modifier::DIM),
                ));
                f.render_widget(Paragraph::new(vec![line]), preview_rect);
            }
            // Priority 4: metadata (fallback)
            else {
                render_attachment_detail(f, att, right_rect);
            }
        }
    }
}

fn render_attachment_input_overlay(
    f: &mut Frame,
    inner: Rect,
    path: &str,
    completions: &[String],
    completion_idx: Option<usize>,
) -> Rect {
    let visible_n = completions.len().min(8);

    let (content_rect, comp_rect, input_rect) = if completions.is_empty() {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(3)])
            .split(inner);
        (rows[0], None, rows[1])
    } else {
        let comp_height = u16::try_from(visible_n).unwrap_or(8) + 2;
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(0),
                Constraint::Length(comp_height),
                Constraint::Length(3),
            ])
            .split(inner);
        (rows[0], Some(rows[1]), rows[2])
    };

    // Render input box
    let input_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Blue))
        .title(" Upload file ");
    let input_inner = input_block.inner(input_rect);
    f.render_widget(input_block, input_rect);
    let display = format!("{path}\u{2588}");
    f.render_widget(Paragraph::new(display.as_str()), input_inner);

    // Render completions list
    if let Some(comp_area) = comp_rect {
        let comp_block = Block::default().borders(Borders::ALL);
        let comp_inner = comp_block.inner(comp_area);
        f.render_widget(comp_block, comp_area);

        // Scroll so that the selected item is visible (keep at bottom of window)
        let scroll =
            completion_idx.map_or(0, |idx| idx.saturating_sub(visible_n.saturating_sub(1)));

        let lines: Vec<Line> = completions
            .iter()
            .enumerate()
            .skip(scroll)
            .take(visible_n)
            .map(|(i, comp)| {
                let is_dir = comp.ends_with('/');
                let style = if Some(i) == completion_idx {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else if is_dir {
                    Style::default().add_modifier(Modifier::DIM)
                } else {
                    Style::default()
                };
                Line::from(Span::styled(comp.as_str(), style))
            })
            .collect();
        f.render_widget(Paragraph::new(lines), comp_inner);
    }

    content_rect
}

fn build_meta_footer(att: &Attachment) -> String {
    let author = att.author.display().to_string();
    let created_date = att.created.get(..10).unwrap_or(&att.created);
    let created_time = att.created.get(11..16).unwrap_or("");
    let date = if created_time.is_empty() {
        created_date.to_string()
    } else {
        format!("{created_date} {created_time}")
    };
    let mut parts = format!("{author} · {date}");
    if let Some(size) = att.size {
        let _ = write!(parts, " · {}", format_size(size));
    }
    parts
}

fn render_attachment_detail(f: &mut Frame, att: &Attachment, right_rect: Rect) {
    let author = att.author.display();
    let created_date = att.created.get(..10).unwrap_or(&att.created);
    let created_time = att.created.get(11..16).unwrap_or("");
    let created = if created_time.is_empty() {
        created_date.to_string()
    } else {
        format!("{created_date} {created_time}")
    };

    let label_style = Style::default().add_modifier(Modifier::BOLD);
    let mut detail: Vec<Line> = vec![
        Line::from(vec![
            Span::styled("Filename:  ", label_style),
            Span::raw(att.filename.clone()),
        ]),
        Line::from(vec![
            Span::styled("Author:    ", label_style),
            Span::raw(author),
        ]),
        Line::from(vec![
            Span::styled("Date:      ", label_style),
            Span::raw(created),
        ]),
    ];
    if let Some(size) = att.size {
        detail.push(Line::from(vec![
            Span::styled("Size:      ", label_style),
            Span::raw(format_size(size)),
        ]));
    }
    if let Some(mime) = &att.mime_type {
        detail.push(Line::from(vec![
            Span::styled("Type:      ", label_style),
            Span::raw(mime.clone()),
        ]));
    }
    if att.content.is_none() {
        detail.push(Line::from(Span::styled(
            "(no download URL)",
            Style::default().add_modifier(Modifier::DIM),
        )));
    }
    f.render_widget(
        Paragraph::new(detail).wrap(Wrap { trim: false }),
        right_rect,
    );
}

/// Height of a comment widget: top border (1) + header (1) + body lines + bottom border (1).
fn measure_comment_block(comment: &Comment, width: u16) -> usize {
    let usable = if width > 2 { (width - 2) as usize } else { 1 };
    let text = json_to_text(&comment.body);
    let body_lines: usize = text
        .lines()
        .map(|line| {
            let chars = line.chars().count();
            if chars == 0 {
                1
            } else {
                chars.div_ceil(usable)
            }
        })
        .sum();
    let body_rows = body_lines.max(1);
    // border-top with title (1) + body + border-bottom (1)
    2 + body_rows
}

fn render_comments(
    f: &mut Frame,
    inner: Rect,
    app: &AppState,
    issue: &crate::jira::types::Issue,
    viewport_h: usize,
    render_out: &mut RenderOut,
) {
    let Some(list) = &issue.fields.comment else {
        let line = Line::from(Span::styled(
            "(no comments)",
            Style::default().add_modifier(Modifier::DIM),
        ));
        render_out.overlay_content_h = 1;
        f.render_widget(Paragraph::new(vec![line]), inner);
        return;
    };

    if list.comments.is_empty() {
        let line = Line::from(vec![
            Span::styled(
                "No comments. Press ",
                Style::default().add_modifier(Modifier::DIM),
            ),
            Span::styled("n", Style::default().fg(Color::Blue)),
            Span::styled(
                " to compose the first one",
                Style::default().add_modifier(Modifier::DIM),
            ),
        ])
        .alignment(Alignment::Center);
        render_out.overlay_content_h = 1;
        f.render_widget(Paragraph::new(vec![line]), inner);
        return;
    }

    // Build virtual layout
    let mut offsets: Vec<(usize, usize)> = Vec::with_capacity(list.comments.len());
    let mut y = 0usize;
    for comment in &list.comments {
        let h = measure_comment_block(comment, inner.width);
        offsets.push((y, y + h));
        y += h;
    }
    // +1 for the "press n" hint line at the bottom
    let hint_h = 1usize;
    let content_h = y + hint_h;
    render_out.overlay_content_h = content_h;
    render_out.overlay_comment_offsets.clone_from(&offsets);

    let scroll = app.overlay_scroll;

    // Render each visible comment widget
    let area_top = inner.top();
    for (idx, comment) in list.comments.iter().enumerate() {
        let (top, bottom) = offsets[idx];
        // Skip if completely scrolled past
        if bottom <= scroll || top >= scroll + viewport_h {
            continue;
        }

        let widget_h = bottom - top;
        // Where on screen does this widget start?
        let screen_top = if top >= scroll {
            area_top + u16::try_from(top - scroll).unwrap_or(u16::MAX)
        } else {
            area_top
        };
        // Clip to viewport
        let visible_h = {
            let end_screen = area_top + u16::try_from(viewport_h).unwrap_or(u16::MAX);
            let widget_screen_bottom = screen_top + u16::try_from(widget_h).unwrap_or(u16::MAX);
            widget_screen_bottom.min(end_screen) - screen_top
        };

        let widget_area = Rect {
            x: inner.x,
            y: screen_top,
            width: inner.width,
            height: visible_h,
        };

        let focused = idx == app.overlay_focused_comment;
        render_comment_widget(f, widget_area, comment, focused);
    }

    // Render "press n" hint at the bottom of the virtual list
    let hint_top = y; // virtual position of the hint line
    if hint_top < scroll + viewport_h && hint_top + hint_h > scroll {
        let screen_y =
            area_top + u16::try_from(hint_top.saturating_sub(scroll)).unwrap_or(u16::MAX);
        let hint_area = Rect {
            x: inner.x,
            y: screen_y,
            width: inner.width,
            height: 1,
        };
        let hint = Line::from(vec![
            Span::styled("n", Style::default().fg(Color::Blue)),
            Span::styled(
                " — compose a new comment",
                Style::default().add_modifier(Modifier::DIM),
            ),
        ])
        .alignment(Alignment::Center);
        f.render_widget(Paragraph::new(vec![hint]), hint_area);
    }

    if content_h > viewport_h {
        // Re-derive `area` from inner + border offset
        let outer = Rect {
            x: inner.x.saturating_sub(1),
            y: inner.y.saturating_sub(1),
            width: inner.width + 2,
            height: inner.height + 2,
        };
        render_scrollbar(f, outer, content_h, viewport_h, scroll);
    }
}

fn render_comment_widget(f: &mut Frame, area: Rect, comment: &Comment, focused: bool) {
    if area.height == 0 {
        return;
    }

    let border_style = if focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let author = comment.author.display();
    let created_date = comment.created.get(..10).unwrap_or(&comment.created);
    let created_time = comment.created.get(11..16).unwrap_or("");
    let created = if created_time.is_empty() {
        created_date.to_string()
    } else {
        format!("{created_date} {created_time}")
    };
    let modified = if comment.created == comment.updated {
        ""
    } else {
        " [edited]"
    };
    let header = format!("{author} · {created}{modified}");

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Span::styled(
            format!(" {header} "),
            Style::default().add_modifier(Modifier::BOLD),
        ));

    let block_inner = block.inner(area);
    f.render_widget(block, area);

    if block_inner.height == 0 {
        return;
    }

    let body_text = json_to_text(&comment.body);
    let styled_lines = markdown_to_lines(&body_text);
    f.render_widget(
        Paragraph::new(styled_lines).wrap(Wrap { trim: false }),
        block_inner,
    );
}

fn render_scrollbar(f: &mut Frame, area: Rect, content_h: usize, viewport_h: usize, scroll: usize) {
    let mut scrollbar_state = ScrollbarState::new(content_h)
        .viewport_content_length(viewport_h)
        .position(scroll);
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(Some("┐"))
        .end_symbol(Some("┘"))
        .track_symbol(Some("│"))
        .track_style(Style::default())
        .thumb_style(Style::default().fg(Color::Yellow));
    f.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
}

fn format_size(bytes: u64) -> String {
    if bytes >= 1_048_576 {
        let mb = bytes / 1_048_576;
        let tenth = (bytes % 1_048_576) * 10 / 1_048_576;
        format!("{mb}.{tenth} MB")
    } else if bytes >= 1024 {
        let kb = bytes / 1024;
        let tenth = (bytes % 1024) * 10 / 1024;
        format!("{kb}.{tenth} KB")
    } else {
        format!("{bytes} B")
    }
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
