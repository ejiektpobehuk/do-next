//! Backlog-tab rank reordering: the pure move computation and the debounced
//! pending mutation that `dispatch_background_tasks` sends to Jira.

use std::time::Instant;

use crate::jira::types::RankAnchor;

/// A rank mutation waiting for its debounce window to close. Self-contained
/// (anchor and rank field captured at keypress) so it can be dispatched even
/// after the user switches tabs or teams.
#[derive(Debug, Clone)]
pub struct PendingRank {
    pub source_id: String,
    pub issue_key: String,
    pub anchor: RankAnchor,
    pub rank_field_id: Option<u64>,
    pub last_move_at: Instant,
}

/// Compute a one-step rank move of `key` within `keys` (the backlog's full
/// rank order). Returns the index `key` moves to and the Jira rank anchor
/// (`Before` the item above when moving up, `After` the item below when
/// moving down); `None` when the move falls off either end or `key` is
/// absent.
pub fn compute_rank_move(keys: &[String], key: &str, up: bool) -> Option<(usize, RankAnchor)> {
    let idx = keys.iter().position(|k| k == key)?;
    if up {
        let target = idx.checked_sub(1)?;
        Some((target, RankAnchor::Before(keys[target].clone())))
    } else {
        let target = idx + 1;
        if target >= keys.len() {
            return None;
        }
        Some((target, RankAnchor::After(keys[target].clone())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(v: &[&str]) -> Vec<String> {
        v.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn middle_item_moves_both_ways() {
        let k = keys(&["A-1", "A-2", "A-3"]);
        assert_eq!(
            compute_rank_move(&k, "A-2", true),
            Some((0, RankAnchor::Before("A-1".into())))
        );
        assert_eq!(
            compute_rank_move(&k, "A-2", false),
            Some((2, RankAnchor::After("A-3".into())))
        );
    }

    #[test]
    fn edges_do_not_move_past_the_ends() {
        let k = keys(&["A-1", "A-2", "A-3"]);
        assert_eq!(compute_rank_move(&k, "A-1", true), None);
        assert_eq!(compute_rank_move(&k, "A-3", false), None);
    }

    #[test]
    fn single_item_never_moves() {
        let k = keys(&["A-1"]);
        assert_eq!(compute_rank_move(&k, "A-1", true), None);
        assert_eq!(compute_rank_move(&k, "A-1", false), None);
    }

    #[test]
    fn unknown_key_is_a_no_op() {
        let k = keys(&["A-1", "A-2"]);
        assert_eq!(compute_rank_move(&k, "B-9", true), None);
        assert_eq!(compute_rank_move(&k, "B-9", false), None);
    }
}
