//! Daily standup mode: what you did since the previous standup.
//!
//! "Done" here is not a status. It is any trace you left on a work item inside
//! the window — a transition, a comment, a field edit, a logged worklog, an
//! issue you filed, a merge request you opened, a page you edited. Moving
//! something to Done is one entry kind among many, which is what makes the
//! screen useful on days when nothing shipped.
//!
//! Layout:
//! - [`window`] — the pure "since the previous standup" time arithmetic.
//! - [`types`] — [`types::StandupData`] and the flat entry list the screen renders.
//! - [`jql`] — the Jira discovery queries, which are supersets by design.
//! - [`derive`] — pure payload → entry attribution rules.
//! - [`collect`] — the per-backend I/O that feeds `derive`.
//! - [`group`] — the day → item pivot, shared by the screen and the digest.
//! - [`digest`] — the pasteable markdown summary.

pub mod collect;
pub mod derive;
pub mod digest;
pub mod group;
pub mod jql;
pub mod types;
pub mod window;
