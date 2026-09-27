//! Cache accounting (section 29).
//!
//! The block store owns the bytes; this table owns the bookkeeping that tells
//! the evictor what to drop first. Pinned blocks — anything this node
//! published — are never evicted.

use rusqlite::{params, OptionalExtension};
use serde::Serialize;

use ovn_protocol::ContentId;

use crate::{now_secs, Database, Result};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheEntry {
    pub cid: String,
    pub size_bytes: i64,
    pub added_at: i64,
    pub last_accessed: i64,
    pub pinned: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheSummary {
    pub block_count: i64,
    pub total_bytes: i64,
    pub pinned_bytes: i64,
}

impl Database {
    /// Note that a block is held. Re-recording an existing block refreshes its
    /// access time but keeps its original `added_at` and pin state.
    pub fn record_cached(&self, cid: &ContentId, size_bytes: u64, pinned: bool) -> Result<()> {
        let now = now_secs();
        self.conn()?.execute(
            "INSERT INTO content_cache (cid, size_bytes, added_at, last_accessed, pinned)
             VALUES (?1, ?2, ?3, ?3, ?4)
             ON CONFLICT(cid) DO UPDATE SET
                 size_bytes    = ?2,
                 last_accessed = ?3,
                 pinned        = MAX(content_cache.pinned, ?4)",
            params![cid.to_string(), size_bytes as i64, now, i64::from(pinned)],
        )?;
        Ok(())
    }

    pub fn touch_cached(&self, cid: &ContentId) -> Result<()> {
        self.conn()?.execute(
            "UPDATE content_cache SET last_accessed = ?2 WHERE cid = ?1",
            params![cid.to_string(), now_secs()],
        )?;
        Ok(())
    }

    pub fn set_pinned(&self, cid: &ContentId, pinned: bool) -> Result<()> {
        self.conn()?.execute(
            "UPDATE content_cache SET pinned = ?2 WHERE cid = ?1",
            params![cid.to_string(), i64::from(pinned)],
        )?;
        Ok(())
    }

    pub fn forget_cached(&self, cid: &ContentId) -> Result<bool> {
        self.forget_cached_raw(&cid.to_string())
    }

    /// Drop a row by its stored text id, for rows whose id no longer parses.
    pub fn forget_cached_raw(&self, cid: &str) -> Result<bool> {
        let changed = self
            .conn()?
            .execute("DELETE FROM content_cache WHERE cid = ?1", params![cid])?;
        Ok(changed > 0)
    }

    pub fn cache_entry(&self, cid: &ContentId) -> Result<Option<CacheEntry>> {
        Ok(self
            .conn()?
            .query_row(
                "SELECT cid, size_bytes, added_at, last_accessed, pinned
                   FROM content_cache WHERE cid = ?1",
                params![cid.to_string()],
                row_to_entry,
            )
            .optional()?)
    }

    pub fn cache_summary(&self) -> Result<CacheSummary> {
        Ok(self.conn()?.query_row(
            "SELECT COUNT(*), COALESCE(SUM(size_bytes), 0),
                    COALESCE(SUM(CASE WHEN pinned THEN size_bytes ELSE 0 END), 0)
               FROM content_cache",
            [],
            |row| {
                Ok(CacheSummary {
                    block_count: row.get(0)?,
                    total_bytes: row.get(1)?,
                    pinned_bytes: row.get(2)?,
                })
            },
        )?)
    }

    /// Unpinned blocks, least recently used first: what to drop when the
    /// cache is over its limit.
    ///
    /// `rowid` breaks ties so that two blocks recorded in the same second
    /// still evict in the order they arrived, rather than arbitrarily.
    pub fn eviction_candidates(&self, limit: usize) -> Result<Vec<CacheEntry>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT cid, size_bytes, added_at, last_accessed, pinned
               FROM content_cache
              WHERE pinned = 0
              ORDER BY last_accessed ASC, added_at ASC, rowid ASC
              LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], row_to_entry)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Every tracked block, for reconciling the accounting against the disk.
    pub fn all_cache_entries(&self) -> Result<Vec<CacheEntry>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT cid, size_bytes, added_at, last_accessed, pinned FROM content_cache",
        )?;
        let rows = stmt.query_map([], row_to_entry)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<CacheEntry> {
    Ok(CacheEntry {
        cid: row.get(0)?,
        size_bytes: row.get(1)?,
        added_at: row.get(2)?,
        last_accessed: row.get(3)?,
        pinned: row.get::<_, i64>(4)? != 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cid(seed: &[u8]) -> ContentId {
        ContentId::from_raw(seed)
    }

    #[test]
    fn summary_separates_pinned_bytes() {
        let db = Database::open_in_memory().unwrap();
        db.record_cached(&cid(b"a"), 100, false).unwrap();
        db.record_cached(&cid(b"b"), 250, true).unwrap();
        let summary = db.cache_summary().unwrap();
        assert_eq!(summary.block_count, 2);
        assert_eq!(summary.total_bytes, 350);
        assert_eq!(summary.pinned_bytes, 250);
    }

    #[test]
    fn pinned_blocks_are_never_eviction_candidates() {
        let db = Database::open_in_memory().unwrap();
        db.record_cached(&cid(b"published"), 100, true).unwrap();
        db.record_cached(&cid(b"fetched"), 100, false).unwrap();
        let candidates = db.eviction_candidates(10).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].cid, cid(b"fetched").to_string());
    }

    #[test]
    fn eviction_order_is_least_recently_used() {
        let db = Database::open_in_memory().unwrap();
        for seed in [b"a".as_slice(), b"b", b"c"] {
            db.record_cached(&cid(seed), 10, false).unwrap();
        }
        // Make `a` the most recently used.
        db.conn()
            .unwrap()
            .execute(
                "UPDATE content_cache SET last_accessed = ?2 WHERE cid = ?1",
                params![cid(b"a").to_string(), now_secs() + 100],
            )
            .unwrap();
        let order: Vec<_> = db
            .eviction_candidates(10)
            .unwrap()
            .into_iter()
            .map(|e| e.cid)
            .collect();
        assert_eq!(order.last().unwrap(), &cid(b"a").to_string());
    }

    #[test]
    fn re_recording_a_block_keeps_its_pin() {
        let db = Database::open_in_memory().unwrap();
        db.record_cached(&cid(b"a"), 10, true).unwrap();
        db.record_cached(&cid(b"a"), 10, false).unwrap();
        assert!(db.cache_entry(&cid(b"a")).unwrap().unwrap().pinned);
        assert_eq!(db.cache_summary().unwrap().block_count, 1);
    }

    #[test]
    fn unpinning_makes_a_block_evictable() {
        let db = Database::open_in_memory().unwrap();
        db.record_cached(&cid(b"a"), 10, true).unwrap();
        assert!(db.eviction_candidates(10).unwrap().is_empty());
        db.set_pinned(&cid(b"a"), false).unwrap();
        assert_eq!(db.eviction_candidates(10).unwrap().len(), 1);
    }

    #[test]
    fn forgetting_a_block_reports_whether_it_was_tracked() {
        let db = Database::open_in_memory().unwrap();
        db.record_cached(&cid(b"a"), 10, false).unwrap();
        assert!(db.forget_cached(&cid(b"a")).unwrap());
        assert!(!db.forget_cached(&cid(b"a")).unwrap());
    }
}
