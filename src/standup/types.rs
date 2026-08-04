//! The standup data model.
//!
//! Deliberately *not* a fourth [`WorkItem`] variant. `WorkItem::key()` is the
//! dedup / hide-for-a-day / selection-restore identity, and one issue produces
//! many timeline entries — synthetic per-entry keys would then have to be parsed
//! back apart to group by day. A new variant would also flip every `supports_*`
//! capability gate to false and force an exhaustive-match sweep through the list,
//! detail, board and search renderers, for a feature whose UI is its own screen.
//!
//! So: entries are their own flat type, and the real [`WorkItem`] payloads ride
//! alongside in [`StandupData::items`] so pressing Enter on a row can open the
//! existing detail view with full capabilities.

use chrono::{DateTime, Utc};

use crate::items::WorkItem;
use crate::standup::window::Window;

/// Which backend an entry came from. Drives the row symbol and tells the screen
/// whether to expect a [`WorkItem`] payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Backend {
    Jira,
    Gitlab,
    ConfluenceTask,
    ConfluencePage,
}

impl Backend {
    /// Single-char list indicator, in the spirit of `SourceIndication::symbol`.
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Jira => "◆",
            Self::Gitlab => "⑃",
            Self::ConfluenceTask => "☑",
            Self::ConfluencePage => "▤",
        }
    }

    /// Label for the digest's per-backend grouping and for error rows.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Jira => "Jira",
            Self::Gitlab => "GitLab",
            Self::ConfluenceTask => "Confluence tasks",
            Self::ConfluencePage => "Confluence pages",
        }
    }
}

/// The thing an entry happened to. Cheap to clone and repeated across every
/// entry for that item, so it carries only what a row needs to draw and open.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ItemRef {
    /// Matches `WorkItem::key()` when a payload exists, so the screen can find
    /// it in [`StandupData::items`].
    pub key: String,
    pub title: String,
    pub url: String,
    pub backend: Backend,
}

/// What you did. The variants carry enough to render a row without re-reading
/// the source payload.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EntryKind {
    /// You filed the issue. Synthesized from `fields.creator`/`fields.created`,
    /// because Jira writes no changelog entry for creation.
    Created,
    Transition {
        from: String,
        to: String,
    },
    FieldChange {
        field: String,
        from: Option<String>,
        to: Option<String>,
    },
    Comment {
        id: String,
        /// True when matched via `updateAuthor` rather than original authorship.
        edited: bool,
    },
    Worklog {
        seconds: i64,
        /// When the work happened, as opposed to when it was typed in. Entries
        /// are placed on this day.
        started: DateTime<Utc>,
    },
    MrOpened,
    MrMerged,
    MrClosed,
    /// The merge request changed, but not provably *by you* — a teammate's
    /// comment also bumps `updated_at`. Rendered as low confidence.
    MrTouched,
    PageCreated {
        version: u32,
    },
    PageUpdated {
        version: u32,
    },
    TaskCompleted,
    ProjectCreated,
}

impl EntryKind {
    /// Short verb for the row prefix and the digest bullet.
    pub const fn verb(&self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Transition { .. } => "moved",
            Self::FieldChange { .. } => "edited",
            Self::Comment { edited: false, .. } => "commented",
            Self::Comment { edited: true, .. } => "edited comment",
            Self::Worklog { .. } => "logged",
            Self::MrOpened => "opened MR",
            Self::MrMerged => "merged MR",
            Self::MrClosed => "closed MR",
            Self::MrTouched => "touched MR",
            Self::PageCreated { .. } => "created page",
            Self::PageUpdated { .. } => "updated page",
            Self::TaskCompleted => "completed",
            Self::ProjectCreated => "created project",
        }
    }

    /// Whether the entry provably records an action by the current user.
    /// Only [`Self::MrTouched`] is inferred rather than attributed.
    pub const fn is_confident(&self) -> bool {
        !matches!(self, Self::MrTouched)
    }
}

/// One thing you did, at one instant.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StandupEntry {
    /// When it happened. Entries are grouped onto days by this, in local time.
    pub at: DateTime<Utc>,
    pub item: ItemRef,
    pub kind: EntryKind,
    /// One-line human summary, pre-rendered by the collector so the screen and
    /// the digest agree on wording.
    pub detail: String,
}

/// A collected standup.
///
/// `entries` is flat and sorted by `at`; day → item is a pure grouping over it at
/// render time. That is what lets one issue appear under both Friday and Monday.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct StandupData {
    /// The window actually fetched, which may be wider than the one displayed.
    /// Stamped into the cache so a narrower display window can be served by
    /// local filtering instead of a refetch.
    pub coverage: Option<Window>,
    pub entries: Vec<StandupEntry>,
    /// Real payloads for the items that have one, so Enter opens the existing
    /// detail view. Confluence pages have no `WorkItem` and are absent here.
    pub items: Vec<WorkItem>,
    /// Backends that degraded to a less precise path (currently only the
    /// Confluence pages fallback). Surfaced in the header.
    pub degraded: Vec<Backend>,
}

impl StandupData {
    /// Entries inside `window`, sorted by time. Used by both the screen and the
    /// digest so they can never disagree.
    pub fn entries_in(&self, window: &Window) -> Vec<&StandupEntry> {
        self.entries
            .iter()
            .filter(|e| window.contains_instant(e.at))
            .collect()
    }

    /// Sort entries by time and drop exact duplicates. Collectors may overlap
    /// (a transition found via both `updatedBy` and `status CHANGED BY`), so
    /// this runs once before the data is published.
    pub fn normalize(&mut self) {
        self.entries.sort_by(|a, b| {
            a.at.cmp(&b.at)
                .then_with(|| a.item.key.cmp(&b.item.key))
                .then_with(|| a.detail.cmp(&b.detail))
        });
        self.entries
            .dedup_by(|a, b| a.at == b.at && a.item.key == b.item.key && a.kind == b.kind);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0)
            .single()
            .expect("valid")
    }

    fn entry(at: DateTime<Utc>, key: &str, kind: EntryKind) -> StandupEntry {
        StandupEntry {
            at,
            item: ItemRef {
                key: key.to_owned(),
                title: format!("{key} title"),
                url: format!("https://example.test/{key}"),
                backend: Backend::Jira,
            },
            kind,
            detail: String::new(),
        }
    }

    #[test]
    fn normalize_sorts_and_dedups() {
        let mut data = StandupData {
            entries: vec![
                entry(utc(2026, 8, 3, 12, 0), "B-2", EntryKind::Created),
                entry(utc(2026, 8, 3, 9, 0), "A-1", EntryKind::Created),
                // exact duplicate of the first
                entry(utc(2026, 8, 3, 12, 0), "B-2", EntryKind::Created),
            ],
            ..StandupData::default()
        };
        data.normalize();
        assert_eq!(data.entries.len(), 2);
        assert_eq!(data.entries[0].item.key, "A-1");
        assert_eq!(data.entries[1].item.key, "B-2");
    }

    #[test]
    fn normalize_keeps_distinct_kinds_at_the_same_instant() {
        let at = utc(2026, 8, 3, 12, 0);
        let mut data = StandupData {
            entries: vec![
                entry(at, "A-1", EntryKind::Created),
                entry(
                    at,
                    "A-1",
                    EntryKind::Transition {
                        from: "To Do".into(),
                        to: "In Progress".into(),
                    },
                ),
            ],
            ..StandupData::default()
        };
        data.normalize();
        assert_eq!(data.entries.len(), 2);
    }

    #[test]
    fn entries_in_filters_to_the_display_window() {
        let data = StandupData {
            entries: vec![
                entry(utc(2026, 8, 1, 9, 0), "OLD-1", EntryKind::Created),
                entry(utc(2026, 8, 3, 9, 0), "NEW-1", EntryKind::Created),
            ],
            ..StandupData::default()
        };
        let window = Window {
            start: utc(2026, 8, 2, 0, 0),
            end: utc(2026, 8, 4, 0, 0),
        };
        let got = data.entries_in(&window);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].item.key, "NEW-1");
    }

    #[test]
    fn mr_touched_is_the_only_low_confidence_kind() {
        assert!(!EntryKind::MrTouched.is_confident());
        assert!(EntryKind::Created.is_confident());
        assert!(EntryKind::TaskCompleted.is_confident());
    }
}
