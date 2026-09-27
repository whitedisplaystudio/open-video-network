//! The on-disk block store.
//!
//! Every block is a file named after its [`ContentId`], sharded one level deep
//! by the first byte of the digest so that a large store does not become one
//! enormous directory. Blocks are verified on the way in *and* on the way out:
//! section 20 is about hostile peers, but the same check catches a bad disk.

use std::fs;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ovn_protocol::{ContentId, CHUNK_SIZE};

use crate::manifest::{ImportedVideo, VideoManifest};
use crate::probe::guess_media_type;
use crate::{io_err, ContentError, Result};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StoreStats {
    pub block_count: u64,
    pub total_bytes: u64,
}

#[derive(Clone, Debug)]
pub struct BlockStore {
    root: PathBuf,
}

impl BlockStore {
    /// Open (creating if needed) a block store rooted at `root`.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(io_err(&root))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path_for(&self, cid: &ContentId) -> PathBuf {
        let digest = cid.digest();
        let shard = format!("{:02x}", digest.first().copied().unwrap_or(0));
        self.root.join(shard).join(cid.to_string())
    }

    pub fn has(&self, cid: &ContentId) -> bool {
        self.path_for(cid).is_file()
    }

    /// Store a chunk of opaque bytes, returning its id.
    pub fn put_raw(&self, data: &[u8]) -> Result<ContentId> {
        if data.len() > CHUNK_SIZE {
            return Err(ContentError::ChunkTooLarge {
                actual: data.len(),
                limit: CHUNK_SIZE,
            });
        }
        let cid = ContentId::from_raw(data);
        self.write_block(&cid, data)?;
        Ok(cid)
    }

    /// Store a DAG-CBOR document, returning its id.
    pub fn put_dag_cbor(&self, data: &[u8]) -> Result<ContentId> {
        let cid = ContentId::from_dag_cbor(data);
        self.write_block(&cid, data)?;
        Ok(cid)
    }

    /// Store bytes that arrived from a peer under the id we asked for.
    ///
    /// This is the enforcement point for section 20: if the bytes do not hash
    /// to `cid`, nothing is written and the caller learns the peer lied.
    pub fn put_verified(&self, cid: &ContentId, data: &[u8]) -> Result<()> {
        if data.len() > CHUNK_SIZE {
            return Err(ContentError::ChunkTooLarge {
                actual: data.len(),
                limit: CHUNK_SIZE,
            });
        }
        if !cid.verifies(data) {
            return Err(ContentError::IntegrityFailure {
                cid: cid.to_string(),
            });
        }
        self.write_block(cid, data)
    }

    fn write_block(&self, cid: &ContentId, data: &[u8]) -> Result<()> {
        let final_path = self.path_for(cid);
        if final_path.is_file() {
            return Ok(());
        }
        let dir = final_path
            .parent()
            .expect("block paths always have a shard directory");
        fs::create_dir_all(dir).map_err(io_err(dir))?;

        // Write to a temporary name and rename, so a crash mid-write can never
        // leave a truncated file sitting under a valid content id.
        let temp_path = dir.join(format!(
            ".tmp-{}-{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        {
            let mut file = fs::File::create(&temp_path).map_err(io_err(&temp_path))?;
            file.write_all(data).map_err(io_err(&temp_path))?;
            file.sync_all().map_err(io_err(&temp_path))?;
        }
        fs::rename(&temp_path, &final_path).map_err(io_err(&final_path))?;
        Ok(())
    }

    /// Read a block, verifying it still hashes to its id.
    pub fn get(&self, cid: &ContentId) -> Result<Vec<u8>> {
        self.try_get(cid)?.ok_or_else(|| ContentError::Missing {
            cid: cid.to_string(),
        })
    }

    pub fn try_get(&self, cid: &ContentId) -> Result<Option<Vec<u8>>> {
        let path = self.path_for(cid);
        if !path.is_file() {
            return Ok(None);
        }
        let data = fs::read(&path).map_err(io_err(&path))?;
        if !cid.verifies(&data) {
            // Corrupt on disk. Drop it so the node can refetch rather than
            // serving bad bytes to its peers.
            let _ = fs::remove_file(&path);
            return Err(ContentError::IntegrityFailure {
                cid: cid.to_string(),
            });
        }
        Ok(Some(data))
    }

    pub fn remove(&self, cid: &ContentId) -> Result<bool> {
        let path = self.path_for(cid);
        if !path.is_file() {
            return Ok(false);
        }
        fs::remove_file(&path).map_err(io_err(&path))?;
        Ok(true)
    }

    pub fn block_size(&self, cid: &ContentId) -> Option<u64> {
        fs::metadata(self.path_for(cid)).ok().map(|m| m.len())
    }

    pub fn stats(&self) -> Result<StoreStats> {
        let mut stats = StoreStats::default();
        let shards = match fs::read_dir(&self.root) {
            Ok(shards) => shards,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(stats),
            Err(e) => return Err(io_err(&self.root)(e)),
        };
        for shard in shards {
            let shard = shard.map_err(io_err(&self.root))?;
            if !shard.file_type().map_err(io_err(shard.path()))?.is_dir() {
                continue;
            }
            for entry in fs::read_dir(shard.path()).map_err(io_err(shard.path()))? {
                let entry = entry.map_err(io_err(shard.path()))?;
                let name = entry.file_name();
                if name.to_string_lossy().starts_with(".tmp-") {
                    continue;
                }
                let meta = entry.metadata().map_err(io_err(entry.path()))?;
                if meta.is_file() {
                    stats.block_count += 1;
                    stats.total_bytes += meta.len();
                }
            }
        }
        Ok(stats)
    }

    /// Every block held, with its size. Used by the cache accountant.
    pub fn list(&self) -> Result<Vec<(ContentId, u64)>> {
        let mut out = Vec::new();
        let shards = match fs::read_dir(&self.root) {
            Ok(shards) => shards,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(io_err(&self.root)(e)),
        };
        for shard in shards {
            let shard = shard.map_err(io_err(&self.root))?;
            if !shard.file_type().map_err(io_err(shard.path()))?.is_dir() {
                continue;
            }
            for entry in fs::read_dir(shard.path()).map_err(io_err(shard.path()))? {
                let entry = entry.map_err(io_err(shard.path()))?;
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with(".tmp-") {
                    continue;
                }
                if let Ok(cid) = ContentId::parse(&name) {
                    let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
                    out.push((cid, len));
                }
            }
        }
        Ok(out)
    }

    /// Split a file into chunks, store them and the manifest, and return the
    /// video's id. This is the whole of `ourvideo video publish <FILE>` on the
    /// storage side.
    pub fn import_file(&self, path: impl AsRef<Path>) -> Result<ImportedVideo> {
        let path = path.as_ref();
        let meta = fs::metadata(path).map_err(io_err(path))?;
        if meta.len() == 0 {
            return Err(ContentError::EmptyFile);
        }
        let file = fs::File::open(path).map_err(io_err(path))?;
        let mut reader = BufReader::new(file);
        let mut buffer = vec![0u8; CHUNK_SIZE];
        let mut chunks = Vec::new();
        let mut total = 0u64;

        loop {
            let mut filled = 0;
            // `read` is allowed to return short reads; fill the buffer fully so
            // that chunk boundaries depend on the file, not on the I/O layer.
            while filled < CHUNK_SIZE {
                match reader.read(&mut buffer[filled..]).map_err(io_err(path))? {
                    0 => break,
                    n => filled += n,
                }
            }
            if filled == 0 {
                break;
            }
            total += filled as u64;
            chunks.push(self.put_raw(&buffer[..filled])?);
            if filled < CHUNK_SIZE {
                break;
            }
        }

        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "video".to_string());
        let manifest = VideoManifest::new(
            guess_media_type(path),
            file_name,
            total,
            CHUNK_SIZE as u32,
            chunks,
        );
        let manifest_bytes = manifest.to_bytes()?;
        let content_id = self.put_dag_cbor(&manifest_bytes)?;
        tracing::info!(
            cid = %content_id,
            chunks = manifest.chunks.len(),
            bytes = total,
            "imported file into the block store"
        );
        Ok(ImportedVideo {
            content_id,
            manifest,
            manifest_bytes,
        })
    }

    /// Chunks from `manifest` that we do not hold yet, in fetch order.
    pub fn missing_chunks(&self, manifest: &VideoManifest) -> Vec<ContentId> {
        manifest
            .chunks
            .iter()
            .filter(|cid| !self.has(cid))
            .copied()
            .collect()
    }

    pub fn has_all_chunks(&self, manifest: &VideoManifest) -> bool {
        manifest.chunks.iter().all(|cid| self.has(cid))
    }

    /// Write the original file back out from its chunks.
    pub fn assemble(&self, manifest: &VideoManifest, out_path: impl AsRef<Path>) -> Result<u64> {
        let out_path = out_path.as_ref();
        if let Some(parent) = out_path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(io_err(parent))?;
            }
        }
        let temp_path = out_path.with_extension(format!(
            "ovn-partial-{}",
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let mut written = 0u64;
        {
            let mut out = fs::File::create(&temp_path).map_err(io_err(&temp_path))?;
            for (index, cid) in manifest.chunks.iter().enumerate() {
                let data = self.get(cid)?;
                let expected = manifest
                    .chunk_len(index)
                    .expect("index comes from the chunk list");
                if data.len() as u64 != expected {
                    let _ = fs::remove_file(&temp_path);
                    return Err(ContentError::MalformedManifest(format!(
                        "chunk {index} is {} bytes, manifest says {expected}",
                        data.len()
                    )));
                }
                out.write_all(&data).map_err(io_err(&temp_path))?;
                written += data.len() as u64;
            }
            out.sync_all().map_err(io_err(&temp_path))?;
        }
        if written != manifest.total_size {
            let _ = fs::remove_file(&temp_path);
            return Err(ContentError::MalformedManifest(format!(
                "assembled {written} bytes, manifest says {}",
                manifest.total_size
            )));
        }
        fs::rename(&temp_path, out_path).map_err(io_err(out_path))?;
        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, BlockStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = BlockStore::open(dir.path().join("blocks")).unwrap();
        (dir, store)
    }

    #[test]
    fn put_and_get_roundtrip() {
        let (_dir, store) = store();
        let cid = store.put_raw(b"some chunk").unwrap();
        assert!(store.has(&cid));
        assert_eq!(store.get(&cid).unwrap(), b"some chunk");
    }

    #[test]
    fn storing_the_same_bytes_twice_is_idempotent() {
        let (_dir, store) = store();
        let a = store.put_raw(b"dup").unwrap();
        let b = store.put_raw(b"dup").unwrap();
        assert_eq!(a, b);
        assert_eq!(store.stats().unwrap().block_count, 1);
    }

    #[test]
    fn a_peer_cannot_write_bytes_under_the_wrong_id() {
        let (_dir, store) = store();
        let cid = ContentId::from_raw(b"what we asked for");
        let err = store.put_verified(&cid, b"what they sent").unwrap_err();
        assert!(matches!(err, ContentError::IntegrityFailure { .. }));
        assert!(!store.has(&cid));
    }

    #[test]
    fn honest_bytes_are_accepted_under_their_id() {
        let (_dir, store) = store();
        let cid = ContentId::from_raw(b"honest");
        store.put_verified(&cid, b"honest").unwrap();
        assert_eq!(store.get(&cid).unwrap(), b"honest");
    }

    #[test]
    fn oversized_chunks_are_refused() {
        let (_dir, store) = store();
        let big = vec![0u8; CHUNK_SIZE + 1];
        assert!(matches!(
            store.put_raw(&big),
            Err(ContentError::ChunkTooLarge { .. })
        ));
    }

    #[test]
    fn corruption_on_disk_is_caught_on_read_and_the_block_is_dropped() {
        let (_dir, store) = store();
        let cid = store.put_raw(b"good").unwrap();
        fs::write(store.path_for(&cid), b"evil").unwrap();
        assert!(matches!(
            store.get(&cid),
            Err(ContentError::IntegrityFailure { .. })
        ));
        assert!(!store.has(&cid), "corrupt block should be removed");
    }

    #[test]
    fn missing_blocks_report_cleanly() {
        let (_dir, store) = store();
        let cid = ContentId::from_raw(b"never stored");
        assert!(matches!(store.get(&cid), Err(ContentError::Missing { .. })));
        assert_eq!(store.try_get(&cid).unwrap(), None);
    }

    #[test]
    fn import_then_assemble_reproduces_the_file() {
        let (dir, store) = store();
        // Two full chunks plus a partial one.
        let payload: Vec<u8> = (0..(CHUNK_SIZE * 2 + 1234))
            .map(|i| (i % 251) as u8)
            .collect();
        let source = dir.path().join("clip.mp4");
        fs::write(&source, &payload).unwrap();

        let imported = store.import_file(&source).unwrap();
        assert_eq!(imported.manifest.chunks.len(), 3);
        assert_eq!(imported.manifest.total_size, payload.len() as u64);
        assert_eq!(imported.manifest.media_type, "video/mp4");
        assert_eq!(imported.manifest.file_name, "clip.mp4");
        assert!(imported.manifest.validate().is_ok());
        assert_eq!(
            imported.content_id,
            ContentId::from_dag_cbor(&imported.manifest_bytes)
        );

        let out = dir.path().join("out").join("clip.mp4");
        let written = store.assemble(&imported.manifest, &out).unwrap();
        assert_eq!(written, payload.len() as u64);
        assert_eq!(fs::read(&out).unwrap(), payload);
    }

    #[test]
    fn a_file_exactly_one_chunk_long_imports_as_one_chunk() {
        let (dir, store) = store();
        let payload = vec![7u8; CHUNK_SIZE];
        let source = dir.path().join("exact.bin");
        fs::write(&source, &payload).unwrap();
        let imported = store.import_file(&source).unwrap();
        assert_eq!(imported.manifest.chunks.len(), 1);
        assert!(imported.manifest.validate().is_ok());
    }

    #[test]
    fn empty_files_are_refused() {
        let (dir, store) = store();
        let source = dir.path().join("empty.mp4");
        fs::write(&source, b"").unwrap();
        assert!(matches!(
            store.import_file(&source),
            Err(ContentError::EmptyFile)
        ));
    }

    #[test]
    fn identical_files_deduplicate_to_one_video_id() {
        let (dir, store) = store();
        fs::write(dir.path().join("a.mp4"), b"same bytes").unwrap();
        fs::write(dir.path().join("b.mp4"), b"same bytes").unwrap();
        let a = store.import_file(dir.path().join("a.mp4")).unwrap();
        let b = store.import_file(dir.path().join("b.mp4")).unwrap();
        // Chunks dedupe; the manifests differ only by file name.
        assert_eq!(a.manifest.chunks, b.manifest.chunks);
        assert_ne!(a.content_id, b.content_id);
    }

    #[test]
    fn missing_chunks_are_reported_for_a_partial_download() {
        let (dir, store) = store();
        let source = dir.path().join("v.mp4");
        fs::write(&source, vec![1u8; CHUNK_SIZE + 10]).unwrap();
        let imported = store.import_file(&source).unwrap();
        assert!(store.has_all_chunks(&imported.manifest));
        assert!(store.missing_chunks(&imported.manifest).is_empty());

        store.remove(&imported.manifest.chunks[1]).unwrap();
        assert!(!store.has_all_chunks(&imported.manifest));
        assert_eq!(
            store.missing_chunks(&imported.manifest),
            vec![imported.manifest.chunks[1]]
        );
        assert!(store
            .assemble(&imported.manifest, dir.path().join("o"))
            .is_err());
    }

    #[test]
    fn stats_and_list_agree() {
        let (_dir, store) = store();
        store.put_raw(b"one").unwrap();
        store.put_raw(b"two").unwrap();
        let stats = store.stats().unwrap();
        let listed = store.list().unwrap();
        assert_eq!(stats.block_count, 2);
        assert_eq!(listed.len(), 2);
        assert_eq!(
            stats.total_bytes,
            listed.iter().map(|(_, n)| n).sum::<u64>()
        );
    }

    #[test]
    fn remove_reports_whether_it_did_anything() {
        let (_dir, store) = store();
        let cid = store.put_raw(b"gone soon").unwrap();
        assert!(store.remove(&cid).unwrap());
        assert!(!store.remove(&cid).unwrap());
    }
}
