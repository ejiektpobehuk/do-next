//! The day → item pivot.
//!
//! Entries are stored flat and sorted by time, because one issue can be worked
//! on across several days and must appear under each. The grouping is therefore
//! derived here rather than baked into the data, and it is a pure function so
//! the screen and the digest cannot drift apart.

use chrono::NaiveDate;

use crate::datetime::TzSpec;
use crate::standup::types::{ItemRef, StandupEntry};

/// One item's entries within one day.
#[derive(Debug)]
pub struct ItemGroup<'a> {
    pub item: &'a ItemRef,
    pub entries: Vec<&'a StandupEntry>,
}

/// One day of activity.
#[derive(Debug)]
pub struct DayGroup<'a> {
    /// The local date, per the standup's timezone — not UTC, or late-evening
    /// work would land on tomorrow.
    pub date: NaiveDate,
    pub items: Vec<ItemGroup<'a>>,
}

impl DayGroup<'_> {
    /// Total entries under this day.
    pub fn entry_count(&self) -> usize {
        self.items.iter().map(|i| i.entries.len()).sum()
    }
}

/// Group entries by local day, then by item, preserving chronological order
/// throughout: days ascending, items in order of first appearance within a day,
/// entries ascending within an item.
///
/// First-appearance ordering (rather than alphabetical by key) is deliberate:
/// reading the standup aloud should follow the order the work happened in.
pub fn by_day<'a>(entries: &[&'a StandupEntry], tz: TzSpec) -> Vec<DayGroup<'a>> {
    let mut days: Vec<DayGroup<'a>> = Vec::new();

    for entry in entries {
        let date = entry.at.with_timezone(&tz.offset_at(entry.at)).date_naive();

        // Found by index rather than by holding a `&mut` across the possible
        // push, which would borrow `days` twice.
        let day_idx = days.iter().position(|d| d.date == date).unwrap_or_else(|| {
            days.push(DayGroup {
                date,
                items: Vec::new(),
            });
            days.len() - 1
        });
        let day = &mut days[day_idx];

        if let Some(group) = day.items.iter_mut().find(|g| g.item.key == entry.item.key) {
            group.entries.push(entry);
        } else {
            day.items.push(ItemGroup {
                item: &entry.item,
                entries: vec![entry],
            });
        }
    }

    // `entries` arrives sorted by time, so days come out ascending already;
    // sorting is belt and braces for callers that filter in another order.
    days.sort_by_key(|d| d.date);
    days
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::standup::types::{Backend, EntryKind};
    use chrono::{DateTime, Datelike, FixedOffset, TimeZone, Utc};

    /// Friday 2026-07-31 at `h`.
    fn fri(h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 31, h, 0, 0)
            .single()
            .expect("valid")
    }

    /// Monday 2026-08-03 at `h` — the next standup day after `fri`.
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
                title: format!("{key} title"),
                url: String::new(),
                backend: Backend::Jira,
            },
            kind: EntryKind::Created,
            detail: String::new(),
        }
    }

    #[test]
    fn one_item_worked_on_two_days_appears_under_both() {
        // The reason entries are flat rather than nested under items.
        let entries = vec![
            entry(fri(11), "PROJ-42"),
            entry(fri(14), "PROJ-51"),
            entry(mon(9), "PROJ-42"),
        ];
        let refs: Vec<&StandupEntry> = entries.iter().collect();
        let days = by_day(&refs, TzSpec::Fixed(FixedOffset::east_opt(0).unwrap()));

        assert_eq!(days.len(), 2);
        assert_eq!(days[0].items.len(), 2);
        assert_eq!(days[0].items[0].item.key, "PROJ-42");
        assert_eq!(days[1].items.len(), 1);
        assert_eq!(days[1].items[0].item.key, "PROJ-42");
    }

    #[test]
    fn several_entries_on_one_item_collapse_under_it() {
        let entries = vec![
            entry(fri(11), "PROJ-42"),
            entry(fri(14), "PROJ-42"),
            entry(fri(16), "PROJ-42"),
        ];
        let refs: Vec<&StandupEntry> = entries.iter().collect();
        let days = by_day(&refs, TzSpec::Fixed(FixedOffset::east_opt(0).unwrap()));
        assert_eq!(days.len(), 1);
        assert_eq!(days[0].items.len(), 1);
        assert_eq!(days[0].items[0].entries.len(), 3);
        assert_eq!(days[0].entry_count(), 3);
    }

    #[test]
    fn items_keep_first_appearance_order_not_alphabetical() {
        let entries = vec![entry(fri(11), "ZZ-1"), entry(fri(12), "AA-1")];
        let refs: Vec<&StandupEntry> = entries.iter().collect();
        let days = by_day(&refs, TzSpec::Fixed(FixedOffset::east_opt(0).unwrap()));
        assert_eq!(days[0].items[0].item.key, "ZZ-1");
        assert_eq!(days[0].items[1].item.key, "AA-1");
    }

    #[test]
    fn day_boundaries_follow_the_configured_timezone() {
        // 22:00 UTC on the 30th is 01:00 on the 31st at +03 — the entry must
        // land on the local day, or late-evening work is filed under tomorrow.
        let entries = vec![entry(
            Utc.with_ymd_and_hms(2026, 7, 30, 22, 0, 0)
                .single()
                .unwrap(),
            "PROJ-1",
        )];
        let refs: Vec<&StandupEntry> = entries.iter().collect();

        let utc_days = by_day(&refs, TzSpec::Fixed(FixedOffset::east_opt(0).unwrap()));
        assert_eq!(utc_days[0].date.day(), 30);

        let msk_days = by_day(
            &refs,
            TzSpec::Fixed(FixedOffset::east_opt(3 * 3600).unwrap()),
        );
        assert_eq!(msk_days[0].date.day(), 31);
    }

    #[test]
    fn empty_input_yields_no_days() {
        assert!(by_day(&[], TzSpec::Local).is_empty());
    }
}
