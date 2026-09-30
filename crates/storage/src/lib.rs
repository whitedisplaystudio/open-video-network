//! Cache policy (section 29).
//!
//! The block store holds bytes and the database holds the bookkeeping; this
//! crate is the policy that joins them. A node keeps what it publishes
//! forever (pinned) and keeps what it fetched only while there is room,
//! dropping the least recently used blocks when the limit is passed.
//!
//! Section 30 is honest about the consequence: V1 does not promise a video
//! stays available. If the creator goes offline and every cache has evicted
//! it, it is gone.

use std::path::Path;

use ovn_content::{BlockStore, ContentId, ImportedVideo, VideoManifest};
use ovn_database::{CacheSummary, Database};

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error(transparent)]
    Content(#[from] ovn_content::ContentError),
    #[error(transparent)]
    Database(#[from] ovn_database::DatabaseError),
}

pub type Result<T> = std::result::Result<T, StorageError>;

/// Default cache ceiling, matching the design's example.
pub const DEFAULT_CACHE_LIMIT_BYTES: u64 = 10 * 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct StorageConfig {
    /// Bytes of unpinned content to keep. Pinned content is not counted
    /// against this: a node always keeps what it published.
    pub cache_limit_bytes: u64,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            cache_limit_bytes: DEFAULT_CACHE_LIMIT_BYTES,
        }
    }
}

/// What one eviction pass did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EvictionReport {
    pub blocks_removed: u64,
    pub bytes_freed: u64,
}

impl EvictionReport {
    pub fn is_empty(&self) -> bool {
        self.blocks_removed == 0
    }
}

/// Block store plus cache policy.
#[derive(Clone, Debug)]
pub struct Storage {
    store: BlockStore,
    db: Database,
    config: StorageConfig,
}

impl Storage {
    pub fn new(store: BlockStore, db: Database, config: StorageConfig) -> Self {
        Self { store, db, config }
    }

    pub fn store(&self) -> &BlockStore {
        &self.store
    }

    pub fn config(&self) -> &StorageConfig {
        &self.config
    }

    pub fn has(&self, cid: &ContentId) -> bool {
        self.store.has(cid)
    }

    /// Read a block and mark it as recently used, so that watching something
    /// keeps it in the cache.
    pub fn get(&self, cid: &ContentId) -> Result<Vec<u8>> {
        let data = self.forget_if_corrupt(self.store.get(cid))?;
        self.db.touch_cached(cid)?;
        Ok(data)
    }

    pub fn try_get(&self, cid: &ContentId) -> Result<Option<Vec<u8>>> {
        match self.forget_if_corrupt(self.store.try_get(cid))? {
            Some(data) => {
                self.db.touch_cached(cid)?;
                Ok(Some(data))
            }
            None => Ok(None),
        }
    }

    /// The block store deletes a block that no longer hashes to its id rather
    /// than handing back bad bytes. The bookkeeping has to follow: otherwise
    /// the cache keeps charging for a file that is gone, and the node keeps
    /// counting itself as a provider of content it can no longer serve.
    /// The error names the block, so this covers every read that can reach a
    /// bad file, not only the ones that go through [`Storage::get`].
    fn forget_if_corrupt<T>(
        &self,
        result: std::result::Result<T, ovn_content::ContentError>,
    ) -> Result<T> {
        if let Err(ovn_content::ContentError::IntegrityFailure { cid }) = &result {
            tracing::warn!(%cid, "a stored block no longer matches its id; dropping it");
            if let Err(e) = self.db.forget_cached_raw(cid) {
                tracing::warn!(%cid, error = %e, "could not un-record a corrupt block");
            }
        }
        Ok(result?)
    }

    /// Write a video back out as the file it was, keeping the cache
    /// accounting right if a block turns out to be damaged on the way.
    pub fn assemble(&self, manifest: &VideoManifest, out_path: impl AsRef<Path>) -> Result<u64> {
        self.forget_if_corrupt(self.store.assemble(manifest, out_path))
    }

    /// Store bytes received from a peer under the id we requested.
    ///
    /// Integrity is checked inside the block store; if the peer lied, nothing
    /// is written and nothing is recorded.
    pub fn put_from_peer(&self, cid: &ContentId, data: &[u8]) -> Result<()> {
        self.store.put_verified(cid, data)?;
        self.db.record_cached(cid, data.len() as u64, false)?;
        Ok(())
    }

    /// Import a file this node is publishing. Every block is pinned.
    pub fn import_and_pin(&self, path: impl AsRef<Path>) -> Result<ImportedVideo> {
        let imported = self.store.import_file(path)?;
        for chunk in &imported.manifest.chunks {
            let size = self.store.block_size(chunk).unwrap_or(0);
            self.db.record_cached(chunk, size, true)?;
        }
        self.db.record_cached(
            &imported.content_id,
            imported.manifest_bytes.len() as u64,
            true,
        )?;
        Ok(imported)
    }

    /// Pin everything a manifest refers to, so a fetched video the user wants
    /// to keep survives eviction.
    pub fn pin_video(&self, manifest: &VideoManifest, pinned: bool) -> Result<()> {
        for chunk in &manifest.chunks {
            self.db.set_pinned(chunk, pinned)?;
        }
        Ok(())
    }

    pub fn usage(&self) -> Result<CacheSummary> {
        Ok(self.db.cache_summary()?)
    }

    /// How many unpinned bytes are held.
    pub fn evictable_bytes(&self) -> Result<u64> {
        let summary = self.usage()?;
        Ok((summary.total_bytes - summary.pinned_bytes).max(0) as u64)
    }

    /// Drop least-recently-used unpinned blocks until the cache fits.
    ///
    /// Called after every fetch, so the limit is a ceiling rather than a
    /// periodic cleanup target.
    pub fn enforce_limit(&self) -> Result<EvictionReport> {
        let mut report = EvictionReport::default();
        let mut evictable = self.evictable_bytes()?;
        if evictable <= self.config.cache_limit_bytes {
            return Ok(report);
        }

        // Work in batches: a very full cache should not load every row.
        const BATCH: usize = 256;
        loop {
            let candidates = self.db.eviction_candidates(BATCH)?;
            if candidates.is_empty() {
                break;
            }
            for entry in candidates {
                if evictable <= self.config.cache_limit_bytes {
                    break;
                }
                let Ok(cid) = ContentId::parse(&entry.cid) else {
                    // Unparseable row: drop the bookkeeping, leave the disk be.
                    self.db.forget_cached_raw(&entry.cid)?;
                    continue;
                };
                self.store.remove(&cid)?;
                self.db.forget_cached(&cid)?;
                let freed = entry.size_bytes.max(0) as u64;
                evictable = evictable.saturating_sub(freed);
                report.blocks_removed += 1;
                report.bytes_freed += freed;
            }
            if evictable <= self.config.cache_limit_bytes {
                break;
            }
        }
        if !report.is_empty() {
            tracing::info!(
                blocks = report.blocks_removed,
                bytes = report.bytes_freed,
                "evicted cached blocks"
            );
        }
        Ok(report)
    }

    /// Make the database's view of the cache match what is actually on disk.
    ///
    /// Run at startup: a crash between writing a block and recording it, or a
    /// user deleting files by hand, should not leave the accounting wrong
    /// forever.
    pub fn reconcile(&self) -> Result<()> {
        let on_disk = self.store.list()?;
        for (cid, size) in &on_disk {
            if self.db.cache_entry(cid)?.is_none() {
                self.db.record_cached(cid, *size, false)?;
            }
        }
        let known: std::collections::HashSet<String> =
            on_disk.iter().map(|(cid, _)| cid.to_string()).collect();
        for entry in self.db.all_cache_entries()? {
            if !known.contains(&entry.cid) {
                self.db.forget_cached_raw(&entry.cid)?;
            }
        }
        tracing::debug!(blocks = on_disk.len(), "reconciled cache accounting");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _dir: tempfile::TempDir,
        storage: Storage,
        db: Database,
    }

    fn fixture(limit: u64) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let store = BlockStore::open(dir.path().join("blocks")).unwrap();
        let db = Database::open_in_memory().unwrap();
        let storage = Storage::new(
            store,
            db.clone(),
            StorageConfig {
                cache_limit_bytes: limit,
            },
        );
        Fixture {
            _dir: dir,
            storage,
            db,
        }
    }

    fn chunk(n: usize, size: usize) -> Vec<u8> {
        vec![n as u8; size]
    }

    #[test]
    fn a_verified_block_is_stored_and_counted() {
        let f = fixture(1_000);
        let data = chunk(1, 100);
        let cid = ContentId::from_raw(&data);
        f.storage.put_from_peer(&cid, &data).unwrap();
        assert!(f.storage.has(&cid));
        assert_eq!(f.storage.get(&cid).unwrap(), data);
        assert_eq!(f.storage.usage().unwrap().total_bytes, 100);
    }

    #[test]
    fn a_block_that_does_not_match_its_id_is_neither_stored_nor_counted() {
        let f = fixture(1_000);
        let cid = ContentId::from_raw(b"expected");
        assert!(f.storage.put_from_peer(&cid, b"delivered").is_err());
        assert!(!f.storage.has(&cid));
        assert_eq!(f.storage.usage().unwrap().block_count, 0);
    }

    #[test]
    fn under_the_limit_nothing_is_evicted() {
        let f = fixture(1_000);
        for i in 0..5 {
            let data = chunk(i, 100);
            f.storage
                .put_from_peer(&ContentId::from_raw(&data), &data)
                .unwrap();
        }
        assert!(f.storage.enforce_limit().unwrap().is_empty());
        assert_eq!(f.storage.usage().unwrap().block_count, 5);
    }

    #[test]
    fn over_the_limit_the_oldest_unpinned_blocks_go_first() {
        let f = fixture(300);
        let mut cids = Vec::new();
        for i in 0..5 {
            let data = chunk(i, 100);
            let cid = ContentId::from_raw(&data);
            f.storage.put_from_peer(&cid, &data).unwrap();
            cids.push(cid);
        }
        // All five were recorded in the same second, so eviction falls back to
        // insertion order: the first two stored are the first two dropped.
        let report = f.storage.enforce_limit().unwrap();
        assert_eq!(report.blocks_removed, 2);
        assert_eq!(report.bytes_freed, 200);
        assert!(!f.storage.has(&cids[0]));
        assert!(!f.storage.has(&cids[1]));
        assert!(f.storage.has(&cids[4]));
        assert_eq!(f.storage.evictable_bytes().unwrap(), 300);
    }

    #[test]
    fn published_content_is_pinned_and_survives_eviction_pressure() {
        let f = fixture(0);
        let source = f._dir.path().join("mine.mp4");
        std::fs::write(&source, vec![9u8; 2048]).unwrap();
        let imported = f.storage.import_and_pin(&source).unwrap();

        // Add unpinned content too, so there is something to evict.
        let other = chunk(1, 500);
        f.storage
            .put_from_peer(&ContentId::from_raw(&other), &other)
            .unwrap();

        let report = f.storage.enforce_limit().unwrap();
        assert_eq!(report.blocks_removed, 1);
        assert!(f.storage.has(&imported.content_id));
        for chunk_cid in &imported.manifest.chunks {
            assert!(f.storage.has(chunk_cid));
        }
    }

    #[test]
    fn eviction_stops_when_only_pinned_blocks_remain() {
        let f = fixture(0);
        let source = f._dir.path().join("mine.mp4");
        std::fs::write(&source, vec![9u8; 2048]).unwrap();
        f.storage.import_and_pin(&source).unwrap();
        // Nothing evictable: the pass must terminate rather than spin.
        assert!(f.storage.enforce_limit().unwrap().is_empty());
    }

    #[test]
    fn pinning_a_fetched_video_protects_it() {
        let f = fixture(0);
        let source = f._dir.path().join("v.mp4");
        std::fs::write(&source, vec![3u8; 1024]).unwrap();
        let imported = f.storage.import_and_pin(&source).unwrap();
        f.storage.pin_video(&imported.manifest, false).unwrap();
        assert_eq!(f.storage.enforce_limit().unwrap().blocks_removed, 1);

        // Re-import and keep it pinned this time.
        let imported = f.storage.import_and_pin(&source).unwrap();
        f.storage.pin_video(&imported.manifest, true).unwrap();
        f.storage.enforce_limit().unwrap();
        assert!(f.storage.has(&imported.manifest.chunks[0]));
    }

    #[test]
    fn reconcile_adopts_blocks_the_database_never_heard_of() {
        let f = fixture(10_000);
        // Write straight to the store, bypassing the accounting, as a crash
        // between the two steps would.
        let cid = f.storage.store().put_raw(b"orphan").unwrap();
        assert_eq!(f.storage.usage().unwrap().block_count, 0);
        f.storage.reconcile().unwrap();
        assert_eq!(f.storage.usage().unwrap().block_count, 1);
        assert!(f.db.cache_entry(&cid).unwrap().is_some());
    }

    #[test]
    fn reconcile_drops_rows_for_blocks_that_are_gone() {
        let f = fixture(10_000);
        let data = chunk(1, 50);
        let cid = ContentId::from_raw(&data);
        f.storage.put_from_peer(&cid, &data).unwrap();
        std::fs::remove_file(f.storage.store().path_for(&cid)).unwrap();
        f.storage.reconcile().unwrap();
        assert_eq!(f.storage.usage().unwrap().block_count, 0);
    }
}
