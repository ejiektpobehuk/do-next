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

use crate::config::types::{PostmortemFieldConfig, PostmortemViewConfig};
use crate::jira::types::Issue;
use crate::tui::app::{ActionState, AppState};
use crate::tui::render::RenderOut;

// ── Segment model ─────────────────────────────────────────────────────────────

enum Segment {
    /// Plain read-only text lines — not focusable.
    ReadOnly { lines: Vec<Line<'static>> },
    /// A field with a bordered block. May be read-only or editable.
    EditableField {
        label: String,
        /// Text shown inside the block.
        content: String,
        /// Flat index among all editable fields. Stable per config.
        field_idx: usize,
        /// If true, Enter opens a browser link (if URL) but never opens editing.
        readonly: bool,
    },
}

// ── Public helpers ─────────────────────────────────────────────────────────────

pub fn num_postmortem_fields(cfg: Option<&PostmortemViewConfig>) -> usize {
    cfg.map_or(0, |c| c.sections.iter().map(|s| s.fields.len()).sum())
}

/// Flat-index into the sections to retrieve a field config.
pub fn postmortem_field_cfg(
    cfg: Option<&PostmortemViewConfig>,
    idx: usize,
) -> Option<&PostmortemFieldConfig> {
    let cfg = cfg?;
    let mut count = 0;
    for section in &cfg.sections {
        for field in &section.fields {
            if count == idx {
                return Some(field);
            }
            count += 1;
        }
    }
    None
}

/// Resolve the display label for a field, consulting API names as fallback.
pub fn resolve_field_label(
    field: &PostmortemFieldConfig,
    field_names: &HashMap<String, String>,
) -> String {
    field
        .name
        .as_deref()
        .or_else(|| field_names.get(&field.field_id).map(String::as_str))
        .unwrap_or(&field.field_id)
        .to_string()
}

/// Return the configured hint for editable field `idx`, if any.
pub fn postmortem_field_hint(cfg: Option<&PostmortemViewConfig>, idx: usize) -> Option<String> {
    postmortem_field_cfg(cfg, idx)?.hint.clone()
}

/// Return true if the field at `idx` is marked readonly.
pub fn postmortem_field_is_readonly(cfg: Option<&PostmortemViewConfig>, idx: usize) -> bool {
    postmortem_field_cfg(cfg, idx)
        .and_then(|f| f.readonly)
        .unwrap_or(false)
}

/// Public helper used by app.rs to get (`field_id`, current JSON value) for editing.
pub fn postmortem_editable_field_spec(
    cfg: Option<&PostmortemViewConfig>,
    issue: &Issue,
    idx: usize,
) -> (String, serde_json::Value) {
    let Some(field_cfg) = postmortem_field_cfg(cfg, idx) else {
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

pub fn render_postmortem(
    f: &mut Frame,
    area: Rect,
    issue: &Issue,
    app: &AppState,
    render_out: &mut RenderOut,
) -> usize {
    let cfg = app.config.view_modes.postmortem.as_ref();
    let tz = resolve_tz(cfg);
    let w = area.width;

    let segments = build_segments(issue, cfg, tz, w, &app.postmortem_field_names);

    let scroll = app.detail_scroll;
    let viewport_h = area.height as usize;
    let mut virtual_y: usize = 0;

    // Ensure field offsets vec is large enough for all editable fields
    let num_fields = num_postmortem_fields(cfg);
    render_out
        .postmortem_field_offsets
        .resize(num_fields, (0, 0));

    for seg in &segments {
        let seg_height = measure_segment(seg, w);
        let seg_top = virtual_y;
        let seg_bot = virtual_y + seg_height;
        virtual_y += seg_height;

        // Always record field positions (used by auto-scroll, even for off-screen fields)
        if let Segment::EditableField { field_idx, .. } = seg
            && *field_idx < render_out.postmortem_field_offsets.len()
        {
            render_out.postmortem_field_offsets[*field_idx] = (seg_top, seg_bot);
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
        Segment::EditableField {
            label,
            field_idx,
            content,
            readonly,
            ..
        } => {
            let selected = app.postmortem_field_idx == *field_idx;
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
    cfg: Option<&PostmortemViewConfig>,
    tz: FixedOffset,
    width: u16,
    field_names: &HashMap<String, String>,
) -> Vec<Segment> {
    let mut segs: Vec<Segment> = Vec::new();

    // Header (read-only)
    segs.push(Segment::ReadOnly {
        lines: header_lines(issue),
    });

    let sections = cfg.map_or(&[][..], |c| c.sections.as_slice());
    let mut field_flat_idx = 0usize;

    for (sec_idx, section) in sections.iter().enumerate() {
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

    segs
}

fn get_field_content(issue: &Issue, field: &PostmortemFieldConfig, tz: FixedOffset) -> String {
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

fn header_lines(issue: &Issue) -> Vec<Line<'static>> {
    vec![
        Line::from(vec![
            Span::raw(issue.fields.summary.clone()),
            Span::raw("  "),
            Span::styled(
                issue.fields.status.name.clone(),
                Style::default().add_modifier(Modifier::DIM),
            ),
        ]),
        Line::from(""),
    ]
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

pub fn resolve_tz(cfg: Option<&PostmortemViewConfig>) -> FixedOffset {
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
