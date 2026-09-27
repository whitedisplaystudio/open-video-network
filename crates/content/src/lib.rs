//! Content addressing and local block storage.
//!
//! A published file is split into fixed-size chunks. Each chunk gets its own
//! [`ContentId`]; the list of chunk ids plus a Merkle root over them forms a
//! [`VideoManifest`], and the manifest's own id is the video's id. Nothing in
//! here knows about the network: it turns files into verifiable blocks and
//! back again.

mod manifest;
mod merkle;
mod probe;
mod store;

pub use manifest::{ImportedVideo, VideoManifest, MANIFEST_VERSION};
pub use merkle::{merkle_proof, merkle_root, verify_merkle_proof};
pub use probe::{guess_media_type, probe_duration_secs};
pub use store::{BlockStore, StoreStats};

pub use ovn_protocol::{ContentId, CHUNK_SIZE};

#[derive(Debug, thiserror::Error)]
pub enum ContentError {
    #[error("i/o error on {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("content {cid} did not match the received bytes")]
    IntegrityFailure { cid: String },
    #[error("block {cid} is not held locally")]
    Missing { cid: String },
    #[error("chunk is {actual} bytes, limit is {limit}")]
    ChunkTooLarge { actual: usize, limit: usize },
    #[error("manifest is malformed: {0}")]
    MalformedManifest(String),
    #[error("refusing to publish an empty file")]
    EmptyFile,
    #[error(transparent)]
    Protocol(#[from] ovn_protocol::ProtocolError),
}

pub type Result<T> = std::result::Result<T, ContentError>;

pub(crate) fn io_err(
    path: impl AsRef<std::path::Path>,
) -> impl FnOnce(std::io::Error) -> ContentError {
    let path = path.as_ref().display().to_string();
    move |source| ContentError::Io { path, source }
}
