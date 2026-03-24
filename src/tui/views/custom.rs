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

use crate::config::types::{CustomViewConfig, CustomViewFieldConfig};
use crate::jira::types::Issue;
use crate::tui::app::{ActionState, AppState, DetailFocus};
use crate::tui::render::RenderOut;

// ── Segment model ─────────────────────────────────────────────────────────────

pub enum DetailNavKind {
    Comments,
    Attachments,
}

enum Segment {
    /// Plain read-only text lines — not focusable.
    ReadOnly { lines: Vec<Line<'static>> },
    /// A navigable widget (Comments or Attachments count summary).
    NavWidget {
        nav: DetailNavKind,
        content: String,
    },
    /// A field with a bordered block. May be read-only or editable.
    EditableField {
        label: String,
        /// Text shown inside the block.
        content: String,
        /// Flat index among all editable fields. Stable per config (or iteration order for default).
        field_idx: usize,
        /// If true, Enter opens a browser link (if URL) but never opens editing.
        readonly: bool,
    },
}

// ── Public helpers ─────────────────────────────────────────────────────────────

/// Number of focusable fields in the view.
/// For custom views: total configured fields. For the default view (cfg=None): all extra fields.
pub fn num_view_fields(cfg: Option<&CustomViewConfig>, issue: Option<&Issue>) -> usize {
    match cfg {
        Some(c) => c.sections.iter().map(|s| s.fields.len()).sum(),
        None => issue.map_or(0, |i| i.fields.extra.len()),
    }
}

/// Retrieve the field config at flat index `idx`.
/// For the default view (cfg=None), synthesizes a config from the issue's extra fields.
pub fn view_field_cfg(
    cfg: Option<&CustomViewConfig>,
    issue: Option<&Issue>,
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
    } else if let Some(issue) = issue {
        let mut keys: Vec<&String> = issue.fields.extra.keys().collect();
        keys.sort();
        let key = keys.into_iter().nth(idx)?;
        Some(CustomViewFieldConfig {
            field_id: key.clone(),
            ..Default::default()
        })
    } else {
        None
    }
}

/// Resolve the display label for a field, consulting API names as fallback.
pub fn resolve_field_label(
    field: &CustomViewFieldConfig,
    field_names: &HashMap<String, String>,
) -> String {
    field
        .name
        .as_deref()
        .or_else(|| field_names.get(&field.field_id).map(String::as_str))
        .unwrap_or(&field.field_id)
        .to_string()
}

/// Public helper used by app.rs to get (`field_id`, current JSON value) for editing.
pub fn view_editable_field_spec(
    cfg: Option<&CustomViewConfig>,
    issue: &Issue,
    idx: usize,
) -> (String, serde_json::Value) {
    let Some(field_cfg) = view_field_cfg(cfg, Some(issue), idx) else {
        return (String::new(), serde_json::Value::Null);
    };
    let field_id = field_cfg.field_id.clone();
    let value = issue
        .fields
        .extra
        .get(&field_id)
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    (field_id, value)
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Render the detail view — either a configured custom view or the auto-generated default view.
/// `cfg = None` activates the default view (all issue fields).
pub fn render_detail_view(
    f: &mut Frame,
    area: Rect,
    issue: &Issue,
    app: &AppState,
    render_out: &mut RenderOut,
) -> usize {
    let cfg = current_view_config(app);
    let tz = resolve_tz(cfg);
    let w = area.width;

    let segments = build_segments(issue, cfg, tz, w, &app.field_names);

    let scroll = app.detail_scroll;
    let viewport_h = area.height as usize;
    let mut virtual_y: usize = 0;

    // Ensure offsets vec is large enough: Comments(0), Attachments(1), Field(i)=2+i
    let num_fields = num_view_fields(cfg, Some(issue));
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
        crate::tui::app::ViewMode::Custom(id) => app.config.views.get(id.as_str()),
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
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::DarkGray)
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
        Segment::EditableField {
            label,
            field_idx,
            content,
            readonly,
            ..
        } => {
            let selected =
                matches!(&app.detail_focus, DetailFocus::Field(fi) if *fi == *field_idx);
            let is_inline_edit = matches!(
                &app.action_state,
                ActionState::InlineEditingField { field_idx: fi, .. } if *fi == *field_idx
            );
            let border_style = if is_inline_edit {
                Style::default().fg(Color::Yellow)
            } else if selected && *readonly {
                Style::default()
            } else if selected {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::DarkGray)
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
    }
}

// ── Segment builder ──────────────────────────────────────────────────────────

fn build_segments(
    issue: &Issue,
    cfg: Option<&CustomViewConfig>,
    tz: FixedOffset,
    width: u16,
    field_names: &HashMap<String, String>,
) -> Vec<Segment> {
    let mut segs: Vec<Segment> = Vec::new();

    // Header (read-only) — expanded for default view
    segs.push(Segment::ReadOnly {
        lines: header_lines(issue, cfg.is_none()),
    });

    // Nav widgets: Comments and Attachments
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

    match cfg {
        Some(cfg) => {
            build_custom_segments(&mut segs, issue, cfg, tz, width, field_names);
        }
        None => {
            build_default_segments(&mut segs, issue, width, field_names);
        }
    }

    segs
}

fn build_custom_segments(
    segs: &mut Vec<Segment>,
    issue: &Issue,
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
            let content = get_field_content(issue, field, tz);
            let readonly = field.readonly.unwrap_or(false);
            segs.push(Segment::EditableField {
                label,
                content,
                field_idx: field_flat_idx,
                readonly,
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
                start_field.and_then(|f| parse_field_dt(issue, Some(f.field_id.as_str())));
            let end_dt = end_field.and_then(|f| parse_field_dt(issue, Some(f.field_id.as_str())));
            let jira_h = section
                .fields
                .iter()
                .find(|f| f.duration_role.as_deref() == Some("jira_value"))
                .and_then(|f| issue.fields.extra.get(&f.field_id))
                .and_then(serde_json::Value::as_f64);
            segs.push(Segment::ReadOnly {
                lines: duration_lines(start_dt.as_ref(), end_dt.as_ref(), jira_h),
            });
        }
    }
}

fn build_default_segments(
    segs: &mut Vec<Segment>,
    issue: &Issue,
    width: u16,
    field_names: &HashMap<String, String>,
) {
    // Description section
    if let Some(ref desc) = issue.fields.description {
        let text = json_to_text(desc);
        if !text.is_empty() {
            segs.push(Segment::ReadOnly {
                lines: vec![
                    Line::from(""),
                    section_sep("Description", width),
                    Line::from(""),
                ],
            });
            let desc_lines: Vec<Line<'static>> = text
                .replace('\r', "")
                .lines()
                .map(|l| Line::from(l.to_string()))
                .collect();
            segs.push(Segment::ReadOnly { lines: desc_lines });
        }
    }

    // Extra fields section
    if !issue.fields.extra.is_empty() {
        segs.push(Segment::ReadOnly {
            lines: vec![
                Line::from(""),
                section_sep("Fields", width),
                Line::from(""),
            ],
        });
        let mut extra_fields: Vec<(&String, &serde_json::Value)> =
            issue.fields.extra.iter().collect();
        extra_fields.sort_by_key(|(k, _)| k.as_str());
        for (field_idx, (field_id, value)) in extra_fields.into_iter().enumerate() {
            let label = field_names
                .get(field_id)
                .cloned()
                .unwrap_or_else(|| field_id.clone());
            let content = val_to_str(value);
            segs.push(Segment::EditableField {
                label,
                content,
                field_idx,
                readonly: false,
            });
        }
    }
}

fn get_field_content(issue: &Issue, field: &CustomViewFieldConfig, tz: FixedOffset) -> String {
    let Some(raw) = issue.fields.extra.get(&field.field_id) else {
        return String::new();
    };
    if raw.is_null() {
        return String::new();
    }
    if field.datetime == Some(true)
        && let Some(s) = raw.as_str()
        && let Some(dt) = parse_dt(s)
    {
        return fmt_dt(&dt, tz);
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

fn header_lines(issue: &Issue, full: bool) -> Vec<Line<'static>> {
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

fn parse_field_dt(issue: &Issue, field_id: Option<&str>) -> Option<DateTime<FixedOffset>> {
    let fid = field_id?;
    let v = issue.fields.extra.get(fid)?;
    if v.is_null() {
        return None;
    }
    v.as_str().and_then(parse_dt)
}

pub fn val_to_str(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.replace('\r', ""),
        serde_json::Value::Object(_) => ["value", "name", "displayName"]
            .iter()
            .find_map(|k| {
                v.get(k)
                    .and_then(|x| x.as_str())
                    .map(|s| s.replace('\r', ""))
            })
            .unwrap_or_else(|| v.to_string()),
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

fn local_tz() -> FixedOffset {
    let secs = chrono::Local::now().offset().local_minus_utc();
    FixedOffset::east_opt(secs)
        .unwrap_or_else(|| FixedOffset::east_opt(0).expect("UTC offset 0 is always valid"))
}

fn parse_tz_offset(s: &str) -> Option<FixedOffset> {
    let s = s.trim();
    let sign: i32 = if s.starts_with('-') { -1 } else { 1 };
    let digits = s.trim_start_matches(['+', '-']);
    let h: i32 = digits.get(..2)?.parse().ok()?;
    let m: i32 = digits.get(2..).and_then(|x| x.parse().ok()).unwrap_or(0);
    FixedOffset::east_opt(sign * (h * 3600 + m * 60))
}

// ── Formatting ───────────────────────────────────────────────────────────────

fn fmt_dt(dt: &DateTime<FixedOffset>, tz: FixedOffset) -> String {
    dt.with_timezone(&tz).format("%Y-%m-%d  %H:%M").to_string()
}

fn parse_dt(s: &str) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(s)
        .or_else(|_| DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.3f%z"))
        .ok()
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

// ── ADF / description text extraction ────────────────────────────────────────

/// Best-effort plain text extraction from Jira description (string or ADF JSON).
pub fn json_to_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Object(_) => extract_adf_text(value),
        _ => String::new(),
    }
}

fn extract_adf_text(node: &serde_json::Value) -> String {
    let mut out = String::new();
    if let Some(text) = node.get("text").and_then(|t| t.as_str()) {
        out.push_str(text);
    }
    if let Some(content) = node.get("content").and_then(|c| c.as_array()) {
        for child in content {
            out.push_str(&extract_adf_text(child));
        }
        out.push('\n');
    }
    out
}
