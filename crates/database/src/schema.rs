//! Schema definition and forward-only migrations.

use crate::{Database, DatabaseError, Result};

/// Bump this and append a migration whenever the schema changes.
pub(crate) const SCHEMA_VERSION: i64 = 3;

const MIGRATION_1: &str = r#"
-- ---------------------------------------------------------------- network
-- Peers we have heard of, however we heard of them.
CREATE TABLE known_peers (
    peer_id         TEXT PRIMARY KEY,
    public_key      BLOB,
    node_name       TEXT NOT NULL DEFAULT '',
    addresses       TEXT NOT NULL DEFAULT '',   -- newline separated multiaddrs
    source          TEXT NOT NULL,
    first_seen      INTEGER NOT NULL,
    last_seen       INTEGER NOT NULL,
    last_connected  INTEGER,
    failed_attempts INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX known_peers_last_seen ON known_peers(last_seen DESC);

-- Creator profiles, learned from signed PROFILE_UPDATE messages.
CREATE TABLE creators (
    public_key   TEXT PRIMARY KEY,              -- lowercase hex
    display_name TEXT NOT NULL DEFAULT '',
    bio          TEXT NOT NULL DEFAULT '',
    avatar_cid   TEXT,
    updated_at   INTEGER NOT NULL,
    first_seen   INTEGER NOT NULL
);

-- Videos we have discovered. `announcement` keeps the original signed CBOR so
-- the claim can be re-verified, and re-served, byte for byte.
CREATE TABLE known_videos (
    cid                TEXT PRIMARY KEY,
    creator_public_key TEXT NOT NULL,
    title              TEXT NOT NULL,
    description        TEXT NOT NULL DEFAULT '',
    tags               TEXT NOT NULL DEFAULT '',  -- space separated, normalised
    duration_secs      INTEGER NOT NULL DEFAULT 0,
    thumbnail_cid      TEXT,
    created_at         INTEGER NOT NULL,
    discovered_at      INTEGER NOT NULL,
    announcement       BLOB NOT NULL,
    is_local           INTEGER NOT NULL DEFAULT 0,
    have_manifest      INTEGER NOT NULL DEFAULT 0,
    have_content       INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX known_videos_created_at ON known_videos(created_at DESC);
CREATE INDEX known_videos_creator ON known_videos(creator_public_key);

-- Full text index over discovered metadata. Section 26: search is local, and
-- a query never leaves the device.
CREATE VIRTUAL TABLE videos_fts USING fts5(
    title,
    description,
    tags,
    content='known_videos',
    content_rowid='rowid',
    tokenize='unicode61 remove_diacritics 2'
);
CREATE TRIGGER known_videos_ai AFTER INSERT ON known_videos BEGIN
    INSERT INTO videos_fts(rowid, title, description, tags)
    VALUES (new.rowid, new.title, new.description, new.tags);
END;
CREATE TRIGGER known_videos_ad AFTER DELETE ON known_videos BEGIN
    INSERT INTO videos_fts(videos_fts, rowid, title, description, tags)
    VALUES ('delete', old.rowid, old.title, old.description, old.tags);
END;
CREATE TRIGGER known_videos_au AFTER UPDATE ON known_videos BEGIN
    INSERT INTO videos_fts(videos_fts, rowid, title, description, tags)
    VALUES ('delete', old.rowid, old.title, old.description, old.tags);
    INSERT INTO videos_fts(rowid, title, description, tags)
    VALUES (new.rowid, new.title, new.description, new.tags);
END;

CREATE TABLE following (
    public_key TEXT PRIMARY KEY,
    since      INTEGER NOT NULL
);

-- Which blocks we hold, for cache accounting and eviction.
CREATE TABLE content_cache (
    cid           TEXT PRIMARY KEY,
    size_bytes    INTEGER NOT NULL,
    added_at      INTEGER NOT NULL,
    last_accessed INTEGER NOT NULL,
    pinned        INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX content_cache_eviction ON content_cache(pinned, last_accessed);

-- Local moderation (section 33).
CREATE TABLE blocked_cids (
    cid        TEXT PRIMARY KEY,
    blocked_at INTEGER NOT NULL,
    reason     TEXT NOT NULL DEFAULT ''
);
CREATE TABLE blocked_creators (
    public_key TEXT PRIMARY KEY,
    blocked_at INTEGER NOT NULL,
    reason     TEXT NOT NULL DEFAULT ''
);

-- ------------------------------------------------------------- local only
-- NOTHING BELOW THIS LINE MAY EVER BE SENT TO A PEER.
-- The Rust types for these tables implement neither Serialize nor
-- Deserialize, so there is no mechanism that could transmit them.
CREATE TABLE watch_history (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    cid           TEXT NOT NULL,
    watched_at    INTEGER NOT NULL,
    watched_secs  INTEGER NOT NULL,
    duration_secs INTEGER NOT NULL,
    completed     INTEGER NOT NULL DEFAULT 0,
    skipped       INTEGER NOT NULL DEFAULT 0,
    liked         INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX watch_history_cid ON watch_history(cid);
CREATE INDEX watch_history_time ON watch_history(watched_at DESC);

CREATE TABLE preferences (
    tag        TEXT PRIMARY KEY,
    weight     REAL NOT NULL,
    updated_at INTEGER NOT NULL
);
"#;

/// A subscription is a follow that also remembers where to go asking.
///
/// Extending the table rather than adding another one keeps the two from
/// drifting apart: there is no state where you follow someone but are not
/// subscribed to them, and no second place to look.
const MIGRATION_2: &str = r#"
ALTER TABLE following ADD COLUMN display_name TEXT NOT NULL DEFAULT '';
-- Newline-separated multiaddrs from the channel link. Hints, not identity:
-- a subscription outlives all of them going stale.
ALTER TABLE following ADD COLUMN addresses TEXT NOT NULL DEFAULT '';
-- When this channel was last asked for new work, so a refresh can ask for
-- little rather than for everything.
ALTER TABLE following ADD COLUMN last_checked INTEGER NOT NULL DEFAULT 0;
"#;

/// Where a video's bytes are served from.
///
/// The network carries what is needed to find and check a video; the file
/// itself comes from the creator's own server, and this is the address of it.
/// Signed as part of the announcement, so it is a claim by the creator rather
/// than by whoever relayed it.
const MIGRATION_3: &str = r#"
ALTER TABLE known_videos ADD COLUMN source_url TEXT NOT NULL DEFAULT '';
"#;

impl Database {
    pub(crate) fn migrate(&self) -> Result<()> {
        let conn = self.conn()?;
        let current: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if current > SCHEMA_VERSION {
            return Err(DatabaseError::SchemaTooNew {
                found: current,
                supported: SCHEMA_VERSION,
            });
        }
        if current < 1 {
            conn.execute_batch(MIGRATION_1)?;
        }
        if current < 2 {
            conn.execute_batch(MIGRATION_2)?;
        }
        if current < 3 {
            conn.execute_batch(MIGRATION_3)?;
        }
        if current != SCHEMA_VERSION {
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            tracing::info!(from = current, to = SCHEMA_VERSION, "migrated database");
        }
        Ok(())
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self
            .conn()?
            .pragma_query_value(None, "user_version", |row| row.get(0))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_database_is_at_the_current_version() {
        let db = Database::open_in_memory().unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn migrating_twice_is_a_no_op() {
        let db = Database::open_in_memory().unwrap();
        db.migrate().unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn a_database_from_the_future_is_refused_rather_than_corrupted() {
        let db = Database::open_in_memory().unwrap();
        db.conn()
            .unwrap()
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        assert!(matches!(
            db.migrate(),
            Err(DatabaseError::SchemaTooNew { .. })
        ));
    }
}
