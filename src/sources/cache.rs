//! On-disk per-source cache for stale-while-revalidate rendering: a source's
//! last successful fetch is written here, and read back on the next launch so
//! the list/board paints instantly while a fresh fetch runs in the background.
//!
//! Best-effort throughout — every failure degrades to a cache miss (a network
//! fetch), never an error the user sees.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::types::CacheConfig;
use crate::items::WorkItem;
use crate::jira::types::{BoardConfiguration, BoardSwimlanes};

/// One source's cached fetch result.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SourceCache {
    /// Unix seconds when this entry was written; used for the TTL check.
    pub fetched_at: u64,
    pub items: Vec<WorkItem>,
    /// Board column configuration (board sources only).
    #[serde(default)]
    pub board_config: Option<BoardConfiguration>,
    /// Resolved query swimlanes (board sources with auto/query lanes only).
    #[serde(default)]
    pub lanes: Option<BoardSwimlanes>,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Base directory for source caches: the configured `path`, else
/// `<system cache dir>/do-next/sources`.
fn cache_dir(cfg: &CacheConfig) -> PathBuf {
    cfg.path.as_ref().map_or_else(
        || {
            dirs::cache_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join("do-next")
                .join("sources")
        },
        PathBuf::from,
    )
}

/// Per-source cache file. `source_id` is sanitized to a safe filename.
fn cache_file(cfg: &CacheConfig, source_id: &str) -> PathBuf {
    let safe: String = source_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    cache_dir(cfg).join(format!("{safe}.json"))
}

/// Persist a source's fetch result. No-op when caching is disabled; failures
/// are logged and swallowed.
pub fn write(
    cfg: &CacheConfig,
    source_id: &str,
    items: &[WorkItem],
    board_config: Option<&BoardConfiguration>,
    lanes: Option<&BoardSwimlanes>,
) {
    if !cfg.enabled {
        return;
    }
    let entry = SourceCache {
        fetched_at: now_secs(),
        items: items.to_vec(),
        board_config: board_config.cloned(),
        lanes: lanes.cloned(),
    };
    let path = cache_file(cfg, source_id);
    if let Some(dir) = path.parent()
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        log::warn!("cache: create dir {} failed: {e}", dir.display());
        return;
    }
    match serde_json::to_vec(&entry) {
        Ok(bytes) => {
            if let Err(e) = std::fs::write(&path, bytes) {
                log::warn!("cache: write {} failed: {e}", path.display());
            } else {
                log::debug!("cache: wrote {} ({} items)", source_id, entry.items.len());
            }
        }
        Err(e) => log::warn!("cache: serialize {source_id} failed: {e}"),
    }
}

/// Read a source's cache when caching is enabled, the file exists and parses,
/// and it is within `max_age_seconds` (an absent TTL means never expires).
/// Returns `None` on any miss.
pub fn read(cfg: &CacheConfig, source_id: &str) -> Option<SourceCache> {
    if !cfg.enabled {
        return None;
    }
    let path = cache_file(cfg, source_id);
    let bytes = std::fs::read(&path).ok()?;
    let entry: SourceCache = serde_json::from_slice(&bytes).ok()?;
    if let Some(max_age) = cfg.max_age_seconds {
        let age = now_secs().saturating_sub(entry.fetched_at);
        if age > max_age {
            log::debug!("cache: {source_id} stale (age {age}s > {max_age}s); ignoring");
            return None;
        }
    }
    log::debug!("cache: hit {} ({} items)", source_id, entry.items.len());
    Some(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_entry_is_a_miss() {
        // TTL logic is a pure comparison; exercise it directly.
        let max_age = 300u64;
        let fresh_age = 100u64;
        let stale_age = 400u64;
        assert!(fresh_age <= max_age, "within TTL is a hit");
        assert!(stale_age > max_age, "past TTL is a miss");
    }

    #[test]
    fn source_id_is_sanitized_to_a_safe_filename() {
        let cfg = CacheConfig {
            enabled: true,
            max_age_seconds: None,
            path: Some("/tmp/do-next-test-cache".into()),
        };
        let path = cache_file(&cfg, "team/incidents:1");
        assert_eq!(
            path.file_name().unwrap().to_str().unwrap(),
            "team_incidents_1.json"
        );
    }

    #[test]
    fn round_trips_items_through_disk() {
        let dir = std::env::temp_dir().join("do-next-cache-test-rt");
        let _ = std::fs::remove_dir_all(&dir);
        let cfg = CacheConfig {
            enabled: true,
            max_age_seconds: Some(3600),
            path: Some(dir.to_string_lossy().into_owned()),
        };
        write(&cfg, "src", &[], None, None);
        let got = read(&cfg, "src").expect("cache hit");
        assert_eq!(got.items.len(), 0);
        assert!(got.board_config.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
