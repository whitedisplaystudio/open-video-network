//! Every node's local state, in one SQLite file.
//!
//! Two kinds of data live here and they are deliberately not mixed:
//!
//! * **Network data** — peers, discovered videos, creators, cache accounting.
//!   Public, shareable, and typed with `Serialize` so the local API can hand
//!   it to a GUI.
//! * **Local-only data** — watch history, watch ratios, preference weights.
//!   Section 32 requires that this has no path onto the network, so those
//!   types implement neither `Serialize` nor `Deserialize`. There is no
//!   setting to turn the sending off because there is nothing that can send.

mod cache;
mod moderation;
mod peers;
mod preferences;
mod schema;
mod videos;
mod watch;

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::Connection;

pub use cache::{CacheEntry, CacheSummary};
pub use moderation::BlockEntry;
pub use peers::{PeerRecord, PeerSource};
pub use preferences::TagWeight;
pub use videos::{CreatorRecord, VideoRecord, VideoUpsert};
pub use watch::{WatchEvent, WatchRecord, WatchSummary};

#[derive(Debug, thiserror::Error)]
pub enum DatabaseError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("the database lock was poisoned by a panic in another thread")]
    Poisoned,
    #[error("stored row is malformed: {0}")]
    Malformed(String),
    #[error("database schema version {found} is newer than this build supports ({supported})")]
    SchemaTooNew { found: i64, supported: i64 },
    #[error(transparent)]
    Protocol(#[from] ovn_protocol::ProtocolError),
}

pub type Result<T> = std::result::Result<T, DatabaseError>;

/// Handle to the node's SQLite database.
///
/// Cloning is cheap and shares the same connection; SQLite serialises access
/// behind the mutex. Operations here are small local reads and writes, so they
/// are synchronous by design.
#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Database")
    }
}

impl Database {
    /// Open (creating and migrating if needed) the database at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    DatabaseError::Malformed(format!("cannot create {}: {e}", parent.display()))
                })?;
            }
        }
        let conn = Connection::open(path)?;
        Self::configure(&conn)?;
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.migrate()?;
        Ok(db)
    }

    /// An in-memory database, for tests.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::configure(&conn)?;
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.migrate()?;
        Ok(db)
    }

    fn configure(conn: &Connection) -> Result<()> {
        // WAL keeps readers from blocking the writer, which matters once the
        // local API and the network event loop are both touching the file.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(())
    }

    pub(crate) fn conn(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|_| DatabaseError::Poisoned)
    }

    /// Run `VACUUM`, reclaiming space after a large eviction.
    pub fn vacuum(&self) -> Result<()> {
        self.conn()?.execute_batch("VACUUM")?;
        Ok(())
    }
}

pub(crate) fn now_secs() -> i64 {
    ovn_protocol::now_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_twice_keeps_the_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("node.db");
        {
            let db = Database::open(&path).unwrap();
            db.set_following(&test_key(), true).unwrap();
        }
        let db = Database::open(&path).unwrap();
        assert_eq!(db.following().unwrap().len(), 1);
    }

    pub(crate) fn test_key() -> ovn_identity::PublicKey {
        ovn_identity::Identity::generate().public_key()
    }
}
