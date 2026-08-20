use std::collections::HashMap;

use chrono::{DateTime, FixedOffset};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    symbols::border::{self, Set as BorderSet},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

/// Border set that shows only the four corners; lines are replaced with spaces.
const CORNERS_ONLY: BorderSet = BorderSet {
    top_left: "┌",
    top_right: "┐",
    bottom_left: "└",
    bottom_right: "┘",
    vertical_left: " ",
    vertical_right: " ",
    horizontal_top: " ",
    horizontal_bottom: " ",
};

use crate::config::types::{CustomViewConfig, CustomViewFieldConfig, FieldType};
use crate::datetime::{local_tz, parse_dt, parse_tz_offset};
use crate::items::WorkItem;
use crate::jira::types::Issue;
use crate::tui::app::{ActionState, AppState, DetailFocus};
use crate::tui::markdown::markdown_to_lines;
use crate::tui::render::RenderOut;
use crate::tui::theme;

// ── Segment model ─────────────────────────────────────────────────────────────

pub enum DetailNavKind {
    Comments,
    Attachments,
}

enum Segment {
    /// Plain read-only text lines — not focusable.
    ReadOnly { lines: Vec<Line<'static>> },
    /// A navigable widget (Comments or Attachments count summary).
    NavWidget { nav: DetailNavKind, content: String },
    /// A field with a bordered block. May be read-only or editable.
    EditableField {
        label: String,
        /// Text shown inside the block.
        content: String,
        /// Flat index among all editable fields. Stable per config (or iteration order for default).
        field_idx: usize,
        /// If true, Enter opens a browser link (if URL) but never opens editing.
        readonly: bool,
        /// If true, content is markdown and should be rendered with styling.
        is_markdown: bool,
    },
}

// ── Public helpers ─────────────────────────────────────────────────────────────

/// The item's body field — the long-form prose the default view shows above
/// everything else. Jira keeps it in the typed `description` field, a merge
/// request in `gl.description`. A Confluence task's body (`conf.task`) is
/// deliberately absent: it doubles as the task's title, so hoisting it would
/// print the same text twice.
const fn body_field_id(item: &WorkItem) -> Option<&'static str> {
    match item {
        WorkItem::Jira(_) => Some(crate::items::FIELD_DESCRIPTION),
        WorkItem::Gitlab(_) => Some(crate::gitlab::types::FIELD_DESCRIPTION),
        WorkItem::Confluence(_) => None,
    }
}

/// Whether a field cannot be edited *yet* because the item's full detail is
/// still on its way. A board-trimmed issue reports no description until the
/// background fetch lands, so editing it would open a blank buffer whose
/// contents replace the real text.
pub fn field_awaiting_detail(item: &WorkItem, field_id: &str) -> bool {
    field_id == crate::items::FIELD_DESCRIPTION && item.as_jira().is_some_and(|issue| issue.partial)
}

/// Fields the default view exposes, in focus order: the body field first, then
/// every entry in `fields_map()` sorted by id, minus the hoisted body field.
///
/// This is the single source of truth behind `num_view_fields`,
/// `view_field_cfg` and `build_default_segments` — the flat `field_idx` that
/// `DetailFocus::Field` carries indexes into exactly this list, so deriving it
/// three separate ways is how the focus ring and the rendered segments drift
/// apart.
///
/// The body field is listed even when it is empty, so an issue with no
/// description still offers a place to write the first one.
fn default_view_fields(item: &WorkItem) -> Vec<CustomViewFieldConfig> {
    let body = body_field_id(item);
    let mut fields: Vec<CustomViewFieldConfig> = body
        .map(|field_id| CustomViewFieldConfig {
            field_id: field_id.to_owned(),
            name: Some("Description".to_owned()),
            // Prose belongs in $EDITOR, never in the inline single-line editor.
            use_editor: Some(true),
            ..Default::default()
        })
        .into_iter()
        .collect();

    let mut extra: Vec<&String> = item
        .fields_map()
        .keys()
        .filter(|k| Some(k.as_str()) != body)
        .collect();
    extra.sort();
    fields.extend(extra.into_iter().map(|key| CustomViewFieldConfig {
        field_id: key.clone(),
        ..Default::default()
    }));
    fields
}

/// Number of focusable fields in the view.
/// For custom views: total configured fields. For the default view (cfg=None):
/// the body field plus all extra fields.
pub fn num_view_fields(cfg: Option<&CustomViewConfig>, item: Option<&WorkItem>) -> usize {
    cfg.map_or_else(
        || item.map_or(0, |i| default_view_fields(i).len()),
        |c| c.sections.iter().map(|s| s.fields.len()).sum(),
    )
}

/// Retrieve the field config at flat index `idx`.
/// For the default view (cfg=None), synthesizes a config from the item's field map.
pub fn view_field_cfg(
    cfg: Option<&CustomViewConfig>,
    item: Option<&WorkItem>,
    idx: usize,
) -> Option<CustomViewFieldConfig> {
    if let Some(cfg) = cfg {
        let mut count = 0;
        for section in &cfg.sections {
            for field in &section.fields {
                if count == idx {
                    return Some(field.clone());
                }
                count += 1;
            }
        }
        None
    } else if let Some(item) = item {
        default_view_fields(item).into_iter().nth(idx)
    } else {
        None
    }
}

/// Resolve the display label for a field, consulting API names, then the
/// builtin Confluence and GitLab field names, as fallbacks.
pub fn resolve_field_label(
    field: &CustomViewFieldConfig,
    field_names: &HashMap<String, String>,
) -> String {
    field
        .name
        .as_deref()
        .or_else(|| field_names.get(&field.field_id).map(String::as_str))
        .or_else(|| builtin_field_name(&field.field_id))
        .unwrap_or(&field.field_id)
        .to_string()
}

/// Builtin display name for a synthetic field id (`conf.*` / `gl.*`), which no
/// Jira editmeta describes.
fn builtin_field_name(field_id: &str) -> Option<&'static str> {
    crate::confluence::types::field_name(field_id)
        .or_else(|| crate::gitlab::types::field_name(field_id))
}

/// Public helper used by app.rs to get (`field_id`, current JSON value) for editing.
pub fn view_editable_field_spec(
    cfg: Option<&CustomViewConfig>,
    item: &WorkItem,
    idx: usize,
) -> (String, serde_json::Value) {
    let Some(field_cfg) = view_field_cfg(cfg, Some(item), idx) else {
        return (String::new(), serde_json::Value::Null);
    };
    let field_id = field_cfg.field_id;
    let value = item
        .field(&field_id)
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    (field_id, value)
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Render the detail view — either a configured custom view or the auto-generated default view.
/// `cfg = None` activates the default view (all item fields).
pub fn render_detail_view(
    f: &mut Frame,
    area: Rect,
    item: &WorkItem,
    app: &AppState,
    render_out: &mut RenderOut,
) -> usize {
    let cfg = current_view_config(app);
    let tz = resolve_tz(cfg);
    let w = area.width;

    let segments = build_segments(item, cfg, tz, w, &app.field_names);

    let scroll = app.detail_scroll;
    let viewport_h = area.height as usize;
    let mut virtual_y: usize = 0;

    // Ensure offsets vec is large enough: Comments(0), Attachments(1), Field(i)=2+i
    let num_fields = num_view_fields(cfg, Some(item));
    render_out
        .detail_focus_offsets
        .resize(2 + num_fields, (0, 0));

    for seg in &segments {
        let seg_height = measure_segment(seg, w);
        let seg_top = virtual_y;
        let seg_bot = virtual_y + seg_height;
        virtual_y += seg_height;

        // Always record positions (used by auto-scroll, even for off-screen items)
        match seg {
            Segment::NavWidget {
                nav: DetailNavKind::Comments,
                ..
            } => {
                render_out.detail_focus_offsets[0] = (seg_top, seg_bot);
            }
            Segment::NavWidget {
                nav: DetailNavKind::Attachments,
                ..
            } => {
                render_out.detail_focus_offsets[1] = (seg_top, seg_bot);
            }
            Segment::EditableField { field_idx, .. }
                if 2 + *field_idx < render_out.detail_focus_offsets.len() =>
            {
                render_out.detail_focus_offsets[2 + *field_idx] = (seg_top, seg_bot);
            }
            _ => {}
        }

        // Skip rendering if outside viewport
        if seg_bot <= scroll || seg_top >= scroll + viewport_h {
            continue;
        }

        // How many rows of this segment are clipped at the top
        let clipped_top = scroll.saturating_sub(seg_top);

        // Screen Y for first visible row of this segment
        #[allow(clippy::cast_possible_truncation)]
        let screen_y = area.y + seg_top.saturating_sub(scroll) as u16;

        // Available height on screen for this segment
        let avail_rows = seg_height.saturating_sub(clipped_top);
        let screen_y_rel = seg_top.saturating_sub(scroll);
        let avail_rows = avail_rows.min(viewport_h.saturating_sub(screen_y_rel));
        #[allow(clippy::cast_possible_truncation)]
        let avail_h = avail_rows as u16;

        if avail_h == 0 {
            continue;
        }

        let rect = Rect {
            x: area.x,
            y: screen_y,
            width: area.width,
            height: avail_h,
        };

        render_segment(f, rect, clipped_top, seg, app);
    }

    virtual_y
}

/// Extract the current view config from app state, or None for the default view.
pub fn current_view_config(app: &AppState) -> Option<&CustomViewConfig> {
    match &app.view_mode {
        crate::tui::app::ViewMode::Custom(id) => app.team_config().views.get(id.as_str()),
        _ => None,
    }
}

fn render_segment(f: &mut Frame, rect: Rect, clipped_top: usize, seg: &Segment, app: &AppState) {
    match seg {
        Segment::ReadOnly { lines } => {
            #[allow(clippy::cast_possible_truncation)]
            let scroll_y = clipped_top as u16;
            f.render_widget(
                Paragraph::new(lines.clone())
                    .wrap(Wrap { trim: false })
                    .scroll((scroll_y, 0)),
                rect,
            );
        }
        Segment::NavWidget { nav, content } => {
            let selected = match nav {
                DetailNavKind::Comments => {
                    matches!(app.detail_focus, DetailFocus::Comments)
                }
                DetailNavKind::Attachments => {
                    matches!(app.detail_focus, DetailFocus::Attachments)
                }
            };
            let border_style = if selected {
                Style::default().fg(theme::BORDER_FOCUS)
            } else {
                Style::default().fg(theme::MUTED)
            };
            let block = Block::default()
                .borders(Borders::ALL)
                .border_set(border::PLAIN)
                .border_style(border_style);
            let inner = block.inner(rect);
            f.render_widget(block, rect);
            if inner.height > 0 {
                let inner_scroll = u16::try_from(clipped_top)
                    .unwrap_or(u16::MAX)
                    .saturating_sub(1);
                f.render_widget(
                    Paragraph::new(content.as_str()).scroll((inner_scroll, 0)),
                    inner,
                );
            }
        }
        Segment::EditableField { .. } => {
            render_editable_field(f, rect, clipped_top, seg, app);
        }
    }
}

fn render_editable_field(
    f: &mut Frame,
    rect: Rect,
    clipped_top: usize,
    seg: &Segment,
    app: &AppState,
) {
    let Segment::EditableField {
        label,
        field_idx,
        content,
        readonly,
        is_markdown,
    } = seg
    else {
        return;
    };
    let selected = matches!(&app.detail_focus, DetailFocus::Field(fi) if *fi == *field_idx);
    let is_inline_edit = matches!(
        &app.action_state,
        ActionState::InlineEditingField { field_idx: fi, .. } if *fi == *field_idx
    );
    let border_style = if is_inline_edit {
        Style::default().fg(Color::Yellow)
    } else if selected && *readonly {
        Style::default()
    } else if selected {
        Style::default().fg(theme::BORDER_FOCUS)
    } else {
        Style::default().fg(theme::MUTED)
    };
    let title = format!(" {label} ");
    let block = if *readonly {
        Block::default()
            .title(title.as_str())
            .borders(Borders::ALL)
            .border_set(CORNERS_ONLY)
            .border_style(border_style)
    } else {
        Block::default()
            .title(title.as_str())
            .borders(Borders::ALL)
            .border_set(border::PLAIN)
            .border_style(border_style)
    };
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    // Scroll inner content: subtract 1 for top border (if clipped)
    #[allow(clippy::cast_possible_truncation)]
    let inner_scroll = (clipped_top as u16).saturating_sub(1);
    if inner.height > 0 {
        if is_inline_edit {
            if let ActionState::InlineEditingField {
                ref input, cursor, ..
            } = app.action_state
            {
                let line = inline_cursor_line(input, cursor);
                f.render_widget(Paragraph::new(line).scroll((inner_scroll, 0)), inner);
            }
        } else if *is_markdown {
            f.render_widget(
                Paragraph::new(markdown_to_lines(content))
                    .wrap(Wrap { trim: false })
                    .scroll((inner_scroll, 0)),
                inner,
            );
        } else {
            f.render_widget(
                Paragraph::new(content.as_str())
                    .wrap(Wrap { trim: false })
                    .scroll((inner_scroll, 0)),
                inner,
            );
        }
    }
}

// ── Segment builder ──────────────────────────────────────────────────────────

fn build_segments(
    item: &WorkItem,
    cfg: Option<&CustomViewConfig>,
    tz: FixedOffset,
    width: u16,
    field_names: &HashMap<String, String>,
) -> Vec<Segment> {
    let mut segs: Vec<Segment> = Vec::new();

    // Header (read-only) — expanded for default view
    segs.push(Segment::ReadOnly {
        lines: header_lines(item, cfg.is_none()),
    });

    // Nav widgets: Comments and Attachments (Jira items only)
    if let Some(issue) = item.as_jira() {
        let comment_count = issue.fields.comment.as_ref().map_or(0, |c| c.total);
        segs.push(Segment::NavWidget {
            nav: DetailNavKind::Comments,
            content: format!("Comments  ({comment_count})"),
        });
        let attachment_count = issue.fields.attachment.as_ref().map_or(0, Vec::len);
        segs.push(Segment::NavWidget {
            nav: DetailNavKind::Attachments,
            content: format!("Attachments  ({attachment_count})"),
        });

        // A partial (board-trimmed) issue has its full detail — description,
        // comments, custom fields — fetched in the background when opened.
        // Note it so the empty sections don't read as "nothing here".
        if issue.partial {
            segs.push(Segment::ReadOnly {
                lines: vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        "Loading full detail…",
                        Style::default().add_modifier(Modifier::DIM | Modifier::ITALIC),
                    )),
                ],
            });
        }
    }

    match cfg {
        Some(cfg) => {
            build_custom_segments(&mut segs, item, cfg, tz, width, field_names);
        }
        None => {
            build_default_segments(&mut segs, item, width, field_names);
        }
    }

    segs
}

fn build_custom_segments(
    segs: &mut Vec<Segment>,
    item: &WorkItem,
    cfg: &CustomViewConfig,
    tz: FixedOffset,
    width: u16,
    field_names: &HashMap<String, String>,
) {
    let mut field_flat_idx = 0usize;

    for (sec_idx, section) in cfg.sections.iter().enumerate() {
        // Section separator — blank line before all but the first
        let sep_lines = if sec_idx == 0 {
            vec![section_sep(&section.title, width), Line::from("")]
        } else {
            vec![
                Line::from(""),
                section_sep(&section.title, width),
                Line::from(""),
            ]
        };
        segs.push(Segment::ReadOnly { lines: sep_lines });

        // Section description (optional subtitle)
        if let Some(desc) = &section.description {
            segs.push(Segment::ReadOnly {
                lines: vec![Line::from(Span::styled(
                    desc.clone(),
                    Style::default().add_modifier(Modifier::DIM),
                ))],
            });
        }

        // Fields
        for field in &section.fields {
            let label = resolve_field_label(field, field_names);
            let content = get_field_content(item, field, tz);
            // Items without field editing render every field readonly.
            let readonly = field.readonly.unwrap_or(false) || !item.supports_field_edit();
            let is_markdown = item.field(&field.field_id).is_some_and(is_adf)
                || is_markdown_field(item, &field.field_id);
            segs.push(Segment::EditableField {
                label,
                content,
                field_idx: field_flat_idx,
                readonly,
                is_markdown,
            });
            field_flat_idx += 1;
        }

        // Duration row — inserted when section has both "start" and "end" fields
        let start_field = section
            .fields
            .iter()
            .find(|f| f.duration_role.as_deref() == Some("start"));
        let end_field = section
            .fields
            .iter()
            .find(|f| f.duration_role.as_deref() == Some("end"));

        if start_field.is_some() && end_field.is_some() {
            let start_dt =
                start_field.and_then(|f| parse_field_dt(item, Some(f.field_id.as_str())));
            let end_dt = end_field.and_then(|f| parse_field_dt(item, Some(f.field_id.as_str())));
            let jira_h = section
                .fields
                .iter()
                .find(|f| f.duration_role.as_deref() == Some("jira_value"))
                .and_then(|f| item.field(&f.field_id))
                .and_then(serde_json::Value::as_f64);
            segs.push(Segment::ReadOnly {
                lines: duration_lines(start_dt.as_ref(), end_dt.as_ref(), jira_h),
            });
        }
    }
}

/// A synthetic field whose value is markdown rather than ADF or a plain
/// scalar: the Confluence task body and a merge request's description.
fn is_markdown_field(item: &WorkItem, field_id: &str) -> bool {
    (field_id == crate::confluence::types::FIELD_TASK && item.as_confluence().is_some())
        || (field_id == crate::gitlab::types::FIELD_DESCRIPTION && item.as_gitlab().is_some())
}

fn build_default_segments(
    segs: &mut Vec<Segment>,
    item: &WorkItem,
    width: u16,
    field_names: &HashMap<String, String>,
) {
    // `default_view_fields` decides both the order and the flat `field_idx`
    // that `DetailFocus::Field` navigates, so the segments are built straight
    // from it rather than re-deriving the same list here.
    let fields = default_view_fields(item);
    let has_body = body_field_id(item).is_some();
    let editable = item.supports_field_edit();

    for (field_idx, field) in fields.iter().enumerate() {
        let is_body = has_body && field_idx == 0;

        if is_body {
            // No section separator: the block's own border is titled
            // "Description" already, so a header above it says it twice. Just
            // part it from the item header.
            segs.push(Segment::ReadOnly {
                lines: vec![Line::from("")],
            });
        } else if field_idx == usize::from(has_body) {
            // The extras do share one header, since their blocks are titled
            // with individual field names.
            segs.push(Segment::ReadOnly {
                lines: vec![Line::from(""), section_sep("Fields", width), Line::from("")],
            });
        }

        let value = item.field(&field.field_id);
        let content = value
            .filter(|v| !v.is_null())
            .map(val_to_str)
            .unwrap_or_default();
        segs.push(Segment::EditableField {
            label: resolve_field_label(field, field_names),
            content,
            field_idx,
            readonly: !editable || field_awaiting_detail(item, &field.field_id),
            // A Jira description is ADF and a merge request's is markdown, so
            // the body always renders styled — including while it is empty,
            // where there is no value to sniff.
            is_markdown: is_body
                || value.is_some_and(is_adf)
                || is_markdown_field(item, &field.field_id),
        });
    }
}

fn get_field_content(item: &WorkItem, field: &CustomViewFieldConfig, tz: FixedOffset) -> String {
    let Some(raw) = item.field(&field.field_id) else {
        return String::new();
    };
    if raw.is_null() {
        return String::new();
    }
    // Bare `yyyy-MM-dd` values fail parse_dt and fall through to val_to_str,
    // which already displays them as-is.
    if let Some(kind) = field.effective_type()
        && let Some(s) = raw.as_str()
        && let Some(dt) = parse_dt(s)
    {
        return match kind {
            FieldType::Date => dt.with_timezone(&tz).format("%Y-%m-%d").to_string(),
            FieldType::DateTime => fmt_dt(&dt, tz),
        };
    }
    val_to_str(raw)
}

// ── Segment measurement ──────────────────────────────────────────────────────

fn measure_segment(seg: &Segment, width: u16) -> usize {
    if width == 0 {
        return 1;
    }
    match seg {
        Segment::ReadOnly { lines } => lines
            .iter()
            .map(|l| measure_line(l, width))
            .sum::<usize>()
            .max(1),
        Segment::NavWidget { content, .. } => {
            // Single-line content inside a full-border block → always 3 rows
            let _ = content; // content fits on one line
            3
        }
        Segment::EditableField { content, .. } => {
            let inner_w = (width as usize).saturating_sub(2).max(1);
            let content_h = if content.is_empty() {
                1
            } else {
                content
                    .lines()
                    .map(|l| {
                        let chars = l.chars().count();
                        if chars == 0 {
                            1
                        } else {
                            chars.div_ceil(inner_w)
                        }
                    })
                    .sum::<usize>()
                    .max(1)
            };
            2 + content_h // top border + content rows + bottom border
        }
    }
}

fn measure_line(line: &Line, width: u16) -> usize {
    let text_w: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
    if text_w == 0 {
        1 // empty line still takes 1 row
    } else {
        text_w.div_ceil(width as usize).max(1)
    }
}

// ── Header section ────────────────────────────────────────────────────────────

fn header_lines(item: &WorkItem, full: bool) -> Vec<Line<'static>> {
    match item {
        WorkItem::Jira(issue) => jira_header_lines(issue, full),
        WorkItem::Confluence(task) => confluence_header_lines(task, full),
        WorkItem::Gitlab(mr) => gitlab_header_lines(mr, full),
    }
}

fn gitlab_header_lines(mr: &crate::gitlab::types::MergeRequest, full: bool) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = vec![Line::from(vec![
        Span::raw(format!("{} {}", mr.short_ref(), mr.title)),
        Span::raw("  "),
        Span::styled(
            mr.status_label.clone(),
            Style::default().add_modifier(Modifier::DIM),
        ),
    ])];

    if full {
        if let Some(project) = &mr.project_path {
            lines.push(kv_line("Project", project));
        }
        if let Some(author) = &mr.author {
            lines.push(kv_line("Author", author));
        }
        if !mr.reviewers.is_empty() {
            lines.push(kv_line("Reviewers", &mr.reviewers.join(", ")));
        }
        if let (Some(source), Some(target)) = (&mr.source_branch, &mr.target_branch) {
            lines.push(kv_line("Branches", &format!("{source} → {target}")));
        }
        lines.push(kv_line("Approvals", &mr.approvals_summary()));
        lines.push(kv_line("CI", mr.ci_status.as_deref().unwrap_or("—")));
        lines.push(kv_line("Key", &mr.key));
    }

    lines.push(Line::from(""));
    lines
}

fn confluence_header_lines(
    task: &crate::confluence::types::Task,
    full: bool,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = vec![Line::from(vec![
        Span::raw(task.title.clone()),
        Span::raw("  "),
        Span::styled(
            match task.status {
                crate::confluence::types::TaskStatus::Incomplete => "To do",
                crate::confluence::types::TaskStatus::Complete => "Done",
            },
            Style::default().add_modifier(Modifier::DIM),
        ),
    ])];

    if full {
        if let Some(page) = &task.page_title {
            lines.push(kv_line("Page", page));
        }
        if let Some(due) = task.due_at {
            lines.push(kv_line("Due", &due.format("%Y-%m-%d").to_string()));
        }
        lines.push(kv_line("Key", &task.key));
    }

    lines.push(Line::from(""));
    lines
}

fn jira_header_lines(issue: &Issue, full: bool) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(vec![
        Span::raw(issue.fields.summary.clone()),
        Span::raw("  "),
        Span::styled(
            issue.fields.status.name.clone(),
            Style::default().add_modifier(Modifier::DIM),
        ),
    ]));

    if full {
        let priority = issue
            .fields
            .priority
            .as_ref()
            .map_or_else(|| "—".to_string(), |p| format!("{} {}", p.symbol(), p.name));
        lines.push(kv_line("Priority", &priority));

        let assignee = issue
            .fields
            .assignee
            .as_ref()
            .map_or_else(|| "Unassigned".to_string(), |a| a.display().to_string());
        lines.push(kv_line("Assignee", &assignee));

        if let Some(ref reporter) = issue.fields.reporter {
            lines.push(kv_line("Reporter", reporter.display()));
        }

        lines.push(kv_line("Type", &issue.fields.issuetype.name));
        lines.push(kv_line(
            "Project",
            &format!(
                "{} ({})",
                issue.fields.project.name, issue.fields.project.key
            ),
        ));
        lines.push(kv_line("Key", &issue.key));
    }

    lines.push(Line::from(""));
    lines
}

// ── Duration row (read-only, computed from start + end) ──────────────────────

fn duration_lines(
    start_dt: Option<&DateTime<FixedOffset>>,
    end_dt: Option<&DateTime<FixedOffset>>,
    jira_h: Option<f64>,
) -> Vec<Line<'static>> {
    const DUR_PAD: usize = 28;
    let mut lines: Vec<Line> = Vec::new();

    match (start_dt, end_dt) {
        (Some(s), Some(m)) => {
            let our_mins = (m.timestamp() - s.timestamp()) / 60;
            let our_str = fmt_duration(our_mins);
            match jira_h {
                Some(jh) => {
                    #[allow(clippy::cast_possible_truncation)]
                    let jira_mins = (jh * 60.0).round() as i64;
                    let mismatch = (our_mins - jira_mins).abs() > 5;
                    let jira_label = format!("Jira: {jh:.1}h");
                    let (check_str, check_style) = if mismatch {
                        (
                            format!("{jira_label} ⚠"),
                            Style::default().fg(Color::Yellow),
                        )
                    } else {
                        (
                            format!("{jira_label} ✓"),
                            Style::default().add_modifier(Modifier::DIM),
                        )
                    };
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!("{:<14}", "Duration"),
                            Style::default().add_modifier(Modifier::DIM),
                        ),
                        Span::raw(format!("{our_str:<DUR_PAD$}")),
                        Span::styled(check_str, check_style),
                    ]));
                }
                None => lines.push(kv_line("Duration", &our_str)),
            }
        }
        _ => lines.push(Line::from(vec![
            Span::styled(
                format!("{:<14}", "Duration"),
                Style::default().add_modifier(Modifier::DIM),
            ),
            Span::styled("(incomplete)", Style::default().add_modifier(Modifier::DIM)),
        ])),
    }

    lines.push(Line::from(""));
    lines
}

// ── Section separator ────────────────────────────────────────────────────────

fn section_sep(label: &str, width: u16) -> Line<'static> {
    let labeled = format!("── {label} ");
    let fill_len = (width as usize).saturating_sub(labeled.chars().count());
    let fill = "─".repeat(fill_len);
    Line::from(Span::styled(
        format!("{labeled}{fill}"),
        Style::default().add_modifier(Modifier::DIM),
    ))
}

// ── Field helpers ────────────────────────────────────────────────────────────

fn kv_line(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label:<14}"),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Span::raw(value.to_string()),
    ])
}

// ── Data extraction ──────────────────────────────────────────────────────────

fn parse_field_dt(item: &WorkItem, field_id: Option<&str>) -> Option<DateTime<FixedOffset>> {
    let fid = field_id?;
    let v = item.field(fid)?;
    if v.is_null() {
        return None;
    }
    v.as_str().and_then(parse_dt)
}

/// Check if a JSON value is an ADF document (whose text representation is markdown).
fn is_adf(v: &serde_json::Value) -> bool {
    v.get("type").and_then(|t| t.as_str()) == Some("doc")
}

pub fn val_to_str(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.replace('\r', ""),
        serde_json::Value::Object(_) => {
            // Detect ADF documents and convert to markdown
            if v.get("type").and_then(|t| t.as_str()) == Some("doc") {
                return json_to_text(v).replace('\r', "");
            }
            ["value", "name", "displayName"]
                .iter()
                .find_map(|k| {
                    v.get(k)
                        .and_then(|x| x.as_str())
                        .map(|s| s.replace('\r', ""))
                })
                .unwrap_or_else(|| v.to_string())
        }
        serde_json::Value::Array(a) => a
            .iter()
            .map(|item| {
                item.as_str()
                    .or_else(|| item.get("name").and_then(|n| n.as_str()))
                    .or_else(|| item.get("value").and_then(|n| n.as_str()))
                    .unwrap_or("?")
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join(", "),
        _ => v.to_string(),
    }
}

// ── Timezone ─────────────────────────────────────────────────────────────────

pub fn resolve_tz(cfg: Option<&CustomViewConfig>) -> FixedOffset {
    cfg.and_then(|c| c.timezone.as_deref())
        .and_then(parse_tz_offset)
        .unwrap_or_else(local_tz)
}

// ── Formatting ───────────────────────────────────────────────────────────────

fn fmt_dt(dt: &DateTime<FixedOffset>, tz: FixedOffset) -> String {
    dt.with_timezone(&tz).format("%Y-%m-%d  %H:%M").to_string()
}

fn fmt_duration(total_mins: i64) -> String {
    let mins = total_mins.abs();
    let h = mins / 60;
    let m = mins % 60;
    if h == 0 {
        format!("{m}m")
    } else if m == 0 {
        format!("{h}h")
    } else {
        format!("{h}h {m}m")
    }
}

// ── Inline editing ────────────────────────────────────────────────────────────

/// Build a `Line` with a block cursor at `cursor_char` position.
fn inline_cursor_line(input: &str, cursor_char: usize) -> Line<'static> {
    let chars: Vec<char> = input.chars().collect();
    let mut spans: Vec<Span<'static>> = Vec::new();

    if cursor_char < chars.len() {
        let before: String = chars[..cursor_char].iter().collect();
        let at: String = chars[cursor_char..=cursor_char].iter().collect();
        let after: String = chars[cursor_char + 1..].iter().collect();
        if !before.is_empty() {
            spans.push(Span::raw(before));
        }
        spans.push(Span::styled(
            at,
            Style::default().add_modifier(Modifier::REVERSED),
        ));
        if !after.is_empty() {
            spans.push(Span::raw(after));
        }
    } else {
        if !input.is_empty() {
            spans.push(Span::raw(input.to_owned()));
        }
        spans.push(Span::styled(
            " ",
            Style::default().add_modifier(Modifier::REVERSED),
        ));
    }

    Line::from(spans)
}

// Re-export for local use.
pub use crate::jira::adf::json_to_text;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A Jira item whose `extra` map carries `keys`, with the typed
    /// description set from `description`.
    fn jira_item(description: Option<serde_json::Value>, keys: &[&str]) -> WorkItem {
        let mut issue: Issue = serde_json::from_value(json!({
            "id": "1",
            "key": "PROJ-1",
            "fields": {
                "summary": "Summary",
                "status": { "id": "s1", "name": "Open" },
                "issuetype": { "id": "t1", "name": "Task", "subtask": false },
                "project": { "id": "p1", "key": "PROJ", "name": "Project" },
            },
        }))
        .expect("issue fixture parses");
        issue.fields.description = description;
        for key in keys {
            issue.fields.extra.insert((*key).to_owned(), json!("value"));
        }
        WorkItem::Jira(issue)
    }

    fn adf(text: &str) -> serde_json::Value {
        json!({
            "type": "doc",
            "version": 1,
            "content": [{
                "type": "paragraph",
                "content": [{ "type": "text", "text": text }],
            }],
        })
    }

    fn gitlab_item() -> WorkItem {
        let api = serde_json::from_value(json!({
            "iid": 1,
            "project_id": 2,
            "title": "Add a thing",
            "description": "MR body",
            "state": "opened",
            "web_url": "https://gitlab.example/x/y/-/merge_requests/1",
        }))
        .expect("merge request fixture parses");
        WorkItem::Gitlab(crate::gitlab::types::to_display(api))
    }

    fn confluence_item() -> WorkItem {
        let mut task: crate::confluence::types::Task = serde_json::from_value(json!({
            "id": "9",
            "key": "CONF:9",
            "status": "incomplete",
            "title": "Do the thing",
            "extra": {},
        }))
        .expect("task fixture parses");
        task.extra.insert(
            crate::confluence::types::FIELD_TASK.to_owned(),
            json!("body"),
        );
        task.extra.insert(
            crate::confluence::types::FIELD_PAGE.to_owned(),
            json!("Page"),
        );
        WorkItem::Confluence(task)
    }

    fn field_ids(item: &WorkItem) -> Vec<String> {
        default_view_fields(item)
            .into_iter()
            .map(|f| f.field_id)
            .collect()
    }

    #[test]
    fn description_leads_the_default_view_and_extras_stay_sorted() {
        let item = jira_item(Some(adf("hello")), &["customfield_2", "customfield_1"]);
        assert_eq!(
            field_ids(&item),
            ["description", "customfield_1", "customfield_2"]
        );
    }

    /// The empty case is the whole point of showing it unconditionally: without
    /// a slot to focus there is no way to write a first description.
    #[test]
    fn description_is_listed_even_when_the_issue_has_none() {
        let item = jira_item(None, &[]);
        assert_eq!(field_ids(&item), ["description"]);
    }

    /// `num_view_fields` bounds focus navigation while `view_field_cfg`
    /// resolves what Enter acts on. They index the same list or Enter edits the
    /// wrong field.
    #[test]
    fn focus_indices_agree_with_the_resolved_field_configs() {
        let item = jira_item(Some(adf("hello")), &["customfield_1"]);
        assert_eq!(num_view_fields(None, Some(&item)), 2);

        let first = view_field_cfg(None, Some(&item), 0).expect("index 0 resolves");
        assert_eq!(first.field_id, "description");
        assert_eq!(first.use_editor, Some(true));
        assert_eq!(first.name.as_deref(), Some("Description"));

        let second = view_field_cfg(None, Some(&item), 1).expect("index 1 resolves");
        assert_eq!(second.field_id, "customfield_1");

        assert!(view_field_cfg(None, Some(&item), 2).is_none());
    }

    #[test]
    fn editable_field_spec_hands_the_description_adf_to_the_editor() {
        let item = jira_item(Some(adf("hello")), &[]);
        let (field_id, value) = view_editable_field_spec(None, &item, 0);
        assert_eq!(field_id, "description");
        assert_eq!(value, adf("hello"));
    }

    /// The hazard the gate exists for: a board-trimmed issue has no description
    /// yet, so it must not be editable — a blank $EDITOR buffer would be
    /// committed over the real text.
    #[test]
    fn a_partial_issue_will_not_let_its_description_be_edited() {
        let mut item = jira_item(None, &[]);
        assert!(!field_awaiting_detail(&item, "description"));

        item.as_jira_mut().expect("jira item").partial = true;
        assert!(field_awaiting_detail(&item, "description"));
        assert!(
            !field_awaiting_detail(&item, "customfield_1"),
            "only the lazily-fetched description is gated"
        );
    }

    /// A merge request keeps its description in `extra`, so hoisting it has to
    /// remove it from the sorted tail — otherwise it renders twice, which is
    /// what happened before the body field was introduced.
    #[test]
    fn a_merge_request_description_is_hoisted_and_not_duplicated() {
        let item = gitlab_item();
        let ids = field_ids(&item);
        assert_eq!(ids.first().map(String::as_str), Some("gl.description"));
        assert_eq!(
            ids.iter().filter(|id| *id == "gl.description").count(),
            1,
            "the hoisted body must not also appear among the extras"
        );
    }

    /// A Confluence task's body doubles as its title, so it stays where it was:
    /// in sorted position among the extras, with nothing hoisted above it.
    #[test]
    fn a_confluence_task_keeps_its_fields_in_sorted_order() {
        let item = confluence_item();
        assert_eq!(field_ids(&item), ["conf.page", "conf.task"]);
    }
}
