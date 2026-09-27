//! Local moderation (section 33).
//!
//! V1 does what a fully distributed network can honestly promise: this node
//! stops showing you content you blocked, by content id or by creator. There
//! is no global takedown, and no server that could offer one.

use rusqlite::params;
use serde::Serialize;

use ovn_identity::PublicKey;
use ovn_protocol::ContentId;

use crate::{now_secs, Database, Result};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockEntry {
    /// A content id or a creator public key, depending on which list.
    pub subject: String,
    pub blocked_at: i64,
    pub reason: String,
}

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<BlockEntry> {
    Ok(BlockEntry {
        subject: row.get(0)?,
        blocked_at: row.get(1)?,
        reason: row.get(2)?,
    })
}

impl Database {
    pub fn block_cid(&self, cid: &ContentId, reason: &str) -> Result<()> {
        self.conn()?.execute(
            "INSERT INTO blocked_cids (cid, blocked_at, reason) VALUES (?1, ?2, ?3)
             ON CONFLICT(cid) DO UPDATE SET reason = ?3",
            params![cid.to_string(), now_secs(), reason],
        )?;
        Ok(())
    }

    pub fn unblock_cid(&self, cid: &ContentId) -> Result<bool> {
        let changed = self.conn()?.execute(
            "DELETE FROM blocked_cids WHERE cid = ?1",
            params![cid.to_string()],
        )?;
        Ok(changed > 0)
    }

    pub fn is_cid_blocked(&self, cid: &ContentId) -> Result<bool> {
        Ok(self.conn()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM blocked_cids WHERE cid = ?1)",
            params![cid.to_string()],
            |row| row.get::<_, i64>(0),
        )? != 0)
    }

    pub fn block_creator(&self, public_key: &PublicKey, reason: &str) -> Result<()> {
        self.conn()?.execute(
            "INSERT INTO blocked_creators (public_key, blocked_at, reason) VALUES (?1, ?2, ?3)
             ON CONFLICT(public_key) DO UPDATE SET reason = ?3",
            params![public_key.to_hex(), now_secs(), reason],
        )?;
        Ok(())
    }

    pub fn unblock_creator(&self, public_key: &PublicKey) -> Result<bool> {
        let changed = self.conn()?.execute(
            "DELETE FROM blocked_creators WHERE public_key = ?1",
            params![public_key.to_hex()],
        )?;
        Ok(changed > 0)
    }

    pub fn is_creator_blocked_hex(&self, public_key_hex: &str) -> Result<bool> {
        Ok(self.conn()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM blocked_creators WHERE public_key = ?1)",
            params![public_key_hex],
            |row| row.get::<_, i64>(0),
        )? != 0)
    }

    pub fn is_creator_blocked(&self, public_key: &PublicKey) -> Result<bool> {
        self.is_creator_blocked_hex(&public_key.to_hex())
    }

    pub fn blocked_cids(&self) -> Result<Vec<BlockEntry>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare("SELECT cid, blocked_at, reason FROM blocked_cids ORDER BY blocked_at DESC")?;
        let rows = stmt.query_map([], row_to_entry)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn blocked_creators(&self) -> Result<Vec<BlockEntry>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT public_key, blocked_at, reason FROM blocked_creators ORDER BY blocked_at DESC",
        )?;
        let rows = stmt.query_map([], row_to_entry)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ovn_identity::Identity;

    #[test]
    fn blocking_and_unblocking_a_cid() {
        let db = Database::open_in_memory().unwrap();
        let cid = ContentId::from_dag_cbor(b"v");
        assert!(!db.is_cid_blocked(&cid).unwrap());
        db.block_cid(&cid, "not for me").unwrap();
        assert!(db.is_cid_blocked(&cid).unwrap());
        assert_eq!(db.blocked_cids().unwrap()[0].reason, "not for me");
        assert!(db.unblock_cid(&cid).unwrap());
        assert!(!db.is_cid_blocked(&cid).unwrap());
        assert!(!db.unblock_cid(&cid).unwrap());
    }

    #[test]
    fn blocking_and_unblocking_a_creator() {
        let db = Database::open_in_memory().unwrap();
        let key = Identity::generate().public_key();
        db.block_creator(&key, "spam").unwrap();
        assert!(db.is_creator_blocked(&key).unwrap());
        assert!(db.is_creator_blocked_hex(&key.to_hex()).unwrap());
        assert_eq!(db.blocked_creators().unwrap().len(), 1);
        assert!(db.unblock_creator(&key).unwrap());
        assert!(!db.is_creator_blocked(&key).unwrap());
    }

    #[test]
    fn blocking_twice_updates_the_reason_rather_than_failing() {
        let db = Database::open_in_memory().unwrap();
        let cid = ContentId::from_dag_cbor(b"v");
        db.block_cid(&cid, "first").unwrap();
        db.block_cid(&cid, "second").unwrap();
        let entries = db.blocked_cids().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].reason, "second");
    }
}
