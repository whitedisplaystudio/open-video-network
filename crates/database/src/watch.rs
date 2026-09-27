//! Watch history. **Local only.**
//!
//! Section 2 forbids sending any of this to a peer, and section 32 asks for
//! more than a setting: there should be no send mechanism at all. That is
//! enforced here by omission — [`WatchEvent`], [`WatchRecord`] and
//! [`WatchSummary`] implement neither `Serialize` nor `Deserialize`, so they
//! cannot be encoded into a protocol message even by mistake.
//!
//! Do not add those derives. The local HTTP API converts to its own display
//! types by hand, which keeps that conversion an explicit, reviewable step.

use rusqlite::{params, OptionalExtension};

use ovn_protocol::ContentId;

use crate::{now_secs, Database, Result};

/// One viewing event, as reported by a player.
#[derive(Clone, Debug)]
pub struct WatchEvent {
    pub cid: ContentId,
    /// Seconds actually watched.
    pub watched_secs: u32,
    /// Length of the video, if known.
    pub duration_secs: u32,
    /// The viewer reached the end.
    pub completed: bool,
    /// The viewer moved on early and deliberately.
    pub skipped: bool,
    /// The viewer liked it.
    pub liked: bool,
}

impl WatchEvent {
    pub fn new(cid: ContentId, watched_secs: u32, duration_secs: u32) -> Self {
        Self {
            cid,
            watched_secs,
            duration_secs,
            completed: false,
            skipped: false,
            liked: false,
        }
    }

    /// Fraction of the video watched, clamped to `0.0..=1.0`. Zero-length
    /// videos count as fully watched if anything was watched at all.
    pub fn ratio(&self) -> f64 {
        if self.duration_secs == 0 {
            return if self.watched_secs > 0 { 1.0 } else { 0.0 };
        }
        (self.watched_secs as f64 / self.duration_secs as f64).clamp(0.0, 1.0)
    }
}

#[derive(Clone, Debug)]
pub struct WatchRecord {
    pub id: i64,
    pub cid: String,
    pub watched_at: i64,
    pub watched_secs: u32,
    pub duration_secs: u32,
    pub completed: bool,
    pub skipped: bool,
    pub liked: bool,
}

impl WatchRecord {
    pub fn ratio(&self) -> f64 {
        if self.duration_secs == 0 {
            return if self.watched_secs > 0 { 1.0 } else { 0.0 };
        }
        (self.watched_secs as f64 / self.duration_secs as f64).clamp(0.0, 1.0)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct WatchSummary {
    pub event_count: i64,
    pub distinct_videos: i64,
    pub total_watched_secs: i64,
}

impl Database {
    /// Record a viewing event. Nothing about this ever leaves the device.
    pub fn record_watch(&self, event: &WatchEvent) -> Result<i64> {
        self.record_watch_at(event, now_secs())
    }

    /// Record a viewing event that happened at a given time.
    ///
    /// Used when replaying a session that was buffered while the node was
    /// stopped, and by tests that need to exercise the recency decay.
    pub fn record_watch_at(&self, event: &WatchEvent, watched_at: i64) -> Result<i64> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO watch_history
                 (cid, watched_at, watched_secs, duration_secs, completed, skipped, liked)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                event.cid.to_string(),
                watched_at,
                event.watched_secs as i64,
                event.duration_secs as i64,
                i64::from(event.completed),
                i64::from(event.skipped),
                i64::from(event.liked),
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn watch_history(&self, limit: usize) -> Result<Vec<WatchRecord>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, cid, watched_at, watched_secs, duration_secs, completed, skipped, liked
               FROM watch_history ORDER BY watched_at DESC, id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok(WatchRecord {
                id: row.get(0)?,
                cid: row.get(1)?,
                watched_at: row.get(2)?,
                watched_secs: row.get::<_, i64>(3)? as u32,
                duration_secs: row.get::<_, i64>(4)? as u32,
                completed: row.get::<_, i64>(5)? != 0,
                skipped: row.get::<_, i64>(6)? != 0,
                liked: row.get::<_, i64>(7)? != 0,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Best watch ratio we have ever recorded for a video, if any.
    pub fn best_watch_ratio(&self, cid: &ContentId) -> Result<Option<f64>> {
        let conn = self.conn()?;
        let value: Option<f64> = conn
            .query_row(
                "SELECT MAX(CASE WHEN duration_secs > 0
                                 THEN MIN(1.0, CAST(watched_secs AS REAL) / duration_secs)
                                 WHEN watched_secs > 0 THEN 1.0 ELSE 0.0 END)
                   FROM watch_history WHERE cid = ?1",
                params![cid.to_string()],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        Ok(value)
    }

    pub fn has_watched(&self, cid: &ContentId) -> Result<bool> {
        Ok(self.conn()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM watch_history WHERE cid = ?1)",
            params![cid.to_string()],
            |row| row.get::<_, i64>(0),
        )? != 0)
    }

    pub fn watch_summary(&self) -> Result<WatchSummary> {
        Ok(self.conn()?.query_row(
            "SELECT COUNT(*), COUNT(DISTINCT cid), COALESCE(SUM(watched_secs), 0)
               FROM watch_history",
            [],
            |row| {
                Ok(WatchSummary {
                    event_count: row.get(0)?,
                    distinct_videos: row.get(1)?,
                    total_watched_secs: row.get(2)?,
                })
            },
        )?)
    }

    /// Erase all viewing data. The user owns this and must be able to drop it.
    pub fn clear_watch_history(&self) -> Result<usize> {
        Ok(self.conn()?.execute("DELETE FROM watch_history", [])?)
    }

    /// Watch events joined with the tags of the video watched, newest first.
    ///
    /// This is the only input the preference model needs, and the join stays
    /// inside the database so that the two halves never have to be carried
    /// around together by anything that talks to the network.
    pub fn watch_history_with_tags(&self, limit: usize) -> Result<Vec<(WatchRecord, Vec<String>)>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT h.id, h.cid, h.watched_at, h.watched_secs, h.duration_secs,
                    h.completed, h.skipped, h.liked, COALESCE(v.tags, '')
               FROM watch_history h
               LEFT JOIN known_videos v ON v.cid = h.cid
              ORDER BY h.watched_at DESC, h.id DESC
              LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            let record = WatchRecord {
                id: row.get(0)?,
                cid: row.get(1)?,
                watched_at: row.get(2)?,
                watched_secs: row.get::<_, i64>(3)? as u32,
                duration_secs: row.get::<_, i64>(4)? as u32,
                completed: row.get::<_, i64>(5)? != 0,
                skipped: row.get::<_, i64>(6)? != 0,
                liked: row.get::<_, i64>(7)? != 0,
            };
            let tags: String = row.get(8)?;
            Ok((
                record,
                tags.split_whitespace().map(str::to_string).collect(),
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Content ids the user has watched at all, for filtering a feed.
    pub fn watched_cids(&self) -> Result<std::collections::HashSet<String>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare("SELECT DISTINCT cid FROM watch_history")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<std::collections::HashSet<_>>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cid(seed: &[u8]) -> ContentId {
        ContentId::from_dag_cbor(seed)
    }

    #[test]
    fn ratio_handles_partial_full_and_unknown_durations() {
        assert_eq!(WatchEvent::new(cid(b"a"), 30, 120).ratio(), 0.25);
        assert_eq!(WatchEvent::new(cid(b"a"), 120, 120).ratio(), 1.0);
        // A player reporting more than the duration must not exceed 1.0.
        assert_eq!(WatchEvent::new(cid(b"a"), 500, 120).ratio(), 1.0);
        assert_eq!(WatchEvent::new(cid(b"a"), 10, 0).ratio(), 1.0);
        assert_eq!(WatchEvent::new(cid(b"a"), 0, 0).ratio(), 0.0);
    }

    #[test]
    fn events_are_recorded_and_listed_newest_first() {
        let db = Database::open_in_memory().unwrap();
        db.record_watch(&WatchEvent::new(cid(b"a"), 10, 100))
            .unwrap();
        db.record_watch(&WatchEvent::new(cid(b"b"), 90, 100))
            .unwrap();
        let history = db.watch_history(10).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].cid, cid(b"b").to_string());
        assert_eq!(history[0].ratio(), 0.9);
    }

    #[test]
    fn best_ratio_takes_the_most_complete_viewing() {
        let db = Database::open_in_memory().unwrap();
        let c = cid(b"a");
        db.record_watch(&WatchEvent::new(c, 10, 100)).unwrap();
        db.record_watch(&WatchEvent::new(c, 80, 100)).unwrap();
        db.record_watch(&WatchEvent::new(c, 40, 100)).unwrap();
        assert_eq!(db.best_watch_ratio(&c).unwrap(), Some(0.8));
        assert!(db.has_watched(&c).unwrap());
        assert_eq!(db.best_watch_ratio(&cid(b"never")).unwrap(), None);
        assert!(!db.has_watched(&cid(b"never")).unwrap());
    }

    #[test]
    fn summary_counts_events_videos_and_seconds() {
        let db = Database::open_in_memory().unwrap();
        db.record_watch(&WatchEvent::new(cid(b"a"), 10, 100))
            .unwrap();
        db.record_watch(&WatchEvent::new(cid(b"a"), 20, 100))
            .unwrap();
        db.record_watch(&WatchEvent::new(cid(b"b"), 30, 100))
            .unwrap();
        let summary = db.watch_summary().unwrap();
        assert_eq!(summary.event_count, 3);
        assert_eq!(summary.distinct_videos, 2);
        assert_eq!(summary.total_watched_secs, 60);
    }

    #[test]
    fn the_user_can_erase_everything() {
        let db = Database::open_in_memory().unwrap();
        db.record_watch(&WatchEvent::new(cid(b"a"), 10, 100))
            .unwrap();
        assert_eq!(db.clear_watch_history().unwrap(), 1);
        assert_eq!(db.watch_summary().unwrap().event_count, 0);
    }

    #[test]
    fn flags_round_trip() {
        let db = Database::open_in_memory().unwrap();
        let mut event = WatchEvent::new(cid(b"a"), 100, 100);
        event.completed = true;
        event.liked = true;
        db.record_watch(&event).unwrap();
        let record = &db.watch_history(1).unwrap()[0];
        assert!(record.completed && record.liked && !record.skipped);
    }
}
