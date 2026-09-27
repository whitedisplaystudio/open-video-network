//! Known peers: who we have met and how we met them (section 13).

use rusqlite::{params, OptionalExtension, Row};
use serde::Serialize;

use crate::{now_secs, Database, Result};

/// How we first learned about a peer. Kept so that `ourvideo peer list` can
/// tell the user whether a peer came from their own link, from the local
/// network, or from the DHT.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PeerSource {
    /// Found on the local network by mDNS.
    Mdns,
    /// Found through the Kademlia DHT.
    Dht,
    /// Added by the user from a URL or share link.
    Url,
    /// Added by the user as a raw multiaddr.
    Manual,
    /// A configured bootstrap peer.
    Bootstrap,
    /// Met on a connection — they dialled us, or we identified them after
    /// connecting. We know they are reachable but not how we first heard of
    /// them.
    Connected,
}

impl PeerSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Mdns => "mdns",
            Self::Dht => "dht",
            Self::Url => "url",
            Self::Manual => "manual",
            Self::Bootstrap => "bootstrap",
            Self::Connected => "connected",
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "mdns" => Self::Mdns,
            "url" => Self::Url,
            "manual" => Self::Manual,
            "bootstrap" => Self::Bootstrap,
            "connected" => Self::Connected,
            _ => Self::Dht,
        }
    }
}

impl std::fmt::Display for PeerSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerRecord {
    pub peer_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_key: Option<String>,
    pub node_name: String,
    pub addresses: Vec<String>,
    pub source: PeerSource,
    pub first_seen: i64,
    pub last_seen: i64,
    pub last_connected: Option<i64>,
    pub failed_attempts: i64,
}

fn row_to_peer(row: &Row<'_>) -> rusqlite::Result<PeerRecord> {
    let addresses: String = row.get("addresses")?;
    let public_key: Option<Vec<u8>> = row.get("public_key")?;
    Ok(PeerRecord {
        peer_id: row.get("peer_id")?,
        public_key: public_key.map(|k| data_hex(&k)),
        node_name: row.get("node_name")?,
        addresses: addresses
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(str::to_string)
            .collect(),
        source: PeerSource::from_str(&row.get::<_, String>("source")?),
        first_seen: row.get("first_seen")?,
        last_seen: row.get("last_seen")?,
        last_connected: row.get("last_connected")?,
        failed_attempts: row.get("failed_attempts")?,
    })
}

fn data_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl Database {
    /// Record that we know about a peer, merging with anything already stored.
    ///
    /// Addresses accumulate rather than replace: a peer reachable on both a
    /// LAN address and a public one should keep both.
    pub fn upsert_peer(
        &self,
        peer_id: &str,
        addresses: &[String],
        source: PeerSource,
        node_name: Option<&str>,
        public_key: Option<&[u8]>,
    ) -> Result<()> {
        let now = now_secs();
        let conn = self.conn()?;
        let existing: Option<String> = conn
            .query_row(
                "SELECT addresses FROM known_peers WHERE peer_id = ?1",
                params![peer_id],
                |row| row.get(0),
            )
            .optional()?;

        let mut merged: Vec<String> = existing
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(str::to_string)
            .collect();
        for address in addresses {
            if !address.trim().is_empty() && !merged.contains(address) {
                merged.push(address.clone());
            }
        }
        // Keep the list bounded: a peer that keeps announcing new ephemeral
        // ports must not be able to grow our database without limit.
        const MAX_STORED_ADDRESSES: usize = 32;
        if merged.len() > MAX_STORED_ADDRESSES {
            let overflow = merged.len() - MAX_STORED_ADDRESSES;
            merged.drain(0..overflow);
        }

        conn.execute(
            "INSERT INTO known_peers
                 (peer_id, public_key, node_name, addresses, source,
                  first_seen, last_seen, failed_attempts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, 0)
             ON CONFLICT(peer_id) DO UPDATE SET
                 addresses  = ?4,
                 last_seen  = ?6,
                 node_name  = CASE WHEN ?3 != '' THEN ?3 ELSE known_peers.node_name END,
                 public_key = COALESCE(?2, known_peers.public_key)",
            params![
                peer_id,
                public_key,
                node_name.unwrap_or(""),
                merged.join("\n"),
                source.as_str(),
                now,
            ],
        )?;
        Ok(())
    }

    /// A connection succeeded: clear the failure count.
    pub fn mark_peer_connected(&self, peer_id: &str) -> Result<()> {
        let now = now_secs();
        self.conn()?.execute(
            "UPDATE known_peers
                SET last_connected = ?2, last_seen = ?2, failed_attempts = 0
              WHERE peer_id = ?1",
            params![peer_id, now],
        )?;
        Ok(())
    }

    /// A dial failed. Peers that keep failing sink down the candidate list.
    pub fn mark_peer_failed(&self, peer_id: &str) -> Result<()> {
        self.conn()?.execute(
            "UPDATE known_peers SET failed_attempts = failed_attempts + 1 WHERE peer_id = ?1",
            params![peer_id],
        )?;
        Ok(())
    }

    pub fn peer(&self, peer_id: &str) -> Result<Option<PeerRecord>> {
        Ok(self
            .conn()?
            .query_row(
                "SELECT * FROM known_peers WHERE peer_id = ?1",
                params![peer_id],
                row_to_peer,
            )
            .optional()?)
    }

    pub fn peers(&self) -> Result<Vec<PeerRecord>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare("SELECT * FROM known_peers ORDER BY last_seen DESC")?;
        let rows = stmt.query_map([], row_to_peer)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn peer_count(&self) -> Result<i64> {
        Ok(self
            .conn()?
            .query_row("SELECT COUNT(*) FROM known_peers", [], |row| row.get(0))?)
    }

    /// Peers worth dialling on startup: ones that worked before, that have not
    /// been failing, and that we have an address for.
    ///
    /// This is what lets a node rejoin the network after every bootstrap node
    /// and domain the developers ran has been switched off (Test H).
    pub fn dial_candidates(&self, limit: usize) -> Result<Vec<PeerRecord>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT * FROM known_peers
              WHERE addresses != ''
              ORDER BY failed_attempts ASC,
                       last_connected DESC NULLS LAST,
                       last_seen DESC
              LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], row_to_peer)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn forget_peer(&self, peer_id: &str) -> Result<bool> {
        let changed = self.conn()?.execute(
            "DELETE FROM known_peers WHERE peer_id = ?1",
            params![peer_id],
        )?;
        Ok(changed > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn a_peer_is_stored_and_read_back() {
        let db = db();
        db.upsert_peer(
            "12D3KooWaaa",
            &["/ip4/192.0.2.1/udp/4800/quic-v1".into()],
            PeerSource::Url,
            Some("example node"),
            Some(&[7u8; 32]),
        )
        .unwrap();
        let peer = db.peer("12D3KooWaaa").unwrap().unwrap();
        assert_eq!(peer.node_name, "example node");
        assert_eq!(peer.source, PeerSource::Url);
        assert_eq!(peer.addresses, vec!["/ip4/192.0.2.1/udp/4800/quic-v1"]);
        assert_eq!(peer.public_key.unwrap(), "07".repeat(32));
        assert_eq!(db.peer_count().unwrap(), 1);
    }

    #[test]
    fn addresses_accumulate_across_sightings() {
        let db = db();
        db.upsert_peer(
            "p",
            &["/ip4/192.0.2.1/tcp/4800".into()],
            PeerSource::Mdns,
            None,
            None,
        )
        .unwrap();
        db.upsert_peer(
            "p",
            &["/ip4/10.0.0.5/tcp/4800".into()],
            PeerSource::Dht,
            None,
            None,
        )
        .unwrap();
        let peer = db.peer("p").unwrap().unwrap();
        assert_eq!(peer.addresses.len(), 2);
    }

    #[test]
    fn repeating_an_address_does_not_duplicate_it() {
        let db = db();
        for _ in 0..5 {
            db.upsert_peer(
                "p",
                &["/ip4/192.0.2.1/tcp/4800".into()],
                PeerSource::Mdns,
                None,
                None,
            )
            .unwrap();
        }
        assert_eq!(db.peer("p").unwrap().unwrap().addresses.len(), 1);
    }

    #[test]
    fn the_stored_address_list_is_bounded() {
        let db = db();
        for port in 0..50 {
            db.upsert_peer(
                "noisy",
                &[format!("/ip4/192.0.2.1/tcp/{port}")],
                PeerSource::Connected,
                None,
                None,
            )
            .unwrap();
        }
        assert_eq!(db.peer("noisy").unwrap().unwrap().addresses.len(), 32);
    }

    #[test]
    fn a_later_sighting_does_not_erase_a_known_name() {
        let db = db();
        db.upsert_peer("p", &[], PeerSource::Url, Some("named"), None)
            .unwrap();
        db.upsert_peer("p", &[], PeerSource::Dht, None, None)
            .unwrap();
        assert_eq!(db.peer("p").unwrap().unwrap().node_name, "named");
    }

    #[test]
    fn connecting_clears_the_failure_count() {
        let db = db();
        db.upsert_peer(
            "p",
            &["/ip4/192.0.2.1/tcp/1".into()],
            PeerSource::Dht,
            None,
            None,
        )
        .unwrap();
        db.mark_peer_failed("p").unwrap();
        db.mark_peer_failed("p").unwrap();
        assert_eq!(db.peer("p").unwrap().unwrap().failed_attempts, 2);
        db.mark_peer_connected("p").unwrap();
        let peer = db.peer("p").unwrap().unwrap();
        assert_eq!(peer.failed_attempts, 0);
        assert!(peer.last_connected.is_some());
    }

    #[test]
    fn dial_candidates_prefer_peers_that_have_worked() {
        let db = db();
        db.upsert_peer(
            "flaky",
            &["/ip4/192.0.2.1/tcp/1".into()],
            PeerSource::Dht,
            None,
            None,
        )
        .unwrap();
        db.mark_peer_failed("flaky").unwrap();
        db.upsert_peer(
            "good",
            &["/ip4/192.0.2.2/tcp/1".into()],
            PeerSource::Dht,
            None,
            None,
        )
        .unwrap();
        db.mark_peer_connected("good").unwrap();
        db.upsert_peer("addressless", &[], PeerSource::Dht, None, None)
            .unwrap();

        let candidates = db.dial_candidates(10).unwrap();
        let ids: Vec<_> = candidates.iter().map(|p| p.peer_id.as_str()).collect();
        assert_eq!(ids, vec!["good", "flaky"]);
    }

    #[test]
    fn forgetting_a_peer_reports_whether_it_existed() {
        let db = db();
        db.upsert_peer("p", &[], PeerSource::Manual, None, None)
            .unwrap();
        assert!(db.forget_peer("p").unwrap());
        assert!(!db.forget_peer("p").unwrap());
        assert!(db.peer("p").unwrap().is_none());
    }
}
