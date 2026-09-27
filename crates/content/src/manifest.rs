//! The video manifest: the document a video's [`ContentId`] actually names.

use serde::{Deserialize, Serialize};

use ovn_protocol::{from_cbor_slice, to_cbor_vec, ContentId, CHUNK_SIZE};

use crate::merkle::merkle_root;
use crate::{ContentError, Result};

pub const MANIFEST_VERSION: u16 = 1;

/// Largest number of chunks in one video: 64 Ki chunks of 1 MiB is 64 GiB,
/// comfortably past anything V1 needs and still a bounded allocation when the
/// manifest arrives from a stranger.
const MAX_CHUNKS: usize = 65_536;

/// Describes how to reassemble a file from chunks.
///
/// The manifest is stored and transferred as CBOR. Its [`ContentId`] is the
/// hash of those exact bytes, so there is no canonicalisation step and no way
/// for two nodes to disagree about a video's identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoManifest {
    pub version: u16,
    /// Media type of the original file, e.g. `video/mp4`.
    pub media_type: String,
    /// Original file name, for display and for writing the file back out.
    pub file_name: String,
    pub total_size: u64,
    pub chunk_size: u32,
    pub chunks: Vec<ContentId>,
    /// Hex-encoded Merkle root over the chunk list.
    pub merkle_root: String,
}

impl VideoManifest {
    pub fn new(
        media_type: impl Into<String>,
        file_name: impl Into<String>,
        total_size: u64,
        chunk_size: u32,
        chunks: Vec<ContentId>,
    ) -> Self {
        let root = merkle_root(&chunks);
        Self {
            version: MANIFEST_VERSION,
            media_type: media_type.into(),
            file_name: file_name.into(),
            total_size,
            chunk_size,
            chunks,
            merkle_root: hex::encode(root),
        }
    }

    /// CBOR bytes. The manifest's id is the hash of these.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        Ok(to_cbor_vec(self)?)
    }

    /// The video's [`ContentId`].
    pub fn content_id(&self) -> Result<ContentId> {
        Ok(ContentId::from_dag_cbor(&self.to_bytes()?))
    }

    /// Parse manifest bytes received from a peer and check they are coherent.
    /// The caller must already have verified that `bytes` hash to the expected
    /// id; this checks that the contents make sense.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let manifest: Self = from_cbor_slice(bytes)?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != MANIFEST_VERSION {
            return Err(ContentError::MalformedManifest(format!(
                "unsupported manifest version {}",
                self.version
            )));
        }
        if self.chunks.is_empty() {
            return Err(ContentError::MalformedManifest("no chunks".into()));
        }
        if self.chunks.len() > MAX_CHUNKS {
            return Err(ContentError::MalformedManifest(format!(
                "{} chunks exceeds the limit of {MAX_CHUNKS}",
                self.chunks.len()
            )));
        }
        if self.chunk_size == 0 || self.chunk_size as usize > CHUNK_SIZE {
            return Err(ContentError::MalformedManifest(format!(
                "chunk size {} is outside 1..={CHUNK_SIZE}",
                self.chunk_size
            )));
        }
        if self.chunks.iter().any(|c| c.is_manifest()) {
            return Err(ContentError::MalformedManifest(
                "chunk list contains a manifest id".into(),
            ));
        }
        // total_size must be consistent with the chunk count: every chunk but
        // the last is full, the last is in 1..=chunk_size.
        let full = (self.chunks.len() as u64 - 1) * self.chunk_size as u64;
        if self.total_size <= full || self.total_size > full + self.chunk_size as u64 {
            return Err(ContentError::MalformedManifest(format!(
                "total size {} is inconsistent with {} chunks of {}",
                self.total_size,
                self.chunks.len(),
                self.chunk_size
            )));
        }
        let expected = hex::encode(merkle_root(&self.chunks));
        if self.merkle_root != expected {
            return Err(ContentError::MalformedManifest(
                "merkle root does not match the chunk list".into(),
            ));
        }
        Ok(())
    }

    /// Byte length of chunk `index`, given the total size.
    pub fn chunk_len(&self, index: usize) -> Option<u64> {
        if index >= self.chunks.len() {
            return None;
        }
        let offset = index as u64 * self.chunk_size as u64;
        Some((self.total_size - offset).min(self.chunk_size as u64))
    }
}

/// What [`crate::BlockStore::import_file`] produces.
#[derive(Clone, Debug)]
pub struct ImportedVideo {
    pub content_id: ContentId,
    pub manifest: VideoManifest,
    /// Bytes of the manifest block, already stored.
    pub manifest_bytes: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(chunk_count: usize, chunk_size: u32, total: u64) -> VideoManifest {
        let chunks = (0..chunk_count)
            .map(|i| ContentId::from_raw(format!("c{i}").as_bytes()))
            .collect();
        VideoManifest::new("video/mp4", "a.mp4", total, chunk_size, chunks)
    }

    #[test]
    fn a_well_formed_manifest_validates() {
        assert!(manifest(3, 1000, 2500).validate().is_ok());
        assert!(manifest(1, 1000, 1).validate().is_ok());
        assert!(manifest(1, 1000, 1000).validate().is_ok());
    }

    #[test]
    fn id_is_the_hash_of_the_encoded_bytes() {
        let m = manifest(3, 1000, 2500);
        let bytes = m.to_bytes().unwrap();
        assert_eq!(m.content_id().unwrap(), ContentId::from_dag_cbor(&bytes));
        assert!(m.content_id().unwrap().is_manifest());
    }

    #[test]
    fn roundtrips_through_bytes() {
        let m = manifest(4, 512, 1700);
        let back = VideoManifest::from_bytes(&m.to_bytes().unwrap()).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn inconsistent_total_size_is_rejected() {
        // 3 chunks of 1000 must total 2001..=3000.
        let mut m = manifest(3, 1000, 2500);
        m.total_size = 1500;
        assert!(m.validate().is_err());
        m.total_size = 3001;
        assert!(m.validate().is_err());
    }

    #[test]
    fn a_forged_merkle_root_is_rejected() {
        let mut m = manifest(3, 1000, 2500);
        m.merkle_root = hex::encode([9u8; 32]);
        assert!(m.validate().is_err());
    }

    #[test]
    fn reordered_chunks_are_rejected_by_the_root() {
        let mut m = manifest(4, 1000, 3500);
        m.chunks.swap(0, 2);
        assert!(m.validate().is_err());
    }

    #[test]
    fn empty_and_oversized_chunk_lists_are_rejected() {
        let mut m = manifest(2, 1000, 1500);
        m.chunks.clear();
        assert!(m.validate().is_err());

        let mut m = manifest(2, 1000, 1500);
        m.chunk_size = (CHUNK_SIZE + 1) as u32;
        assert!(m.validate().is_err());
    }

    #[test]
    fn a_manifest_id_cannot_appear_in_the_chunk_list() {
        let mut m = manifest(2, 1000, 1500);
        m.chunks[1] = ContentId::from_dag_cbor(b"nested");
        m.merkle_root = hex::encode(crate::merkle_root(&m.chunks));
        assert!(m.validate().is_err());
    }

    #[test]
    fn chunk_len_accounts_for_a_short_tail() {
        let m = manifest(3, 1000, 2500);
        assert_eq!(m.chunk_len(0), Some(1000));
        assert_eq!(m.chunk_len(1), Some(1000));
        assert_eq!(m.chunk_len(2), Some(500));
        assert_eq!(m.chunk_len(3), None);
    }
}
