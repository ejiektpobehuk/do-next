//! The standup screen: what you did since the previous standup, as a
//! day-by-day timeline over the full main area.
//!
//! The rows are derived from the flat entry list every frame via
//! [`crate::standup::group`], because one item can appear under several days.
//! Selection is an index into the *entry* rows only — day and item headings are
//! labels, not destinations.

use std::fmt::Write as _;

use chrono::Datelike;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::standup::group::{self, DayGroup};
use crate::standup::types::{ItemRef, StandupEntry};
use crate::tui::app::AppState;

/// Vertical scroll offset, persisted across frames like `BoardScroll`.
#[derive(Default)]
pub struct StandupScroll {
    /// First visible row.
    pub off: usize,
}

/// One rendered line of the timeline.
#[derive(Debug)]
pub enum Row<'a> {
    /// A day heading and how many entries fall under it.
    Day { label: String, entries: usize },
    /// An item heading.
    Item(&'a ItemRef),
    /// One thing you did. `idx` is its position among entry rows — what
    /// `AppState::standup_selected` refers to.
    Entry { entry: &'a StandupEntry, idx: usize },
    /// Blank separator between days.
    Spacer,
}

/// Flatten grouped days into renderable rows.
///
/// Pure, so the row/selection mapping is testable without a terminal.
pub fn rows<'a>(days: &'a [DayGroup<'a>]) -> Vec<Row<'a>> {
    let mut out = Vec::new();
    let mut entry_idx = 0;
    for (i, day) in days.iter().enumerate() {
        if i > 0 {
            out.push(Row::Spacer);
        }
        out.push(Row::Day {
            label: format!(
                "{} {} {}",
                day.date.format("%A"),
                day.date.day(),
                day.date.format("%B")
            ),
            entries: day.entry_count(),
        });
        for item in &day.items {
            out.push(Row::Item(item.item));
            for entry in &item.entries {
                out.push(Row::Entry {
                    entry,
                    idx: entry_idx,
                });
                entry_idx += 1;
            }
        }
    }
    out
}

/// Scroll offset that keeps `selected_row` visible without overscrolling.
///
/// Pure because viewport arithmetic is where off-by-ones live: a selection on the
/// last visible line must not scroll, and the offset must never leave blank space
/// below the final row.
pub fn scroll_offset(
    current: usize,
    total_rows: usize,
    viewport: usize,
    selected_row: Option<usize>,
) -> usize {
    if viewport == 0 {
        return 0;
    }
    let mut off = current;
    if let Some(row) = selected_row {
        if row < off {
            off = row;
        } else if row >= off + viewport {
            off = row + 1 - viewport;
        }
    }
    off.min(total_rows.saturating_sub(viewport))
}

/// Which row index holds entry `selected`, if any.
pub fn row_of_entry(rows: &[Row<'_>], selected: usize) -> Option<usize> {
    rows.iter()
        .position(|r| matches!(r, Row::Entry { idx, .. } if *idx == selected))
}

/// Render the timeline over the whole main area.
pub fn render_standup(
    f: &mut Frame,
    area: Rect,
    app: &AppState,
    scroll: &mut StandupScroll,
    focused: bool,
) {
    let border_style = if focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(header_title(app));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let Some(source_id) = app.standup_source_id().map(str::to_owned) else {
        return;
    };

    let Some(data) = app.standup_data.get(&source_id).and_then(|s| s.loaded()) else {
        let note = if app
            .standup_data
            .get(&source_id)
            .is_some_and(|s| matches!(s, crate::tui::app::CacheState::Failed(_)))
        {
            "Standup collection failed — see the source rows for details."
        } else {
            "Collecting your activity…"
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                note,
                Style::default().add_modifier(Modifier::DIM),
            ))),
            inner,
        );
        return;
    };

    let window = app.standup_window();
    let tz = app.standup_tz();
    let entries = data.entries_in(&window);
    if entries.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "Nothing recorded in this window. Press < to look further back.",
                Style::default().add_modifier(Modifier::DIM),
            ))),
            inner,
        );
        return;
    }

    let days = group::by_day(&entries, tz);
    let all_rows = rows(&days);

    scroll.off = scroll_offset(
        scroll.off,
        all_rows.len(),
        inner.height as usize,
        row_of_entry(&all_rows, app.standup_selected),
    );
    let viewport = inner.height as usize;

    let lines: Vec<Line> = all_rows
        .iter()
        .skip(scroll.off)
        .take(viewport)
        .map(|row| render_row(row, app.standup_selected, tz, focused))
        .collect();

    f.render_widget(Paragraph::new(lines), inner);
}

/// Title bar: the window, the entry count, and any accuracy caveat.
fn header_title(app: &AppState) -> String {
    let window = app.standup_window();
    let tz = app.standup_tz();
    let start = window.start.with_timezone(&tz.offset_at(window.start));
    let mut title = format!(" Standup — since {} ", start.format("%a %H:%M"));

    if let Some(data) = app
        .standup_source_id()
        .and_then(|id| app.standup_data.get(id))
        .and_then(|s| s.loaded())
    {
        let count = data.entries_in(&window).len();
        let _ = write!(title, "· {count} entr{} ", plural(count));
        if !data.degraded.is_empty() {
            title.push_str("· reduced accuracy ");
        }
    }
    title
}

const fn plural(n: usize) -> &'static str {
    if n == 1 { "y" } else { "ies" }
}

fn render_row<'a>(
    row: &Row<'a>,
    selected: usize,
    tz: crate::datetime::TzSpec,
    focused: bool,
) -> Line<'a> {
    match row {
        Row::Spacer => Line::from(""),
        Row::Day { label, entries } => Line::from(vec![
            Span::styled(
                label.clone(),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {entries} entr{}", plural(*entries)),
                Style::default().add_modifier(Modifier::DIM),
            ),
        ]),
        Row::Item(item) => Line::from(vec![
            Span::raw("  "),
            Span::styled(
                item.backend.symbol().to_owned(),
                Style::default().fg(Color::Cyan),
            ),
            Span::raw(" "),
            Span::styled(
                item.key.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                item.title.clone(),
                Style::default().add_modifier(Modifier::DIM),
            ),
        ]),
        Row::Entry { entry, idx } => {
            let at = entry.at.with_timezone(&tz.offset_at(entry.at));
            let is_selected = *idx == selected;
            let marker = if is_selected && focused { "▶ " } else { "  " };
            let mut style = Style::default();
            if is_selected {
                style = style.add_modifier(Modifier::REVERSED);
            }
            if !entry.kind.is_confident() {
                // A merge request that moved without provably moving *by you*.
                style = style.add_modifier(Modifier::DIM);
            }

            let mut text = format!("{marker}    {}  {}", at.format("%H:%M"), entry.kind.verb());
            if !entry.detail.is_empty() {
                let _ = write!(text, "  {}", entry.detail);
            }
            Line::from(Span::styled(text, style))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datetime::TzSpec;
    use crate::standup::types::{Backend, EntryKind};
    use chrono::{DateTime, FixedOffset, TimeZone, Utc};

    fn tz() -> TzSpec {
        TzSpec::Fixed(FixedOffset::east_opt(0).expect("valid"))
    }

    fn fri(h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 31, h, 0, 0)
            .single()
            .expect("valid")
    }

    fn mon(h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 3, h, 0, 0)
            .single()
            .expect("valid")
    }

    fn entry(at: DateTime<Utc>, key: &str) -> StandupEntry {
        StandupEntry {
            at,
            item: ItemRef {
                key: key.to_owned(),
                title: format!("{key} summary"),
                url: String::new(),
                backend: Backend::Jira,
            },
            kind: EntryKind::Created,
            detail: String::new(),
        }
    }

    #[test]
    fn rows_interleave_day_item_and_entry_lines() {
        let entries = vec![entry(fri(11), "A-1"), entry(fri(12), "A-1")];
        let refs: Vec<&StandupEntry> = entries.iter().collect();
        let days = group::by_day(&refs, tz());
        let rows = rows(&days);

        assert!(matches!(rows[0], Row::Day { entries: 2, .. }));
        assert!(matches!(rows[1], Row::Item(_)));
        assert!(matches!(rows[2], Row::Entry { idx: 0, .. }));
        assert!(matches!(rows[3], Row::Entry { idx: 1, .. }));
        assert_eq!(rows.len(), 4);
    }

    #[test]
    fn entry_indices_are_continuous_across_days_and_items() {
        // Selection must be able to walk the whole timeline without gaps, even
        // though headings are interleaved.
        let entries = vec![
            entry(fri(11), "A-1"),
            entry(fri(12), "B-2"),
            entry(mon(9), "A-1"),
        ];
        let refs: Vec<&StandupEntry> = entries.iter().collect();
        let days = group::by_day(&refs, tz());
        let rows = rows(&days);

        let indices: Vec<usize> = rows
            .iter()
            .filter_map(|r| match r {
                Row::Entry { idx, .. } => Some(*idx),
                _ => None,
            })
            .collect();
        assert_eq!(indices, vec![0, 1, 2]);
    }

    #[test]
    fn a_spacer_separates_days_but_never_leads() {
        let entries = vec![entry(fri(11), "A-1"), entry(mon(9), "A-1")];
        let refs: Vec<&StandupEntry> = entries.iter().collect();
        let days = group::by_day(&refs, tz());
        let rows = rows(&days);

        assert!(!matches!(rows[0], Row::Spacer));
        assert_eq!(
            rows.iter().filter(|r| matches!(r, Row::Spacer)).count(),
            1,
            "one spacer for two days"
        );
    }

    #[test]
    fn row_of_entry_maps_selection_to_a_line() {
        let entries = vec![entry(fri(11), "A-1"), entry(mon(9), "B-2")];
        let refs: Vec<&StandupEntry> = entries.iter().collect();
        let days = group::by_day(&refs, tz());
        let rows = rows(&days);

        // Day, Item, Entry(0), Spacer, Day, Item, Entry(1)
        assert_eq!(row_of_entry(&rows, 0), Some(2));
        assert_eq!(row_of_entry(&rows, 1), Some(6));
        assert_eq!(row_of_entry(&rows, 99), None);
    }

    #[test]
    fn no_days_yields_no_rows() {
        assert!(rows(&[]).is_empty());
    }

    #[test]
    fn scrolling_keeps_the_selection_visible() {
        // 20 rows, 5-line viewport.
        // Already visible: no movement.
        assert_eq!(scroll_offset(0, 20, 5, Some(3)), 0);
        // On the last visible line: still no movement (the off-by-one case).
        assert_eq!(scroll_offset(0, 20, 5, Some(4)), 0);
        // One past it: scroll by exactly one.
        assert_eq!(scroll_offset(0, 20, 5, Some(5)), 1);
        // Above the viewport: jump up to it.
        assert_eq!(scroll_offset(10, 20, 5, Some(2)), 2);
    }

    #[test]
    fn scrolling_never_leaves_blank_space_below_the_last_row() {
        assert_eq!(scroll_offset(99, 20, 5, None), 15);
        // Fewer rows than the viewport pins the offset at zero.
        assert_eq!(scroll_offset(3, 2, 5, None), 0);
    }

    #[test]
    fn a_zero_height_viewport_does_not_underflow() {
        assert_eq!(scroll_offset(7, 20, 0, Some(9)), 0);
    }

    #[test]
    fn plurals_read_correctly() {
        assert_eq!(plural(1), "y");
        assert_eq!(plural(0), "ies");
        assert_eq!(plural(3), "ies");
    }
}
