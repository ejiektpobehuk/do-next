//! Kanban board view: pure grouping/cursor logic (this module) plus the
//! renderer (`render_board`). Cards are grouped by board column (status id)
//! and optionally into swimlane bands; selection stays in the app's flat
//! `nav_idx` and the 3-D board cursor is derived from it every frame, so
//! cursor survival across refreshes and card moves falls out of the existing
//! key-based selection restore.

use std::collections::{HashMap, HashSet};

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::config::types::SwimlaneConfig;
use crate::items::WorkItem;
use crate::jira::types::{BoardColumn, BoardSwimlanes, Transition};
use crate::tui::app::{AppState, LanesState, NavItem, SourceState, source_config_for};
use crate::tui::theme;

/// One card on the board, pointing back into the app's flat lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoardCard {
    /// Index into `app.nav_items` — assigning it to `nav_idx` selects the card.
    pub nav_pos: usize,
    /// Index into `app.issues` for rendering.
    pub issue_idx: usize,
}

#[derive(Debug, Clone, Default)]
pub struct BoardColumnView {
    pub cards: Vec<BoardCard>,
}

/// One swimlane band: a full row of columns. A laneless board is a single
/// band with `name: None`.
#[derive(Debug, Clone)]
pub struct BoardLaneView {
    pub name: Option<String>,
    pub columns: Vec<BoardColumnView>,
}

impl BoardLaneView {
    pub fn card_count(&self) -> usize {
        self.columns.iter().map(|c| c.cards.len()).sum()
    }
}

/// The whole grouped board. `column_names` is shared by every lane so the
/// column layout stays aligned across bands.
#[derive(Debug, Clone, Default)]
pub struct BoardGrouping {
    pub column_names: Vec<String>,
    pub lanes: Vec<BoardLaneView>,
}

/// Which lane grouping to apply, resolved by the caller from config + state.
#[derive(Debug, Clone, Copy)]
pub enum LaneSpec<'a> {
    /// No swimlanes: one unnamed band.
    None,
    /// One lane per distinct display value of this field (client-side).
    Field(&'a str),
    /// Query lanes resolved at fetch time.
    Resolved(&'a BoardSwimlanes),
}

/// Name of the synthetic trailing column holding issues whose status is
/// mapped to no board column. Jira hides those from the board; we surface
/// them instead — cards must never silently vanish.
pub const OTHER_COLUMN: &str = "(other)";

/// Name of the synthetic trailing lane for field-strategy items without a
/// value, e.g. unassigned issues under assignee lanes.
fn no_value_lane(field: &str) -> String {
    format!("No {field}")
}

/// Group the board source's items into lanes × columns.
///
/// - `member_keys`: keys of the board source's `Loaded` items. Membership is
///   by key, NOT by `source_id`: cross-source dedup can leave a board issue
///   in the flat list tagged with an earlier source's id.
/// - Status is read from the flat `issues` (in-place transition writes only
///   touch the flat list), and card order follows flat-list order (board rank).
/// - Lanes with zero cards are dropped; the synthetic "(other)" column is
///   kept only when it has cards in some lane.
pub fn build_board_grouping(
    columns: &[BoardColumn],
    lanes: LaneSpec,
    member_keys: &HashSet<&str>,
    issues: &[WorkItem],
    nav_items: &[NavItem],
) -> BoardGrouping {
    let nav_pos_by_issue_idx: HashMap<usize, usize> = nav_items
        .iter()
        .enumerate()
        .filter_map(|(nav_pos, nav)| match nav {
            NavItem::Issue(issue_idx) => Some((*issue_idx, nav_pos)),
            _ => None,
        })
        .collect();

    let n_cols = columns.len() + 1; // + synthetic "(other)"
    let mut lane_names: Vec<Option<String>> = Vec::new();
    let mut lane_idx_by_name: HashMap<String, usize> = HashMap::new();
    let mut lane_columns: Vec<Vec<BoardColumnView>> = Vec::new();
    let mut no_value_cards: Vec<(usize, BoardCard)> = Vec::new(); // (col, card)

    // Resolved lanes have a fixed order; pre-create them so empty ordering
    // survives even when early lanes fill later than late ones.
    if let LaneSpec::Resolved(swimlanes) = lanes {
        for name in &swimlanes.lane_names {
            lane_idx_by_name.insert(name.clone(), lane_names.len());
            lane_names.push(Some(name.clone()));
            lane_columns.push(vec![BoardColumnView::default(); n_cols]);
        }
    }
    if matches!(lanes, LaneSpec::None) {
        lane_names.push(None);
        lane_columns.push(vec![BoardColumnView::default(); n_cols]);
    }

    for (issue_idx, item) in issues.iter().enumerate() {
        if !member_keys.contains(item.key()) {
            continue;
        }
        let Some(&nav_pos) = nav_pos_by_issue_idx.get(&issue_idx) else {
            continue;
        };
        let Some(issue) = item.as_jira() else {
            continue; // board sources only produce Jira items
        };
        let col = columns
            .iter()
            .position(|c| c.contains_status(&issue.fields.status.id))
            .unwrap_or(columns.len());
        let card = BoardCard { nav_pos, issue_idx };

        let lane = match lanes {
            LaneSpec::None => 0,
            LaneSpec::Resolved(swimlanes) => match swimlanes.assignment.get(item.key()) {
                Some(&idx) if idx < lane_names.len() => idx,
                // Unassigned with everything-else disabled: explicit opt-out
                // of showing these on the board.
                _ => continue,
            },
            LaneSpec::Field(field) => {
                let Some(value) = lane_key_for_field(item, field) else {
                    // Collected separately so the "No {field}" lane sorts last.
                    no_value_cards.push((col, card));
                    continue;
                };
                *lane_idx_by_name.entry(value.clone()).or_insert_with(|| {
                    lane_names.push(Some(value));
                    lane_columns.push(vec![BoardColumnView::default(); n_cols]);
                    lane_names.len() - 1
                })
            }
        };
        lane_columns[lane][col].cards.push(card);
    }

    if let (LaneSpec::Field(field), false) = (lanes, no_value_cards.is_empty()) {
        lane_names.push(Some(no_value_lane(field)));
        let mut cols = vec![BoardColumnView::default(); n_cols];
        for (col, card) in no_value_cards {
            cols[col].cards.push(card);
        }
        lane_columns.push(cols);
    }

    // Drop the synthetic column unless something landed in it, then drop
    // empty lanes (nothing navigable in them).
    let other_used = lane_columns
        .iter()
        .any(|cols| !cols[columns.len()].cards.is_empty());
    let mut column_names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
    if other_used {
        column_names.push(OTHER_COLUMN.to_string());
    }
    let lanes_out: Vec<BoardLaneView> = lane_names
        .into_iter()
        .zip(lane_columns)
        .map(|(name, mut cols)| {
            if !other_used {
                cols.truncate(columns.len());
            }
            BoardLaneView {
                name,
                columns: cols,
            }
        })
        .filter(|lane| lane.name.is_none() || lane.card_count() > 0)
        .collect();

    BoardGrouping {
        column_names,
        lanes: lanes_out,
    }
}

/// Display value of a field used as a lane key. Named fields that live
/// outside the `extra` map are special-cased; everything else goes through
/// the shared JSON display used by the detail views.
fn lane_key_for_field(item: &WorkItem, field: &str) -> Option<String> {
    match field {
        "priority" => item
            .as_jira()
            .and_then(|i| i.fields.priority.as_ref())
            .map(|p| p.name.clone()),
        "assignee" => item.assignee_display().map(str::to_string),
        _ => item.field(field).and_then(|v| {
            if v.is_null() {
                return None;
            }
            let s = crate::tui::views::custom::val_to_str(v);
            (!s.is_empty()).then_some(s)
        }),
    }
}

/// (lane, column, row) of the card whose `nav_pos` equals `nav_idx`, or
/// `None` when the current selection isn't on the board.
pub fn cursor_pos(g: &BoardGrouping, nav_idx: usize) -> Option<(usize, usize, usize)> {
    g.lanes.iter().enumerate().find_map(|(l, lane)| {
        lane.columns.iter().enumerate().find_map(|(c, col)| {
            col.cards
                .iter()
                .position(|card| card.nav_pos == nav_idx)
                .map(|r| (l, c, r))
        })
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoardMove {
    Left,
    Right,
    Up,
    Down,
    Top,
    Bottom,
}

/// The `nav_pos` a movement lands on, or `None` for a no-op (board edge).
///
/// - Left/Right: nearest non-empty column in that direction within the
///   current lane; the row clamps to the target column's length.
/// - Up/Down: within the column; at a band edge, cross into the adjacent
///   lane — same column if non-empty, else the nearest non-empty one.
/// - Top/Bottom: first/last card of the current column in the current lane.
/// - Off-board selection: any move snaps to the first card of the board.
pub fn move_cursor(g: &BoardGrouping, nav_idx: usize, mv: BoardMove) -> Option<usize> {
    let Some((lane, col, row)) = cursor_pos(g, nav_idx) else {
        return first_card(g);
    };
    let cards = |l: usize, c: usize| -> &[BoardCard] { &g.lanes[l].columns[c].cards };

    match mv {
        BoardMove::Left | BoardMove::Right => {
            let target = non_empty_col_towards(&g.lanes[lane], col, mv == BoardMove::Right)?;
            let target_cards = cards(lane, target);
            Some(target_cards[row.min(target_cards.len() - 1)].nav_pos)
        }
        BoardMove::Up => {
            if row > 0 {
                return Some(cards(lane, col)[row - 1].nav_pos);
            }
            let (l, c) = adjacent_lane_col(g, lane, col, false)?;
            Some(cards(l, c).last()?.nav_pos)
        }
        BoardMove::Down => {
            let column = cards(lane, col);
            if row + 1 < column.len() {
                return Some(column[row + 1].nav_pos);
            }
            let (l, c) = adjacent_lane_col(g, lane, col, true)?;
            Some(cards(l, c).first()?.nav_pos)
        }
        BoardMove::Top => Some(cards(lane, col).first()?.nav_pos),
        BoardMove::Bottom => Some(cards(lane, col).last()?.nav_pos),
    }
}

fn first_card(g: &BoardGrouping) -> Option<usize> {
    g.lanes.iter().find_map(|lane| {
        lane.columns
            .iter()
            .find_map(|col| col.cards.first().map(|card| card.nav_pos))
    })
}

/// Nearest non-empty column strictly left/right of `col` within one lane.
fn non_empty_col_towards(lane: &BoardLaneView, col: usize, right: bool) -> Option<usize> {
    if right {
        (col + 1..lane.columns.len()).find(|&c| !lane.columns[c].cards.is_empty())
    } else {
        (0..col).rev().find(|&c| !lane.columns[c].cards.is_empty())
    }
}

/// Landing (lane, column) when crossing a band edge up/down: the next lane
/// (in that direction) that has any card, preferring the same column, else
/// the nearest non-empty column (ties go left).
fn adjacent_lane_col(
    g: &BoardGrouping,
    lane: usize,
    col: usize,
    down: bool,
) -> Option<(usize, usize)> {
    let candidates: Box<dyn Iterator<Item = usize>> = if down {
        Box::new(lane + 1..g.lanes.len())
    } else {
        Box::new((0..lane).rev())
    };
    for l in candidates {
        let target = &g.lanes[l];
        if target.card_count() == 0 {
            continue;
        }
        if !target.columns[col].cards.is_empty() {
            return Some((l, col));
        }
        let nearest = target
            .columns
            .iter()
            .enumerate()
            .filter(|(_, c)| !c.cards.is_empty())
            .min_by_key(|(i, _)| (col.abs_diff(*i), *i))?
            .0;
        return Some((l, nearest));
    }
    None
}

// ── App-state integration ────────────────────────────────────────────────────

/// The active board's grouping, derived from app state. `None` only when the
/// board's column configuration hasn't arrived (or the source id is stale).
pub fn app_board_grouping(app: &AppState, source_id: &str) -> Option<BoardGrouping> {
    let config = app.board_configs.get(source_id)?;
    let member_keys: HashSet<&str> = match app.sources.get(source_id) {
        Some(SourceState::Loaded(items)) => items.iter().map(WorkItem::key).collect(),
        _ => HashSet::new(),
    };
    Some(build_board_grouping(
        &config.column_config.columns,
        lane_spec_for(app, source_id),
        &member_keys,
        &app.issues,
        &app.nav_items,
    ))
}

/// Resolve the lane strategy for a board source: field lanes come straight
/// from config; query/auto lanes need the fetch-time assignment and degrade
/// to laneless while it's loading or after it failed.
fn lane_spec_for<'a>(app: &'a AppState, source_id: &str) -> LaneSpec<'a> {
    let swimlanes = source_config_for(app.team_config(), source_id)
        .and_then(|s| s.board.as_ref())
        .and_then(|b| b.swimlanes.as_ref());
    match swimlanes {
        Some(SwimlaneConfig::Field { field }) => LaneSpec::Field(field),
        Some(SwimlaneConfig::Auto | SwimlaneConfig::Queries { .. }) => {
            match app.board_lanes.get(source_id) {
                Some(LanesState::Loaded(lanes)) => LaneSpec::Resolved(lanes),
                _ => LaneSpec::None,
            }
        }
        None => LaneSpec::None,
    }
}

// ── Rendering ────────────────────────────────────────────────────────────────

/// Minimum usable column width; narrower terminals show fewer columns and
/// window horizontally around the cursor.
const MIN_COL_WIDTH: u16 = 26;
/// Rows per card: 3 content lines + 1 blank separator.
const CARD_H: usize = 4;

/// Frame-persistent scroll offsets for the board view. Recomputing them from
/// the cursor each frame pinned the cursor to the window edge (the same bug
/// the list views had); keeping them lets the cursor roam the visible window
/// and drag it only at the edges.
#[derive(Debug, Clone, Copy, Default)]
pub struct BoardScroll {
    /// First visible column (horizontal window over columns).
    pub h_off: usize,
    /// First visible card row within the cursor's column.
    pub card_off: usize,
}

/// Render the kanban board over the full main area: an info header line,
/// then swimlane bands stacked vertically, each a row of bordered columns.
pub fn render_board(
    f: &mut Frame,
    area: Rect,
    app: &AppState,
    scroll: &mut BoardScroll,
    focused: bool,
) {
    let Some(source_id) = app.board_view.as_deref() else {
        return;
    };
    let Some(config) = app.board_configs.get(source_id) else {
        render_centered_note(
            f,
            area,
            "Board configuration unavailable — try a refresh (R)",
        );
        return;
    };
    let Some(g) = app_board_grouping(app, source_id) else {
        return;
    };
    if area.height < 4 || area.width < 8 {
        return;
    }

    let header_area = Rect { height: 1, ..area };
    let body = Rect {
        y: area.y + 1,
        height: area.height - 1,
        ..area
    };

    let n_cols = g.column_names.len().max(1);
    let visible_cols = usize::from((body.width / MIN_COL_WIDTH).max(1)).min(n_cols);
    let cursor = cursor_pos(&g, app.nav_idx);
    let cursor_col = cursor.map_or(0, |(_, c, _)| c);
    // Keep the cursor column in the horizontal window, shifting the window
    // only when the cursor crosses one of its edges.
    let mut h_off = scroll.h_off.min(n_cols - visible_cols);
    if cursor_col < h_off {
        h_off = cursor_col;
    } else if cursor_col >= h_off + visible_cols {
        h_off = cursor_col + 1 - visible_cols;
    }
    scroll.h_off = h_off;

    render_board_header(
        f,
        header_area,
        app,
        source_id,
        &config.name,
        &g,
        h_off,
        visible_cols,
    );

    let loading = matches!(
        app.sources.get(source_id),
        Some(SourceState::Pending | SourceState::Loading)
    );
    let total_cards: usize = g.lanes.iter().map(BoardLaneView::card_count).sum();
    if total_cards == 0 {
        let note = if loading {
            let frame = usize::try_from(app.tick_count).unwrap_or(0)
                % crate::tui::list::SPINNER_FRAMES.len();
            format!("{} Loading board…", crate::tui::list::SPINNER_FRAMES[frame])
        } else {
            "Board is empty".to_string()
        };
        render_centered_note(f, body, &note);
        return;
    }

    // Vertical window over bands: walk up from the cursor's lane while
    // earlier bands still fit, then render downward.
    let cursor_lane = cursor.map_or(0, |(l, _, _)| l);
    let avail = usize::from(body.height);
    let band_h = |lane: &BoardLaneView| -> usize {
        let sep = usize::from(lane.name.is_some());
        let tallest = lane
            .columns
            .iter()
            .map(|c| c.cards.len())
            .max()
            .unwrap_or(0)
            .max(1);
        sep + 2 + (tallest * CARD_H - 1)
    };
    let mut first_band = cursor_lane.min(g.lanes.len().saturating_sub(1));
    let mut used = band_h(&g.lanes[first_band]).min(avail);
    while first_band > 0 && used + band_h(&g.lanes[first_band - 1]) <= avail {
        first_band -= 1;
        used += band_h(&g.lanes[first_band]);
    }

    let mut y = body.y;
    let bottom = body.y + body.height;
    for (lane_idx, lane) in g.lanes.iter().enumerate().skip(first_band) {
        let remaining = usize::from(bottom.saturating_sub(y));
        // Below-window bands get a one-line overflow note when there's room.
        let min_h = usize::from(lane.name.is_some()) + 3;
        if remaining < min_h {
            if remaining >= 1 {
                let hidden = g.lanes.len() - lane_idx;
                let note = Line::from(Span::styled(
                    format!("▼ {hidden} more lane(s)"),
                    Style::default().add_modifier(Modifier::DIM),
                ));
                f.render_widget(
                    Paragraph::new(note),
                    Rect {
                        y,
                        height: 1,
                        width: body.width,
                        x: body.x,
                    },
                );
            }
            break;
        }
        let h = band_h(lane).min(remaining);
        #[allow(clippy::cast_possible_truncation)]
        let band_area = Rect {
            y,
            height: h as u16,
            width: body.width,
            x: body.x,
        };
        render_band(
            f,
            band_area,
            app,
            &g,
            lane_idx,
            h_off,
            visible_cols,
            lane_idx == first_band,
            cursor,
            scroll,
            focused,
        );
        y += band_area.height;
    }
}

/// Info line above the board: board name, lane-strategy note, horizontal
/// overflow indicators.
#[allow(clippy::too_many_arguments)]
fn render_board_header(
    f: &mut Frame,
    area: Rect,
    app: &AppState,
    source_id: &str,
    board_name: &str,
    g: &BoardGrouping,
    h_off: usize,
    visible_cols: usize,
) {
    let mut spans = vec![Span::styled(
        format!(" {board_name} "),
        Style::default().add_modifier(Modifier::BOLD),
    )];
    let lane_note = match app.board_lanes.get(source_id) {
        Some(LanesState::Loading) => Some("· lanes loading…".to_string()),
        Some(LanesState::Error(msg)) => Some(format!(
            "· lanes unavailable: {}",
            crate::tui::list::truncate(msg, 60)
        )),
        _ => None,
    };
    if let Some(note) = lane_note {
        spans.push(Span::styled(
            note,
            Style::default().add_modifier(Modifier::DIM),
        ));
    }
    let n_cols = g.column_names.len();
    if h_off > 0 {
        spans.push(Span::styled(
            format!("  ◀ {h_off}"),
            Style::default().add_modifier(Modifier::DIM),
        ));
    }
    let right_hidden = n_cols.saturating_sub(h_off + visible_cols);
    if right_hidden > 0 {
        spans.push(Span::styled(
            format!("  {right_hidden} ▶"),
            Style::default().add_modifier(Modifier::DIM),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// One swimlane band: optional lane separator line + a row of bordered
/// columns. Column titles (name + total count) go on the first visible band.
#[allow(clippy::too_many_arguments)]
fn render_band(
    f: &mut Frame,
    area: Rect,
    app: &AppState,
    g: &BoardGrouping,
    lane_idx: usize,
    h_off: usize,
    visible_cols: usize,
    titled: bool,
    cursor: Option<(usize, usize, usize)>,
    scroll: &mut BoardScroll,
    focused: bool,
) {
    let lane = &g.lanes[lane_idx];
    let cols_area = lane.name.as_ref().map_or(area, |name| {
        let sep = Line::from(Span::styled(
            format!("── {name} ({}) ", lane.card_count()),
            Style::default().add_modifier(Modifier::DIM),
        ));
        f.render_widget(Paragraph::new(sep), Rect { height: 1, ..area });
        Rect {
            y: area.y + 1,
            height: area.height - 1,
            ..area
        }
    });
    if cols_area.height < 2 {
        return;
    }

    #[allow(clippy::cast_possible_truncation)]
    let constraints = vec![Constraint::Ratio(1, visible_cols as u32); visible_cols];
    let slots = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(cols_area);

    for (slot, col_idx) in (h_off..h_off + visible_cols).enumerate() {
        let col_cursor_row = match cursor {
            Some((cur_lane, cur_col, cur_row)) if cur_lane == lane_idx && cur_col == col_idx => {
                Some(cur_row)
            }
            _ => None,
        };
        render_column(
            f,
            slots[slot],
            app,
            g,
            lane_idx,
            col_idx,
            titled,
            col_cursor_row,
            scroll,
            focused,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn render_column(
    f: &mut Frame,
    area: Rect,
    app: &AppState,
    g: &BoardGrouping,
    lane_idx: usize,
    col_idx: usize,
    titled: bool,
    cursor_row: Option<usize>,
    board_scroll: &mut BoardScroll,
    focused: bool,
) {
    let accent = if cursor_row.is_some() && focused {
        theme::BORDER_FOCUS
    } else {
        theme::MUTED
    };
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(accent));
    if titled {
        let total: usize = g.lanes.iter().map(|l| l.columns[col_idx].cards.len()).sum();
        block = block.title(format!(" {} ({total}) ", g.column_names[col_idx]));
    }
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 || inner.width < 4 {
        return;
    }

    let cards = &g.lanes[lane_idx].columns[col_idx].cards;
    let inner_h = usize::from(inner.height);
    // k cards need k*CARD_H - 1 rows (no trailing separator).
    let fit = (inner_h + 1) / CARD_H;
    // Scroll only the cursor's column, shifting the persisted offset just
    // enough to keep the cursor inside the visible window.
    let scroll = cursor_row.map_or(0, |row| {
        let mut off = board_scroll.card_off.min(cards.len().saturating_sub(fit));
        if row < off {
            off = row;
        } else {
            off = off.max((row + 1).saturating_sub(fit));
        }
        board_scroll.card_off = off;
        off
    });
    let visible = &cards[scroll.min(cards.len())..cards.len().min(scroll + fit)];
    let hidden_below = cards.len() - scroll.min(cards.len()) - visible.len();

    let mut lines: Vec<Line> = Vec::with_capacity(inner_h);
    for (i, card) in visible.iter().enumerate() {
        if i > 0 {
            lines.push(Line::default());
        }
        let Some(item) = app.issues.get(card.issue_idx) else {
            continue;
        };
        let selected = cursor_row == Some(scroll + i);
        lines.extend(card_lines(item, usize::from(inner.width), selected));
    }
    if hidden_below > 0 && lines.len() >= inner_h {
        lines.truncate(inner_h - 1);
        lines.push(Line::from(Span::styled(
            format!("+{hidden_below} more"),
            Style::default().add_modifier(Modifier::DIM),
        )));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

/// The 3 lines of one card: `priority key` + summary wrapped to two lines.
fn card_lines(item: &WorkItem, width: usize, selected: bool) -> Vec<Line<'static>> {
    let style = if selected {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };
    let head = format!("{} {}", item.priority_symbol(), item.key());
    let (l1, l2) = wrap_two_lines(item.title(), width);
    let pad = |s: String| format!("{s:<width$}");
    vec![
        Line::from(Span::styled(
            pad(crate::tui::list::truncate(&head, width).to_string()),
            style.add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(pad(l1), style)),
        Line::from(Span::styled(pad(l2), style)),
    ]
}

/// Char-wrap a summary into exactly two lines; the second line is cut at
/// width (same non-ellipsis truncation the list uses).
fn wrap_two_lines(s: &str, width: usize) -> (String, String) {
    if width == 0 {
        return (String::new(), String::new());
    }
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= width {
        return (s.to_string(), String::new());
    }
    let first: String = chars[..width].iter().collect();
    let rest: String = chars[width..].iter().collect();
    let second = crate::tui::list::truncate(&rest, width).to_string();
    (first, second)
}

fn render_centered_note(f: &mut Frame, area: Rect, text: &str) {
    let y = area.y + area.height / 2;
    let line = Line::from(Span::styled(
        text.to_string(),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    ))
    .centered();
    f.render_widget(
        Paragraph::new(line),
        Rect {
            y,
            height: 1,
            ..area
        },
    );
}

/// One entry of the move-card column picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardColumnChoice {
    pub name: String,
    /// First transition (Jira order) targeting any status in this column;
    /// `None` = the workflow offers no path into it.
    pub transition_id: Option<String>,
    pub is_current: bool,
}

/// Map an issue's available transitions onto the board's columns, so moving
/// a card is "pick a column" instead of "pick a workflow transition".
pub fn map_transitions_to_columns(
    columns: &[BoardColumn],
    current_status_id: &str,
    transitions: &[Transition],
) -> Vec<BoardColumnChoice> {
    columns
        .iter()
        .map(|col| BoardColumnChoice {
            name: col.name.clone(),
            transition_id: transitions
                .iter()
                .find(|t| col.contains_status(&t.to.id))
                .map(|t| t.id.clone()),
            is_current: col.contains_status(current_status_id),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jira::types::{
        BoardColumn, ColumnStatus, Issue, IssueFields, IssueTypeField, PriorityField, ProjectField,
        StatusField,
    };

    fn column(name: &str, status_ids: &[&str]) -> BoardColumn {
        BoardColumn {
            name: name.into(),
            statuses: status_ids
                .iter()
                .map(|id| ColumnStatus { id: (*id).into() })
                .collect(),
        }
    }

    fn issue(key: &str, status_id: &str, priority: Option<&str>) -> WorkItem {
        WorkItem::Jira(Issue {
            id: format!("id-{key}"),
            key: key.into(),
            fields: IssueFields {
                summary: format!("Summary {key}"),
                status: StatusField {
                    id: status_id.into(),
                    name: format!("Status {status_id}"),
                },
                priority: priority.map(|name| PriorityField {
                    id: "p".into(),
                    name: name.into(),
                }),
                assignee: None,
                reporter: None,
                issuetype: IssueTypeField {
                    id: "t1".into(),
                    name: "Task".into(),
                },
                project: ProjectField {
                    id: "p1".into(),
                    key: "PROJ".into(),
                    name: "Project".into(),
                },
                description: None,
                comment: None,
                attachment: None,
                extra: HashMap::new(),
            },
            source_id: Some("board".into()),
            subsource_idx: 0,
            partial: false,
            changelog: None,
        })
    }

    /// nav_items mirroring the flat list one-to-one.
    fn navs(issues: &[WorkItem]) -> Vec<NavItem> {
        (0..issues.len()).map(NavItem::Issue).collect()
    }

    fn member_keys(issues: &[WorkItem]) -> HashSet<&str> {
        issues.iter().map(WorkItem::key).collect()
    }

    fn cols() -> Vec<BoardColumn> {
        vec![
            column("To Do", &["1"]),
            column("In Progress", &["2", "3"]),
            column("Done", &["4"]),
        ]
    }

    #[test]
    fn grouping_joins_by_status_id_in_config_order() {
        let issues = vec![
            issue("A-1", "2", None),
            issue("A-2", "1", None),
            issue("A-3", "4", None),
            issue("A-4", "3", None),
        ];
        let g = build_board_grouping(
            &cols(),
            LaneSpec::None,
            &member_keys(&issues),
            &issues,
            &navs(&issues),
        );
        assert_eq!(g.column_names, ["To Do", "In Progress", "Done"]);
        assert_eq!(g.lanes.len(), 1);
        let lane = &g.lanes[0];
        assert_eq!(
            lane.columns[0].cards,
            [BoardCard {
                nav_pos: 1,
                issue_idx: 1
            }]
        );
        // Multi-status column keeps flat (rank) order.
        assert_eq!(
            lane.columns[1].cards,
            [
                BoardCard {
                    nav_pos: 0,
                    issue_idx: 0
                },
                BoardCard {
                    nav_pos: 3,
                    issue_idx: 3
                }
            ]
        );
        assert_eq!(lane.columns[2].cards.len(), 1);
    }

    #[test]
    fn membership_is_by_key_and_unmapped_status_gets_other_column() {
        let issues = vec![
            issue("A-1", "1", None),
            issue("B-9", "1", None),   // not a board member
            issue("A-2", "999", None), // status mapped to no column
        ];
        let members: HashSet<&str> = ["A-1", "A-2"].into();
        let g = build_board_grouping(&cols(), LaneSpec::None, &members, &issues, &navs(&issues));
        assert_eq!(
            g.column_names.last().map(String::as_str),
            Some(OTHER_COLUMN)
        );
        assert_eq!(g.lanes[0].columns[0].cards.len(), 1); // A-1 only, B-9 excluded
        assert_eq!(
            g.lanes[0].columns[3].cards,
            [BoardCard {
                nav_pos: 2,
                issue_idx: 2
            }]
        );

        // No unmapped statuses → no synthetic column.
        let mapped = vec![issue("A-1", "1", None)];
        let g = build_board_grouping(
            &cols(),
            LaneSpec::None,
            &member_keys(&mapped),
            &mapped,
            &navs(&mapped),
        );
        assert_eq!(g.column_names.len(), 3);
        assert_eq!(g.lanes[0].columns.len(), 3);
    }

    #[test]
    fn field_lanes_group_by_value_with_no_value_lane_last() {
        let issues = vec![
            issue("A-1", "1", Some("High")),
            issue("A-2", "1", None),
            issue("A-3", "2", Some("Low")),
            issue("A-4", "1", Some("High")),
        ];
        let g = build_board_grouping(
            &cols(),
            LaneSpec::Field("priority"),
            &member_keys(&issues),
            &issues,
            &navs(&issues),
        );
        let names: Vec<_> = g.lanes.iter().map(|l| l.name.as_deref().unwrap()).collect();
        // First-appearance order, "No priority" forced last.
        assert_eq!(names, ["High", "Low", "No priority"]);
        assert_eq!(g.lanes[0].card_count(), 2);
        assert_eq!(g.lanes[2].columns[0].cards[0].issue_idx, 1);
    }

    #[test]
    fn resolved_lanes_keep_order_skip_empty_and_drop_unassigned() {
        let issues = vec![
            issue("A-1", "1", None),
            issue("A-2", "1", None),
            issue("A-3", "2", None),
        ];
        let swimlanes = BoardSwimlanes {
            lane_names: vec![
                "Expedite".into(),
                "Ghost town".into(),
                "Everything Else".into(),
            ],
            // A-3 deliberately unassigned (everything_else: false case).
            assignment: [("A-1".to_string(), 2), ("A-2".to_string(), 0)].into(),
        };
        let g = build_board_grouping(
            &cols(),
            LaneSpec::Resolved(&swimlanes),
            &member_keys(&issues),
            &issues,
            &navs(&issues),
        );
        let names: Vec<_> = g.lanes.iter().map(|l| l.name.as_deref().unwrap()).collect();
        assert_eq!(names, ["Expedite", "Everything Else"]); // empty lane dropped, order kept
        assert_eq!(g.lanes[0].columns[0].cards[0].issue_idx, 1);
        assert_eq!(g.lanes[1].columns[0].cards[0].issue_idx, 0);
    }

    // Board used by the movement tests:
    //   lane 0:  To Do [A-1]        In Progress [A-2, A-3]   Done []
    //   lane 1:  To Do []           In Progress [B-1]        Done [B-2]
    fn movement_fixture() -> (Vec<WorkItem>, BoardSwimlanes) {
        let issues = vec![
            issue("A-1", "1", None),
            issue("A-2", "2", None),
            issue("A-3", "2", None),
            issue("B-1", "2", None),
            issue("B-2", "4", None),
        ];
        let swimlanes = BoardSwimlanes {
            lane_names: vec!["Alpha".into(), "Beta".into()],
            assignment: [
                ("A-1".to_string(), 0),
                ("A-2".to_string(), 0),
                ("A-3".to_string(), 0),
                ("B-1".to_string(), 1),
                ("B-2".to_string(), 1),
            ]
            .into(),
        };
        (issues, swimlanes)
    }

    #[test]
    fn horizontal_moves_skip_empty_columns_and_clamp_row() {
        let (issues, swimlanes) = movement_fixture();
        let g = build_board_grouping(
            &cols(),
            LaneSpec::Resolved(&swimlanes),
            &member_keys(&issues),
            &issues,
            &navs(&issues),
        );
        // From A-3 (lane 0, col 1, row 1): Left clamps row into To Do's single card.
        assert_eq!(move_cursor(&g, 2, BoardMove::Left), Some(0));
        // Right from A-3: Done is empty in lane 0 → no-op.
        assert_eq!(move_cursor(&g, 2, BoardMove::Right), None);
        // In lane 1, Left from B-2 (col 2) lands on the nearest non-empty
        // column to its left (In Progress).
        assert_eq!(move_cursor(&g, 4, BoardMove::Left), Some(3));
    }

    #[test]
    fn vertical_moves_cross_lane_boundaries() {
        let (issues, swimlanes) = movement_fixture();
        let g = build_board_grouping(
            &cols(),
            LaneSpec::Resolved(&swimlanes),
            &member_keys(&issues),
            &issues,
            &navs(&issues),
        );
        // Down within a column.
        assert_eq!(move_cursor(&g, 1, BoardMove::Down), Some(2));
        // Down from the bottom of lane 0's In Progress → lane 1, same column.
        assert_eq!(move_cursor(&g, 2, BoardMove::Down), Some(3));
        // Down from A-1 (lane 0, To Do): lane 1's To Do is empty → nearest
        // non-empty column (In Progress).
        assert_eq!(move_cursor(&g, 0, BoardMove::Down), Some(3));
        // Up from B-1 crosses back into lane 0 (same column, last card).
        assert_eq!(move_cursor(&g, 3, BoardMove::Up), Some(2));
        // Up from the very top is a no-op.
        assert_eq!(move_cursor(&g, 0, BoardMove::Up), None);
        // Top/Bottom stay within the current column and lane.
        assert_eq!(move_cursor(&g, 2, BoardMove::Top), Some(1));
        assert_eq!(move_cursor(&g, 1, BoardMove::Bottom), Some(2));
    }

    #[test]
    fn off_board_cursor_snaps_to_first_card() {
        let (issues, swimlanes) = movement_fixture();
        let g = build_board_grouping(
            &cols(),
            LaneSpec::Resolved(&swimlanes),
            &member_keys(&issues),
            &issues,
            &navs(&issues),
        );
        assert_eq!(cursor_pos(&g, 999), None);
        assert_eq!(move_cursor(&g, 999, BoardMove::Down), Some(0));
    }

    #[test]
    fn transitions_map_to_columns_first_match_wins() {
        let transitions = vec![
            Transition {
                id: "11".into(),
                name: "Start".into(),
                to: StatusField {
                    id: "2".into(),
                    name: "In Progress".into(),
                },
            },
            Transition {
                id: "12".into(),
                name: "Review".into(),
                to: StatusField {
                    id: "3".into(),
                    name: "In Review".into(),
                },
            },
        ];
        let choices = map_transitions_to_columns(&cols(), "1", &transitions);
        assert_eq!(choices.len(), 3);
        assert!(choices[0].is_current);
        assert_eq!(choices[0].transition_id, None); // no transition back into To Do
        // Both transitions target the multi-status column; the first wins.
        assert_eq!(choices[1].transition_id.as_deref(), Some("11"));
        assert_eq!(choices[2].transition_id, None);
    }
}
