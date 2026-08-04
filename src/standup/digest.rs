//! The markdown digest — a standup you can paste into Slack.
//!
//! [`to_markdown`] is pure; only [`write_to_file`] touches the filesystem, so the
//! wording is testable against a fixture.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::NaiveDate;

use crate::datetime::TzSpec;
use crate::standup::group;
use crate::standup::types::{EntryKind, StandupData, StandupEntry};
use crate::standup::window::Window;

/// Render the standup as markdown, grouped by day then item.
pub fn to_markdown(data: &StandupData, window: &Window, tz: TzSpec) -> String {
    let entries = data.entries_in(window);
    let days = group::by_day(&entries, tz);

    let mut out = String::new();
    let start_local = window.start.with_timezone(&tz.offset_at(window.start));
    let _ = writeln!(
        out,
        "# Standup — since {}",
        start_local.format("%a %-d %b %H:%M")
    );

    if !data.degraded.is_empty() {
        let names: Vec<&str> = data.degraded.iter().map(|b| b.label()).collect();
        let _ = writeln!(
            out,
            "\n> Reduced accuracy: {} (some activity may be missing)",
            names.join(", ")
        );
    }

    if days.is_empty() {
        out.push_str("\nNothing recorded in this window.\n");
        return out;
    }

    for day in &days {
        let _ = writeln!(out, "\n## {}", day.date.format("%A %-d %B"));
        for group in &day.items {
            let _ = writeln!(
                out,
                "\n### {} — {}",
                link(&group.item.key, &group.item.url),
                group.item.title
            );
            for entry in &group.entries {
                let at = entry.at.with_timezone(&tz.offset_at(entry.at));
                let mut line = format!("- {} {}", at.format("%H:%M"), describe(entry));
                if !entry.kind.is_confident() {
                    line.push_str(" _(not confirmed as your change)_");
                }
                out.push_str(&line);
                out.push('\n');
            }
        }
    }
    out
}

/// A markdown link, or bare text when the item has no URL (event-sourced rows).
fn link(text: &str, url: &str) -> String {
    if url.is_empty() {
        text.to_owned()
    } else {
        format!("[{text}]({url})")
    }
}

/// The whole bullet phrase for one entry.
///
/// Takes the entry rather than just the kind because `detail` already carries the
/// specifics for some kinds and is redundant for others — the item heading above
/// the bullet repeats a merge request's reference, for instance. Composing the
/// phrase in one place is what stops "moved Ready to test → Done — Ready to test
/// → Done".
///
/// Note `detail` is the *truncated* rendering of a field's new value, so long
/// bodies (descriptions, custom text) are used from here rather than from
/// `EntryKind::FieldChange::to`, which holds the untruncated original.
fn describe(entry: &StandupEntry) -> String {
    let detail = entry.detail.trim();
    // "verb — prose", for details that read as their own clause.
    let with_prose = |verb: &str| {
        if detail.is_empty() {
            verb.to_owned()
        } else {
            format!("{verb} — {detail}")
        }
    };

    match &entry.kind {
        // Detail is the issue type.
        EntryKind::Created => {
            if detail.is_empty() {
                "created".to_owned()
            } else {
                format!("created ({detail})")
            }
        }
        // Detail is "A → B" / "field: value" — the verb just prefixes it. Both
        // fall back to the kind's own data so a missing detail cannot leave a
        // dangling verb.
        EntryKind::Transition { from, to } => {
            if detail.is_empty() {
                format!("moved {from} → {to}")
            } else {
                format!("moved {detail}")
            }
        }
        EntryKind::FieldChange { field, .. } => {
            if detail.is_empty() {
                format!("edited {field}")
            } else {
                format!("edited {detail}")
            }
        }
        EntryKind::Comment { edited: false, .. } => with_prose("commented"),
        EntryKind::Comment { edited: true, .. } => with_prose("edited a comment"),
        // The duration lives in the kind; detail is the same value.
        EntryKind::Worklog { seconds, .. } => {
            format!("logged {}", crate::standup::derive::fmt_duration(*seconds))
        }
        // Detail is the merge request's reference, already in the item heading.
        EntryKind::MrOpened => "opened a merge request".to_owned(),
        EntryKind::MrMerged => "merged a merge request".to_owned(),
        EntryKind::MrClosed => "closed a merge request".to_owned(),
        EntryKind::MrTouched => "updated a merge request".to_owned(),
        EntryKind::PageCreated { .. } => with_prose("created a page"),
        EntryKind::PageUpdated { version } => with_prose(&format!("updated a page (v{version})")),
        // Detail is the page the task lives on.
        EntryKind::TaskCompleted => with_prose("completed a task"),
        EntryKind::ProjectCreated => with_prose("created a project"),
    }
}

/// Filename for a given day's digest. Stable within a day so repeated presses
/// overwrite rather than litter.
pub fn file_name(today: NaiveDate) -> String {
    format!("do-next-standup-{}.md", today.format("%Y-%m-%d"))
}

/// Write the digest into `dir` and return the path.
pub fn write_to_file(dir: &Path, today: NaiveDate, markdown: &str) -> Result<PathBuf> {
    let path = dir.join(file_name(today));
    std::fs::write(&path, markdown)
        .with_context(|| format!("Failed to write the standup digest to {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::standup::types::{Backend, ItemRef};
    use chrono::{DateTime, FixedOffset, TimeZone, Utc};

    fn tz() -> TzSpec {
        TzSpec::Fixed(FixedOffset::east_opt(0).expect("valid"))
    }

    /// Friday 2026-07-31.
    fn fri(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 31, h, m, 0)
            .single()
            .expect("valid")
    }

    /// Monday 2026-08-03 — the next standup day, so the weekend is spanned.
    fn mon(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 3, h, m, 0)
            .single()
            .expect("valid")
    }

    fn entry(at: DateTime<Utc>, key: &str, kind: EntryKind, detail: &str) -> StandupEntry {
        StandupEntry {
            at,
            item: ItemRef {
                key: key.to_owned(),
                title: format!("{key} summary"),
                url: format!("https://jira.test/browse/{key}"),
                backend: Backend::Jira,
            },
            kind,
            detail: detail.to_owned(),
        }
    }

    fn fixture() -> (StandupData, Window) {
        let window = Window {
            start: fri(10, 0),
            end: mon(12, 0),
        };
        let data = StandupData {
            coverage: Some(window),
            entries: vec![
                entry(
                    fri(11, 2),
                    "PROJ-42",
                    EntryKind::Transition {
                        from: "To Do".into(),
                        to: "In Progress".into(),
                    },
                    "To Do → In Progress",
                ),
                entry(
                    fri(14, 20),
                    "PROJ-42",
                    EntryKind::Comment {
                        id: "1".into(),
                        edited: false,
                    },
                    "repro'd on staging",
                ),
                entry(
                    mon(9, 41),
                    "PROJ-42",
                    EntryKind::Transition {
                        from: "In Progress".into(),
                        to: "In Review".into(),
                    },
                    "In Progress → In Review",
                ),
            ],
            items: Vec::new(),
            degraded: Vec::new(),
        };
        (data, window)
    }

    #[test]
    fn groups_by_day_then_item_with_links() {
        let (data, window) = fixture();
        let md = to_markdown(&data, &window, tz());
        assert!(
            md.starts_with("# Standup — since Fri 31 Jul 10:00\n"),
            "{md}"
        );
        assert!(md.contains("## Friday 31 July"), "{md}");
        assert!(md.contains("## Monday 3 August"), "{md}");
        assert!(
            md.contains("### [PROJ-42](https://jira.test/browse/PROJ-42) — PROJ-42 summary"),
            "{md}"
        );
        assert!(md.contains("- 11:02 moved To Do → In Progress"), "{md}");
        assert!(
            md.contains("- 14:20 commented — repro'd on staging"),
            "{md}"
        );
    }

    #[test]
    fn a_transition_states_the_move_exactly_once() {
        // Regression: the bullet used to read
        // "moved To Do → In Progress — To Do → In Progress".
        let (data, window) = fixture();
        let md = to_markdown(&data, &window, tz());
        assert_eq!(md.matches("To Do → In Progress").count(), 1, "{md}");
    }

    #[test]
    fn a_field_change_names_the_field_once() {
        let window = Window {
            start: fri(0, 0),
            end: fri(23, 0),
        };
        let data = StandupData {
            entries: vec![entry(
                fri(11, 34),
                "PROJ-1",
                EntryKind::FieldChange {
                    field: "resolution".into(),
                    from: None,
                    to: Some("Done".into()),
                },
                "resolution: Done",
            )],
            ..StandupData::default()
        };
        let md = to_markdown(&data, &window, tz());
        assert!(md.contains("- 11:34 edited resolution: Done"), "{md}");
        assert_eq!(md.matches("resolution").count(), 1, "{md}");
    }

    #[test]
    fn a_worklog_states_its_duration_once() {
        let window = Window {
            start: fri(0, 0),
            end: fri(23, 0),
        };
        let data = StandupData {
            entries: vec![entry(
                fri(9, 0),
                "PROJ-1",
                EntryKind::Worklog {
                    seconds: 5400,
                    started: fri(9, 0),
                },
                "1h 30m",
            )],
            ..StandupData::default()
        };
        let md = to_markdown(&data, &window, tz());
        assert!(md.contains("- 09:00 logged 1h 30m"), "{md}");
        assert_eq!(md.matches("1h 30m").count(), 1, "{md}");
    }

    #[test]
    fn a_merge_request_bullet_does_not_repeat_the_heading_reference() {
        let window = Window {
            start: fri(0, 0),
            end: fri(23, 0),
        };
        let mut e = entry(fri(16, 45), "MR:api!318", EntryKind::MrMerged, "api!318");
        e.item.backend = Backend::Gitlab;
        let data = StandupData {
            entries: vec![e],
            ..StandupData::default()
        };
        let md = to_markdown(&data, &window, tz());
        // The reference belongs to the heading; the bullet must not echo it.
        let bullet = md
            .lines()
            .find(|l| l.starts_with("- "))
            .expect("a bullet line");
        assert_eq!(bullet, "- 16:45 merged a merge request");
        assert!(!bullet.contains("api!318"), "{bullet}");
    }

    #[test]
    fn a_created_entry_names_the_issue_type() {
        let window = Window {
            start: fri(0, 0),
            end: fri(23, 0),
        };
        let data = StandupData {
            entries: vec![entry(fri(9, 0), "PROJ-1", EntryKind::Created, "Bug")],
            ..StandupData::default()
        };
        let md = to_markdown(&data, &window, tz());
        assert!(md.contains("- 09:00 created (Bug)"), "{md}");
    }

    #[test]
    fn the_same_item_is_listed_under_each_day_it_moved() {
        let (data, window) = fixture();
        let md = to_markdown(&data, &window, tz());
        assert_eq!(md.matches("### [PROJ-42]").count(), 2);
    }

    #[test]
    fn low_confidence_entries_are_flagged() {
        let window = Window {
            start: fri(0, 0),
            end: fri(23, 0),
        };
        let data = StandupData {
            entries: vec![entry(fri(12, 0), "MR:api!7", EntryKind::MrTouched, "api!7")],
            ..StandupData::default()
        };
        let md = to_markdown(&data, &window, tz());
        assert!(md.contains("_(not confirmed as your change)_"), "{md}");
    }

    #[test]
    fn degradation_is_stated_up_front() {
        let window = Window {
            start: fri(0, 0),
            end: fri(23, 0),
        };
        let data = StandupData {
            degraded: vec![Backend::ConfluencePage],
            ..StandupData::default()
        };
        let md = to_markdown(&data, &window, tz());
        assert!(md.contains("> Reduced accuracy: Confluence pages"), "{md}");
    }

    #[test]
    fn an_empty_window_says_so_rather_than_rendering_a_bare_heading() {
        let window = Window {
            start: fri(0, 0),
            end: fri(23, 0),
        };
        let md = to_markdown(&StandupData::default(), &window, tz());
        assert!(md.contains("Nothing recorded in this window."), "{md}");
    }

    #[test]
    fn entries_outside_the_display_window_are_excluded() {
        let (data, _) = fixture();
        // Narrow to Friday only; Monday's transition must drop out.
        let narrow = Window {
            start: fri(0, 0),
            end: fri(23, 59),
        };
        let md = to_markdown(&data, &narrow, tz());
        assert!(md.contains("## Friday 31 July"), "{md}");
        assert!(!md.contains("## Monday 3 August"), "{md}");
    }

    #[test]
    fn items_without_a_url_render_as_plain_text() {
        assert_eq!(link("MR:42!7", ""), "MR:42!7");
        assert_eq!(
            link("A-1", "https://x.test/A-1"),
            "[A-1](https://x.test/A-1)"
        );
    }

    #[test]
    fn digest_file_name_is_stable_within_a_day() {
        let day = NaiveDate::from_ymd_opt(2026, 8, 3).expect("valid");
        assert_eq!(file_name(day), "do-next-standup-2026-08-03.md");
    }

    #[test]
    fn write_to_file_round_trips() {
        let dir = std::env::temp_dir().join("do-next-digest-test");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let day = NaiveDate::from_ymd_opt(2026, 8, 3).expect("valid");
        let path = write_to_file(&dir, day, "# hello\n").expect("write");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "# hello\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
