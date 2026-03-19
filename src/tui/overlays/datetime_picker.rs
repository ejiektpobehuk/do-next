use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Utc};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use crate::tui::app::ActionState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DatetimePickerMode {
    Date,
    Time,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimeFocus {
    Hour,
    Minute,
}

#[derive(Debug, Clone)]
pub struct DatetimePicker {
    pub mode: DatetimePickerMode,
    pub view_year: i32,
    pub view_month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub time_focus: TimeFocus,
    pub tz: FixedOffset,
    /// Buffer holding the first digit of a two-digit time input.
    pub digit_buf: Option<u32>,
    /// Tracks first `g` press for `gg` (go to first day of month) motion.
    pub pending_g: bool,
}

impl DatetimePicker {
    pub fn from_value(value: &serde_json::Value, tz: FixedOffset) -> Self {
        let dt: DateTime<FixedOffset> = value
            .as_str()
            .and_then(|s| {
                chrono::DateTime::parse_from_rfc3339(s)
                    .or_else(|_| chrono::DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.3f%z"))
                    .ok()
            })
            .map_or_else(|| Utc::now().with_timezone(&tz), |dt| dt.with_timezone(&tz));

        Self {
            mode: DatetimePickerMode::Date,
            view_year: dt.year(),
            view_month: dt.month(),
            day: dt.day(),
            hour: dt.hour(),
            minute: dt.minute(),
            time_focus: TimeFocus::Hour,
            tz,
            digit_buf: None,
            pending_g: false,
        }
    }

    pub fn to_iso_string(&self) -> String {
        let date = NaiveDate::from_ymd_opt(self.view_year, self.view_month, self.day)
            .unwrap_or_else(|| NaiveDate::from_ymd_opt(2000, 1, 1).expect("static date"));
        let time = NaiveTime::from_hms_opt(self.hour, self.minute, 0).unwrap_or_default();
        let naive = NaiveDateTime::new(date, time);
        // Convert local naive → UTC naive, then attach offset
        let offset_secs = i64::from(self.tz.local_minus_utc());
        let utc_naive = naive - chrono::Duration::seconds(offset_secs);
        let dt = DateTime::<FixedOffset>::from_naive_utc_and_offset(utc_naive, self.tz);
        dt.format("%Y-%m-%dT%H:%M:%S%.3f%z").to_string()
    }

    pub fn days_in_month(&self) -> u32 {
        days_in_month(self.view_year, self.view_month)
    }

    pub fn clamp_day(&mut self) {
        let max = self.days_in_month();
        if self.day > max {
            self.day = max;
        }
    }
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    NaiveDate::from_ymd_opt(next_year, next_month, 1)
        .and_then(|d| d.pred_opt())
        .map_or(28, |d| d.day())
}

fn weekday_of_first(year: i32, month: u32) -> usize {
    NaiveDate::from_ymd_opt(year, month, 1)
        .map_or(0, |d| d.weekday().num_days_from_monday() as usize)
}

pub const fn advance_month(picker: &mut DatetimePicker) {
    if picker.view_month == 12 {
        picker.view_month = 1;
        picker.view_year += 1;
    } else {
        picker.view_month += 1;
    }
}

pub const fn retreat_month(picker: &mut DatetimePicker) {
    if picker.view_month == 1 {
        picker.view_month = 12;
        picker.view_year -= 1;
    } else {
        picker.view_month -= 1;
    }
}

// ── Key handlers ─────────────────────────────────────────────────────────────

pub fn handle_date_key(picker: &mut DatetimePicker, code: crossterm::event::KeyCode) {
    use crossterm::event::KeyCode;

    // `gg` handling: first `g` sets pending_g; second `g` jumps to day 1.
    if code == KeyCode::Char('g') {
        if picker.pending_g {
            picker.day = 1;
            picker.pending_g = false;
        } else {
            picker.pending_g = true;
        }
        return;
    }

    picker.pending_g = false;

    match code {
        KeyCode::Char('G') => {
            picker.day = picker.days_in_month();
        }
        KeyCode::Char('h') | KeyCode::Left => {
            if picker.day > 1 {
                picker.day -= 1;
            }
        }
        KeyCode::Char('l') | KeyCode::Right => {
            if picker.day < picker.days_in_month() {
                picker.day += 1;
            }
        }
        KeyCode::Char('j') | KeyCode::Down => {
            let new_day = picker.day + 7;
            if new_day > picker.days_in_month() {
                let overflow = new_day - picker.days_in_month();
                advance_month(picker);
                picker.day = overflow;
            } else {
                picker.day = new_day;
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if picker.day > 7 {
                picker.day -= 7;
            } else {
                let borrow = 7 - picker.day; // 0..=6, u32
                retreat_month(picker);
                picker.day = picker.days_in_month() - borrow;
            }
        }
        KeyCode::Char('0') => {
            // First day of current week row (Monday of selected week)
            let first_weekday = weekday_of_first(picker.view_year, picker.view_month);
            let col = (first_weekday + picker.day as usize - 1) % 7;
            let monday = picker.day as usize - col;
            picker.day = u32::try_from(monday.max(1)).unwrap_or(1);
        }
        KeyCode::Char('$') => {
            // Last day of current week row (Sunday of selected week), clamped to month end
            let first_weekday = weekday_of_first(picker.view_year, picker.view_month);
            let col = (first_weekday + picker.day as usize - 1) % 7;
            let sunday = picker.day as usize + (6 - col);
            picker.day = u32::try_from(sunday)
                .unwrap_or(u32::MAX)
                .min(picker.days_in_month());
        }
        KeyCode::Char('[' | '{' | 'K') => {
            retreat_month(picker);
            picker.clamp_day();
        }
        KeyCode::Char(']' | '}' | 'J') => {
            advance_month(picker);
            picker.clamp_day();
        }
        KeyCode::Char('t') | KeyCode::Tab => {
            picker.mode = DatetimePickerMode::Time;
        }
        _ => {}
    }
}

pub fn handle_time_key(picker: &mut DatetimePicker, code: crossterm::event::KeyCode) {
    use crossterm::event::KeyCode;

    match code {
        KeyCode::Char('h') | KeyCode::Left => {
            if picker.time_focus == TimeFocus::Hour {
                picker.mode = DatetimePickerMode::Date;
            } else {
                picker.time_focus = TimeFocus::Hour;
            }
            picker.digit_buf = None;
        }
        KeyCode::Char('l') | KeyCode::Right => {
            picker.time_focus = TimeFocus::Minute;
            picker.digit_buf = None;
        }
        KeyCode::Char('k') | KeyCode::Up => {
            match picker.time_focus {
                TimeFocus::Hour => picker.hour = (picker.hour + 1) % 24,
                TimeFocus::Minute => picker.minute = (picker.minute + 1) % 60,
            }
            picker.digit_buf = None;
        }
        KeyCode::Char('j') | KeyCode::Down => {
            match picker.time_focus {
                TimeFocus::Hour => picker.hour = picker.hour.wrapping_add(23) % 24,
                TimeFocus::Minute => picker.minute = picker.minute.wrapping_add(59) % 60,
            }
            picker.digit_buf = None;
        }
        KeyCode::Char(c @ '0'..='9') => {
            let digit = c.to_digit(10).unwrap_or(0);
            if let Some(first) = picker.digit_buf.take() {
                let value = first * 10 + digit;
                match picker.time_focus {
                    TimeFocus::Hour => {
                        picker.hour = value.min(23);
                        picker.time_focus = TimeFocus::Minute;
                    }
                    TimeFocus::Minute => {
                        picker.minute = value.min(59);
                    }
                }
            } else {
                picker.digit_buf = Some(digit);
            }
        }
        KeyCode::Char('t') | KeyCode::Tab | KeyCode::BackTab => {
            picker.mode = DatetimePickerMode::Date;
            picker.digit_buf = None;
        }
        _ => {
            picker.digit_buf = None;
        }
    }
}

// ── Render ───────────────────────────────────────────────────────────────────

pub fn render_datetime_picker_overlay(f: &mut Frame, app_action: &ActionState) {
    let ActionState::EditingDatetimeField {
        label,
        description,
        picker,
        ..
    } = app_action
    else {
        return;
    };

    let area = centered_rect(30, 40, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {label} "))
        .title_bottom(footer_title(picker));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Vertical split: optional description | main (calendar + time) | bottom datetime
    let content_area = description.as_ref().map_or(inner, |desc| {
        let hint_lines = u16::try_from(desc.chars().count())
            .unwrap_or(u16::MAX)
            .div_ceil(inner.width.saturating_sub(2).max(1))
            + 1;
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(hint_lines), Constraint::Min(0)])
            .split(inner);
        f.render_widget(
            Paragraph::new(desc.as_str())
                .style(Style::default().add_modifier(Modifier::DIM))
                .wrap(ratatui::widgets::Wrap { trim: false }),
            chunks[0],
        );
        chunks[1]
    });

    let vert = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(content_area);
    let main_area = vert[0];
    let bottom_area = vert[1];

    // Horizontal split: calendar (left) | time picker (right, fixed width)
    let horiz = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(13)])
        .split(main_area);
    let cal_area = horiz[0];
    let time_area = horiz[1];

    render_calendar(f, cal_area, picker);
    render_time_side(f, time_area, picker);
    render_bottom_datetime(f, bottom_area, picker);
}

fn render_calendar(f: &mut Frame, area: Rect, picker: &DatetimePicker) {
    let dim_all = picker.mode == DatetimePickerMode::Time;
    let base_dim = Style::default().add_modifier(Modifier::DIM);

    let mut lines: Vec<Line> = Vec::new();

    let header = format!("  {}  {}", picker.view_year, month_name(picker.view_month));
    lines.push(Line::from(if dim_all {
        Span::styled(header, base_dim)
    } else {
        Span::raw(header)
    }));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("  Mo Tu We Th Fr Sa Su", base_dim)));

    let first_weekday = weekday_of_first(picker.view_year, picker.view_month);
    let days_total = picker.days_in_month() as usize;
    let total_cells = first_weekday + days_total;
    let rows = total_cells.div_ceil(7);

    for row in 0..rows {
        let mut row_spans: Vec<Span> = vec![Span::raw("  ")];
        for col in 0..7 {
            let cell = row * 7 + col;
            let day_num: Option<u32> = if cell >= first_weekday && cell < first_weekday + days_total
            {
                Some(u32::try_from(cell - first_weekday + 1).unwrap_or(0))
            } else {
                None
            };

            match day_num {
                Some(d) => {
                    let day_str = format!("{d:2}");
                    if d == picker.day && !dim_all {
                        row_spans.push(Span::styled(
                            day_str,
                            Style::default().add_modifier(Modifier::REVERSED),
                        ));
                    } else if dim_all {
                        row_spans.push(Span::styled(day_str, base_dim));
                    } else {
                        row_spans.push(Span::raw(day_str));
                    }
                }
                None => {
                    row_spans.push(Span::raw("  "));
                }
            }

            if col < 6 {
                row_spans.push(Span::raw(" "));
            }
        }
        lines.push(Line::from(row_spans));
    }

    f.render_widget(Paragraph::new(lines), area);
}

fn render_time_side(f: &mut Frame, area: Rect, picker: &DatetimePicker) {
    const SPINNER_ROWS: u16 = 3; // ▲, value, ▼

    let dim = Style::default().add_modifier(Modifier::DIM);
    let bold = Style::default().add_modifier(Modifier::BOLD);

    let in_time = picker.mode == DatetimePickerMode::Time;

    let (hr_style, min_style) = if in_time {
        match picker.time_focus {
            TimeFocus::Hour => (bold, dim),
            TimeFocus::Minute => (dim, bold),
        }
    } else {
        (dim, dim)
    };
    let top_pad = area.height.saturating_sub(SPINNER_ROWS) / 2;
    let mut lines: Vec<Line> = vec![Line::from(""); top_pad as usize];

    lines.push(Line::from(vec![
        Span::styled(" ▲", hr_style),
        Span::raw("   "),
        Span::styled("▲", min_style),
    ]));
    lines.push(Line::from(vec![
        Span::styled(format!("{:02}", picker.hour), hr_style),
        Span::styled(" : ", dim),
        Span::styled(format!("{:02}", picker.minute), min_style),
    ]));
    lines.push(Line::from(vec![
        Span::styled(" ▼", hr_style),
        Span::raw("   "),
        Span::styled("▼", min_style),
    ]));

    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), area);
}

fn render_bottom_datetime(f: &mut Frame, area: Rect, picker: &DatetimePicker) {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let rev = Style::default().add_modifier(Modifier::REVERSED);

    let date_str = format!(
        "{}-{:02}-{:02}",
        picker.view_year, picker.view_month, picker.day
    );
    let hour_str = format!("{:02}", picker.hour);
    let min_str = format!("{:02}", picker.minute);
    let tz_str = format_tz_offset(picker.tz);

    let spans: Vec<Span> = match picker.mode {
        DatetimePickerMode::Date => vec![
            Span::styled(date_str, rev),
            Span::styled(format!("  {hour_str}:{min_str}  {tz_str}"), dim),
        ],
        DatetimePickerMode::Time => match picker.time_focus {
            TimeFocus::Hour => vec![
                Span::styled(date_str, dim),
                Span::raw("  "),
                Span::styled(hour_str, rev),
                Span::styled(format!(":{min_str}  {tz_str}"), dim),
            ],
            TimeFocus::Minute => vec![
                Span::styled(date_str, dim),
                Span::styled(format!("  {hour_str}:"), dim),
                Span::styled(min_str, rev),
                Span::styled(format!("  {tz_str}"), dim),
            ],
        },
    };

    f.render_widget(
        Paragraph::new(Line::from(spans)).alignment(Alignment::Center),
        area,
    );
}

fn footer_title(picker: &DatetimePicker) -> Line<'static> {
    let blue = Style::default().fg(Color::Blue);
    let magenta = Style::default().fg(Color::Magenta);
    let dim = Style::default().add_modifier(Modifier::DIM);
    let right_active =
        !(picker.mode == DatetimePickerMode::Time && picker.time_focus == TimeFocus::Minute);

    Line::from(vec![
        Span::raw("┤ "),
        Span::styled("↵", blue),
        Span::raw(" | "),
        Span::styled("←↕", blue),
        if right_active {
            Span::styled("→", blue)
        } else {
            Span::styled("→", dim)
        },
        Span::raw(" | "),
        if picker.mode == DatetimePickerMode::Date {
            Span::styled("q", magenta)
        } else {
            Span::styled("q", blue)
        },
        Span::raw(" ├──"),
    ])
    .alignment(Alignment::Right)
}

fn format_tz_offset(tz: FixedOffset) -> String {
    let secs = tz.local_minus_utc();
    let sign = if secs >= 0 { '+' } else { '-' };
    let abs_secs = secs.unsigned_abs();
    let h = abs_secs / 3600;
    let m = (abs_secs % 3600) / 60;
    format!("{sign}{h:02}:{m:02}")
}

const fn month_name(month: u32) -> &'static str {
    match month {
        1 => "January",
        2 => "February",
        3 => "March",
        4 => "April",
        5 => "May",
        6 => "June",
        7 => "July",
        8 => "August",
        9 => "September",
        10 => "October",
        11 => "November",
        12 => "December",
        _ => "?",
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
