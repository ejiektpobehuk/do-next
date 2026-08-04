//! The "since the previous standup" time window.
//!
//! Everything here is pure and takes `now` as a parameter — never `Utc::now()` —
//! so the weekday, grace and DST rules are unit-testable without a clock.
//!
//! The functions are generic over `TimeZone` so tests can use a cheap
//! `FixedOffset` for the ordinary cases and `Local` (or a hand-rolled zone) for
//! the DST edges. [`Window::resolve`] dispatches a [`TzSpec`] onto them.

use chrono::{
    DateTime, Datelike, Duration, MappedLocalTime, NaiveDate, NaiveTime, TimeZone, Utc, Weekday,
};

use crate::datetime::TzSpec;

/// Hard ceiling on how far back the window may reach, however it was widened.
/// Keeps a stray `<` from asking Jira for a year of history.
pub const MAX_WINDOW_DAYS: i64 = 31;

/// How many days back [`nth_previous_occurrence`] will look before giving up.
/// A schedule with a single weekday puts consecutive occurrences 7 days apart,
/// so one skipped occurrence needs 14; the extra week is slack.
const SEARCH_DAYS: i64 = 21;

/// When the standup happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    /// Days the standup happens on. Empty means "no schedule" and makes
    /// [`nth_previous_occurrence`] return `None`.
    pub weekdays: Vec<Weekday>,
    /// Local wall-clock time of the standup.
    pub at: NaiveTime,
    /// How long after an occurrence it still counts as "happening now" and is
    /// therefore skipped when looking backwards.
    ///
    /// Without this, opening the screen at 10:01 with a 10:00 standup yields a
    /// one-minute window — exactly when you need yesterday's work.
    pub grace: Duration,
}

impl Default for Schedule {
    fn default() -> Self {
        Self {
            weekdays: vec![
                Weekday::Mon,
                Weekday::Tue,
                Weekday::Wed,
                Weekday::Thu,
                Weekday::Fri,
            ],
            at: NaiveTime::from_hms_opt(10, 0, 0).expect("10:00 is a valid time"),
            grace: Duration::hours(4),
        }
    }
}

impl Schedule {
    /// Build a schedule from raw config values.
    ///
    /// Shared by config validation (so a bad schedule is a load-time error, not
    /// a silently empty standup) and by the collector.
    pub fn from_parts(days: &[String], time: &str, grace_hours: u32) -> anyhow::Result<Self> {
        let weekdays = days
            .iter()
            .map(|d| parse_weekday(d))
            .collect::<anyhow::Result<Vec<_>>>()?;
        if weekdays.is_empty() {
            anyhow::bail!("`schedule.days` must list at least one weekday");
        }
        let at = NaiveTime::parse_from_str(time, "%H:%M")
            .or_else(|_| NaiveTime::parse_from_str(time, "%H:%M:%S"))
            .map_err(|_| anyhow::anyhow!("`schedule.time` must be \"HH:MM\" (got \"{time}\")"))?;
        Ok(Self {
            weekdays,
            at,
            grace: Duration::hours(i64::from(grace_hours)),
        })
    }
}

/// Parse a weekday name: three-letter or full, any case.
fn parse_weekday(s: &str) -> anyhow::Result<Weekday> {
    match s.trim().to_ascii_lowercase().as_str() {
        "mon" | "monday" => Ok(Weekday::Mon),
        "tue" | "tues" | "tuesday" => Ok(Weekday::Tue),
        "wed" | "weds" | "wednesday" => Ok(Weekday::Wed),
        "thu" | "thur" | "thurs" | "thursday" => Ok(Weekday::Thu),
        "fri" | "friday" => Ok(Weekday::Fri),
        "sat" | "saturday" => Ok(Weekday::Sat),
        "sun" | "sunday" => Ok(Weekday::Sun),
        other => anyhow::bail!(
            "`schedule.days` entries must be weekday names like \"mon\" (got \"{other}\")"
        ),
    }
}

/// Where the window starts, relative to the schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shift {
    /// Start at the Nth-previous standup. `0` is the most recent one, i.e. the
    /// default "since the previous standup".
    Occurrences(u32),
    /// Start a fixed number of days before `now`, ignoring the schedule.
    Days(i64),
}

impl Default for Shift {
    fn default() -> Self {
        Self::Occurrences(0)
    }
}

impl Shift {
    /// Reach one standup further back. Presets collapse to occurrence stepping.
    pub const fn widen(self) -> Self {
        match self {
            Self::Occurrences(n) => Self::Occurrences(n.saturating_add(1)),
            // From a preset, `<` re-enters schedule stepping one step out.
            Self::Days(_) => Self::Occurrences(1),
        }
    }

    /// Reach one standup less far back, stopping at the default window.
    pub const fn narrow(self) -> Self {
        match self {
            Self::Occurrences(n) => Self::Occurrences(n.saturating_sub(1)),
            Self::Days(_) => Self::Occurrences(0),
        }
    }
}

/// A resolved, non-empty time range. `end` is always `now`: a standup reports up
/// to the present moment, never up to some past boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Window {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl Window {
    /// Resolve a window for `now` under `tz`.
    ///
    /// Falls back to a 24h lookback when the schedule yields no occurrence
    /// (empty `weekdays`, or a search that ran past [`SEARCH_DAYS`]).
    pub fn resolve(now: DateTime<Utc>, tz: TzSpec, schedule: &Schedule, shift: Shift) -> Self {
        let start = match shift {
            Shift::Occurrences(n) => match tz {
                TzSpec::Local => nth_previous_occurrence(now, &chrono::Local, schedule, n),
                TzSpec::Fixed(off) => nth_previous_occurrence(now, &off, schedule, n),
            }
            .unwrap_or_else(|| now - Duration::days(1)),
            Shift::Days(d) => now - Duration::days(d.max(0)),
        };
        Self::clamped(start, now)
    }

    /// Build a window, enforcing `start <= end` and [`MAX_WINDOW_DAYS`].
    fn clamped(start: DateTime<Utc>, now: DateTime<Utc>) -> Self {
        let floor = now - Duration::days(MAX_WINDOW_DAYS);
        Self {
            start: start.max(floor).min(now),
            end: now,
        }
    }

    /// Can data fetched for this window serve a `display` window? Drives the
    /// "filter locally instead of refetching" fast path.
    ///
    /// Only the start bound is compared. Both windows end at "now", but a
    /// display window is recomputed against the live clock while a coverage
    /// window is frozen at fetch time — so comparing ends would make this
    /// *always* false, and every window step a refetch. Activity newer than the
    /// fetch is simply not in the data yet; surfacing it is what refresh is for,
    /// not what stepping the window is for.
    pub fn covers(&self, display: &Self) -> bool {
        self.start <= display.start
    }

    pub fn contains_instant(&self, at: DateTime<Utc>) -> bool {
        at >= self.start && at <= self.end
    }

    /// Whole days spanned, rounded up — the basis for day-granular API filters.
    pub fn days(&self) -> i64 {
        let secs = (self.end - self.start).num_seconds().max(0);
        (secs + 86_399) / 86_400
    }
}

/// The Nth-previous standup instant, or `None` if the schedule has no weekdays
/// or none was found within [`SEARCH_DAYS`].
///
/// `n = 0` is the most recent occurrence at least `grace` old.
pub fn nth_previous_occurrence<Tz: TimeZone>(
    now: DateTime<Utc>,
    tz: &Tz,
    schedule: &Schedule,
    n: u32,
) -> Option<DateTime<Utc>> {
    if schedule.weekdays.is_empty() {
        return None;
    }

    let today = now.with_timezone(tz).date_naive();
    let mut remaining = n;

    for back in 0..=SEARCH_DAYS {
        let date = today.checked_sub_signed(Duration::days(back))?;
        if !schedule.weekdays.contains(&date.weekday()) {
            continue;
        }
        let Some(instant) = local_instant(tz, date, schedule.at) else {
            continue;
        };
        // Future occurrences, and one still inside its grace period, are not
        // "previous" — the standup they belong to hasn't concluded yet.
        if now - instant < schedule.grace {
            continue;
        }
        if remaining == 0 {
            return Some(instant);
        }
        remaining -= 1;
    }
    None
}

/// Resolve a local wall-clock date+time to a UTC instant, coping with the two
/// ways a DST transition breaks the mapping.
fn local_instant<Tz: TimeZone>(tz: &Tz, date: NaiveDate, at: NaiveTime) -> Option<DateTime<Utc>> {
    match tz.from_local_datetime(&date.and_time(at)) {
        MappedLocalTime::Single(dt) => Some(dt.with_timezone(&Utc)),
        // Fall-back hour: the wall clock reads `at` twice. Take the earlier
        // instant so the window is the wider of the two readings.
        MappedLocalTime::Ambiguous(earlier, _) => Some(earlier.with_timezone(&Utc)),
        // Spring-forward gap: `at` never happens on this date. Step forward to
        // the first wall-clock time that does.
        MappedLocalTime::None => {
            let mut probe = date.and_time(at);
            for _ in 0..16 {
                probe += Duration::minutes(15);
                if let Some(dt) = tz.from_local_datetime(&probe).earliest() {
                    return Some(dt.with_timezone(&Utc));
                }
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    /// +03, no DST — keeps the ordinary cases arithmetic-simple.
    fn msk() -> FixedOffset {
        FixedOffset::east_opt(3 * 3600).expect("valid offset")
    }

    fn at(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).expect("valid time")
    }

    /// A local wall-clock instant in `msk`, as UTC.
    fn msk_at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        msk()
            .with_ymd_and_hms(y, mo, d, h, mi, 0)
            .single()
            .expect("unambiguous")
            .with_timezone(&Utc)
    }

    fn weekdays_schedule() -> Schedule {
        Schedule {
            at: at(10, 0),
            ..Schedule::default()
        }
    }

    // 2026-08-03 is a Monday; 2026-07-31 a Friday.

    #[test]
    fn tuesday_at_standup_reaches_back_to_monday() {
        // 12:00 is inside 10:00 + 4h grace, i.e. "this morning's standup is the
        // one I am preparing for", so the boundary is yesterday's.
        let now = msk_at(2026, 8, 4, 12, 0); // Tue
        let start = nth_previous_occurrence(now, &msk(), &weekdays_schedule(), 0).expect("found");
        assert_eq!(start, msk_at(2026, 8, 3, 10, 0));
    }

    #[test]
    fn monday_at_standup_reaches_back_to_friday_covering_the_weekend() {
        let now = msk_at(2026, 8, 3, 12, 0); // Mon, inside grace
        let start = nth_previous_occurrence(now, &msk(), &weekdays_schedule(), 0).expect("found");
        assert_eq!(start, msk_at(2026, 7, 31, 10, 0)); // Fri
    }

    #[test]
    fn grace_skips_the_standup_happening_right_now() {
        // Tue 10:01, one minute into today's standup. The useful answer is
        // Monday, not a one-minute window.
        let now = msk_at(2026, 8, 4, 10, 1);
        let start = nth_previous_occurrence(now, &msk(), &weekdays_schedule(), 0).expect("found");
        assert_eq!(start, msk_at(2026, 8, 3, 10, 0));
    }

    #[test]
    fn before_todays_standup_also_reaches_back_to_yesterday() {
        let now = msk_at(2026, 8, 4, 9, 50); // Tue, 10 min before standup
        let start = nth_previous_occurrence(now, &msk(), &weekdays_schedule(), 0).expect("found");
        assert_eq!(start, msk_at(2026, 8, 3, 10, 0));
    }

    #[test]
    fn past_grace_todays_standup_becomes_the_boundary() {
        let now = msk_at(2026, 8, 4, 16, 0); // Tue, well past 10:00 + 4h
        let start = nth_previous_occurrence(now, &msk(), &weekdays_schedule(), 0).expect("found");
        assert_eq!(start, msk_at(2026, 8, 4, 10, 0));
    }

    #[test]
    fn stepping_back_walks_occurrences_including_over_the_weekend() {
        let now = msk_at(2026, 8, 4, 12, 0); // Tue, inside grace
        let s = weekdays_schedule();
        assert_eq!(
            nth_previous_occurrence(now, &msk(), &s, 1).expect("found"),
            msk_at(2026, 7, 31, 10, 0) // Fri — skips the weekend
        );
        assert_eq!(
            nth_previous_occurrence(now, &msk(), &s, 2).expect("found"),
            msk_at(2026, 7, 30, 10, 0) // Thu
        );
    }

    #[test]
    fn afternoon_boundary_is_this_mornings_standup_not_yesterdays() {
        // The counterpart to the two "at standup" cases above: once today's
        // standup is past its grace, work reported at it is already reported.
        let now = msk_at(2026, 8, 3, 18, 0); // Mon evening
        let start = nth_previous_occurrence(now, &msk(), &weekdays_schedule(), 0).expect("found");
        assert_eq!(start, msk_at(2026, 8, 3, 10, 0));
    }

    #[test]
    fn single_weekday_schedule_steps_a_week_at_a_time() {
        let s = Schedule {
            weekdays: vec![Weekday::Mon],
            at: at(10, 0),
            grace: Duration::hours(4),
        };
        let now = msk_at(2026, 8, 4, 15, 0); // Tue
        assert_eq!(
            nth_previous_occurrence(now, &msk(), &s, 0).expect("found"),
            msk_at(2026, 8, 3, 10, 0)
        );
        assert_eq!(
            nth_previous_occurrence(now, &msk(), &s, 1).expect("found"),
            msk_at(2026, 7, 27, 10, 0)
        );
    }

    #[test]
    fn empty_weekdays_yields_no_occurrence() {
        let s = Schedule {
            weekdays: vec![],
            ..Schedule::default()
        };
        assert!(nth_previous_occurrence(msk_at(2026, 8, 4, 15, 0), &msk(), &s, 0).is_none());
    }

    #[test]
    fn resolve_falls_back_to_a_day_without_a_schedule() {
        let now = msk_at(2026, 8, 4, 15, 0);
        let s = Schedule {
            weekdays: vec![],
            ..Schedule::default()
        };
        let w = Window::resolve(now, TzSpec::Fixed(msk()), &s, Shift::default());
        assert_eq!(w.start, now - Duration::days(1));
        assert_eq!(w.end, now);
    }

    #[test]
    fn day_and_week_presets() {
        let now = msk_at(2026, 8, 3, 15, 0);
        let s = weekdays_schedule();
        let day = Window::resolve(now, TzSpec::Fixed(msk()), &s, Shift::Days(1));
        assert_eq!(day.start, now - Duration::days(1));
        let week = Window::resolve(now, TzSpec::Fixed(msk()), &s, Shift::Days(7));
        assert_eq!(week.start, now - Duration::days(7));
    }

    #[test]
    fn window_is_clamped_to_max_days_and_never_inverts() {
        let now = msk_at(2026, 8, 3, 15, 0);
        let s = weekdays_schedule();
        let w = Window::resolve(now, TzSpec::Fixed(msk()), &s, Shift::Days(365));
        assert_eq!(w.start, now - Duration::days(MAX_WINDOW_DAYS));
        assert!(w.start <= w.end);

        // A negative preset cannot push start past now.
        let w = Window::resolve(now, TzSpec::Fixed(msk()), &s, Shift::Days(-5));
        assert_eq!(w.start, now);
        assert!(w.start <= w.end);
    }

    #[test]
    fn shift_widen_and_narrow_saturate() {
        assert_eq!(Shift::Occurrences(0).widen(), Shift::Occurrences(1));
        assert_eq!(Shift::Occurrences(0).narrow(), Shift::Occurrences(0));
        assert_eq!(Shift::Days(7).widen(), Shift::Occurrences(1));
        assert_eq!(Shift::Days(7).narrow(), Shift::Occurrences(0));
    }

    #[test]
    fn schedule_from_parts_accepts_weekday_spellings() {
        let s = Schedule::from_parts(
            &[
                "mon".into(),
                "Tuesday".into(),
                "WED".into(),
                "thurs".into(),
                "fri".into(),
            ],
            "10:30",
            4,
        )
        .expect("valid");
        assert_eq!(
            s.weekdays,
            vec![
                Weekday::Mon,
                Weekday::Tue,
                Weekday::Wed,
                Weekday::Thu,
                Weekday::Fri
            ]
        );
        assert_eq!(s.at, at(10, 30));
        assert_eq!(s.grace, Duration::hours(4));
    }

    #[test]
    fn schedule_from_parts_rejects_bad_input() {
        assert!(Schedule::from_parts(&[], "10:00", 4).is_err());
        assert!(Schedule::from_parts(&["funday".into()], "10:00", 4).is_err());
        assert!(Schedule::from_parts(&["mon".into()], "25:00", 4).is_err());
        assert!(Schedule::from_parts(&["mon".into()], "half ten", 4).is_err());
        // Seconds are tolerated, since "10:30:00" is a natural thing to write.
        assert!(Schedule::from_parts(&["mon".into()], "10:30:00", 4).is_ok());
    }

    #[test]
    fn covers_drives_the_local_filter_fast_path() {
        let now = msk_at(2026, 8, 3, 15, 0);
        let wide = Window {
            start: now - Duration::days(7),
            end: now,
        };
        let narrow = Window {
            start: now - Duration::days(1),
            end: now,
        };
        assert!(wide.covers(&narrow), "narrowing must not refetch");
        assert!(!narrow.covers(&wide), "widening must refetch");
    }

    #[test]
    fn covers_ignores_a_display_end_that_has_drifted_past_the_fetch() {
        // Regression: the display window is recomputed against the live clock,
        // so its end is always later than the frozen coverage end. Comparing
        // ends made every window step a refetch, which also blanked the data
        // while it reloaded.
        let fetched_at = msk_at(2026, 8, 3, 15, 0);
        let coverage = Window {
            start: fetched_at - Duration::days(7),
            end: fetched_at,
        };
        let display_a_minute_later = Window {
            start: fetched_at - Duration::days(1),
            end: fetched_at + Duration::minutes(1),
        };
        assert!(coverage.covers(&display_a_minute_later));
    }

    #[test]
    fn days_rounds_up() {
        let now = msk_at(2026, 8, 3, 15, 0);
        let w = Window {
            start: now - Duration::hours(1),
            end: now,
        };
        assert_eq!(w.days(), 1);
        let w = Window {
            start: now - Duration::days(3) - Duration::minutes(1),
            end: now,
        };
        assert_eq!(w.days(), 4);
    }

    // ── DST ──────────────────────────────────────────────────────────────────
    //
    // These use a zone with real transitions. Europe/Berlin springs forward
    // 2026-03-29 02:00→03:00 and falls back 2026-10-25 03:00→02:00. `Local`
    // depends on the host tz, so instead of asserting instants we assert the
    // invariants that matter: a schedule time inside the gap still resolves,
    // and an ambiguous one picks the earlier reading.

    /// Minimal DST zone: +01 winter, +02 summer, switching on the 2026 EU dates.
    #[derive(Clone, Debug)]
    struct Berlin;

    impl TimeZone for Berlin {
        type Offset = FixedOffset;

        fn from_offset(_: &FixedOffset) -> Self {
            Self
        }

        fn offset_from_local_date(&self, _: &NaiveDate) -> MappedLocalTime<FixedOffset> {
            MappedLocalTime::Single(FixedOffset::east_opt(3600).expect("valid"))
        }

        fn offset_from_local_datetime(
            &self,
            local: &chrono::NaiveDateTime,
        ) -> MappedLocalTime<FixedOffset> {
            let winter = FixedOffset::east_opt(3600).expect("valid");
            let summer = FixedOffset::east_opt(2 * 3600).expect("valid");
            let spring_gap_start = NaiveDate::from_ymd_opt(2026, 3, 29)
                .expect("valid")
                .and_hms_opt(2, 0, 0)
                .expect("valid");
            let spring_gap_end = NaiveDate::from_ymd_opt(2026, 3, 29)
                .expect("valid")
                .and_hms_opt(3, 0, 0)
                .expect("valid");
            let fall_amb_start = NaiveDate::from_ymd_opt(2026, 10, 25)
                .expect("valid")
                .and_hms_opt(2, 0, 0)
                .expect("valid");
            let fall_amb_end = NaiveDate::from_ymd_opt(2026, 10, 25)
                .expect("valid")
                .and_hms_opt(3, 0, 0)
                .expect("valid");

            if *local >= spring_gap_start && *local < spring_gap_end {
                MappedLocalTime::None
            } else if *local >= fall_amb_start && *local < fall_amb_end {
                MappedLocalTime::Ambiguous(summer, winter)
            } else if *local >= spring_gap_end && *local < fall_amb_start {
                MappedLocalTime::Single(summer)
            } else {
                MappedLocalTime::Single(winter)
            }
        }

        fn offset_from_utc_date(&self, _: &NaiveDate) -> FixedOffset {
            FixedOffset::east_opt(3600).expect("valid")
        }

        fn offset_from_utc_datetime(&self, utc: &chrono::NaiveDateTime) -> FixedOffset {
            let summer_start = NaiveDate::from_ymd_opt(2026, 3, 29)
                .expect("valid")
                .and_hms_opt(1, 0, 0)
                .expect("valid");
            let summer_end = NaiveDate::from_ymd_opt(2026, 10, 25)
                .expect("valid")
                .and_hms_opt(1, 0, 0)
                .expect("valid");
            if *utc >= summer_start && *utc < summer_end {
                FixedOffset::east_opt(2 * 3600).expect("valid")
            } else {
                FixedOffset::east_opt(3600).expect("valid")
            }
        }
    }

    #[test]
    fn spring_forward_gap_still_yields_an_instant() {
        // A 02:30 standup on the day the clocks skip 02:00→03:00.
        let s = Schedule {
            weekdays: vec![Weekday::Sun],
            at: at(2, 30),
            grace: Duration::minutes(1),
        };
        let now = Berlin
            .with_ymd_and_hms(2026, 3, 29, 12, 0, 0)
            .single()
            .expect("unambiguous")
            .with_timezone(&Utc);
        let start = nth_previous_occurrence(now, &Berlin, &s, 0)
            .expect("gap must resolve forward, not vanish");
        // 03:00 local (+02) == 01:00 UTC.
        assert_eq!(start, Utc.with_ymd_and_hms(2026, 3, 29, 1, 0, 0).unwrap());
    }

    #[test]
    fn fall_back_ambiguity_takes_the_earlier_reading() {
        // A 02:30 standup on the day 02:00→03:00 happens twice.
        let s = Schedule {
            weekdays: vec![Weekday::Sun],
            at: at(2, 30),
            grace: Duration::minutes(1),
        };
        let now = Berlin
            .with_ymd_and_hms(2026, 10, 25, 12, 0, 0)
            .single()
            .expect("unambiguous")
            .with_timezone(&Utc);
        let start = nth_previous_occurrence(now, &Berlin, &s, 0).expect("found");
        // Earlier reading is 02:30 +02 == 00:30 UTC (the later is 01:30 UTC).
        assert_eq!(start, Utc.with_ymd_and_hms(2026, 10, 25, 0, 30, 0).unwrap());
    }

    #[test]
    fn fixed_offset_is_an_hour_out_across_a_dst_boundary() {
        // Documents *why* TzSpec::Local is the default: the same schedule
        // resolved with a summer fixed offset lands an hour off in winter.
        let s = Schedule {
            weekdays: vec![Weekday::Mon],
            at: at(10, 0),
            grace: Duration::hours(4),
        };
        let now = Utc.with_ymd_and_hms(2026, 12, 8, 15, 0, 0).unwrap();
        let summer_offset = FixedOffset::east_opt(2 * 3600).expect("valid");
        let fixed = nth_previous_occurrence(now, &summer_offset, &s, 0).expect("found");
        let real = nth_previous_occurrence(now, &Berlin, &s, 0).expect("found");
        assert_eq!(real - fixed, Duration::hours(1));
    }
}
