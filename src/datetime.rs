//! Shared timestamp parsing and timezone resolution.
//!
//! Jira hands out timestamps as `2026-07-01T10:00:00.000+0300` — an offset with
//! no colon, which RFC 3339 rejects — so every consumer needs the same two-step
//! parse. Keeping it in one place stops a second, subtly different parser from
//! appearing next to the first.

use chrono::{DateTime, FixedOffset, Local, Offset, TimeZone, Utc};

/// Parse a timestamp as returned by Jira, Confluence or GitLab.
///
/// Tries RFC 3339 first (`…+03:00`, `…Z` — what Confluence and GitLab emit),
/// then Jira's colon-less offset form.
pub fn parse_dt(s: &str) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(s)
        .or_else(|_| DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.3f%z"))
        .ok()
}

/// Parse a config offset string: `"+03"`, `"-0530"`, `"+05:45"`, `"03"`.
pub fn parse_tz_offset(s: &str) -> Option<FixedOffset> {
    let s = s.trim();
    let sign: i32 = if s.starts_with('-') { -1 } else { 1 };
    // Colons are stripped so `"+05:45"` parses; without this it silently read
    // as +05:00, since the minute parse failed and fell back to zero.
    let digits: String = s.trim_start_matches(['+', '-']).replace(':', "");
    let h: i32 = digits.get(..2)?.parse().ok()?;
    let m: i32 = digits.get(2..).and_then(|x| x.parse().ok()).unwrap_or(0);
    FixedOffset::east_opt(sign * (h * 3600 + m * 60))
}

/// The system's current UTC offset, collapsed to a fixed value.
///
/// Correct for *formatting* an instant. When *constructing* a wall-clock time in
/// the past or future, use [`TzSpec::Local`] instead — a fixed offset is
/// DST-blind, so a summer offset applied to a winter date is an hour out.
pub fn local_tz() -> FixedOffset {
    let secs = Local::now().offset().local_minus_utc();
    FixedOffset::east_opt(secs)
        .unwrap_or_else(|| FixedOffset::east_opt(0).expect("UTC offset 0 is always valid"))
}

/// Which timezone wall-clock arithmetic happens in.
///
/// `Local` goes through the OS timezone database and so handles DST correctly;
/// it is the default. `Fixed` exists because config has always accepted an
/// offset string, and is DST-naive by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TzSpec {
    #[default]
    Local,
    Fixed(FixedOffset),
}

impl TzSpec {
    /// Resolve a config timezone string. An absent or unparseable value falls
    /// back to the system timezone.
    pub fn from_config(s: Option<&str>) -> Self {
        s.and_then(parse_tz_offset).map_or(Self::Local, Self::Fixed)
    }

    /// The offset in effect at `at` — for formatting an instant for display.
    pub fn offset_at(self, at: DateTime<Utc>) -> FixedOffset {
        match self {
            Self::Local => Local.offset_from_utc_datetime(&at.naive_utc()).fix(),
            Self::Fixed(off) => off,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_jira_colonless_offset() {
        let dt = parse_dt("2026-07-01T10:00:00.000+0300").expect("jira form");
        assert_eq!(dt.to_rfc3339(), "2026-07-01T10:00:00+03:00");
    }

    #[test]
    fn parses_rfc3339_forms() {
        assert!(parse_dt("2026-07-01T10:00:00Z").is_some());
        assert!(parse_dt("2026-07-01T10:00:00.123+03:00").is_some());
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_dt("").is_none());
        assert!(parse_dt("2026-07-01").is_none());
    }

    #[test]
    fn parses_offset_strings() {
        assert_eq!(parse_tz_offset("+03"), FixedOffset::east_opt(3 * 3600));
        assert_eq!(parse_tz_offset("03"), FixedOffset::east_opt(3 * 3600));
        assert_eq!(
            parse_tz_offset("-0530"),
            FixedOffset::east_opt(-(5 * 3600 + 1800))
        );
        assert_eq!(
            parse_tz_offset("+05:45"),
            FixedOffset::east_opt(5 * 3600 + 45 * 60)
        );
        assert_eq!(parse_tz_offset("nonsense"), None);
    }

    #[test]
    fn tz_spec_from_config() {
        assert_eq!(TzSpec::from_config(None), TzSpec::Local);
        assert_eq!(TzSpec::from_config(Some("bogus")), TzSpec::Local);
        assert_eq!(
            TzSpec::from_config(Some("+03")),
            TzSpec::Fixed(FixedOffset::east_opt(3 * 3600).expect("valid"))
        );
    }
}
