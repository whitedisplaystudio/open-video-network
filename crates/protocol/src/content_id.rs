//! Content identifiers.
//!
//! Section 19: video is identified by what it *is*, not by where it lives. A
//! [`ContentId`] is a CIDv1 wrapping a SHA2-256 multihash, so the same bytes
//! produce the same identifier on every node and in every implementation.

use std::fmt;
use std::str::FromStr;

use cid::Cid;
use multihash::Multihash;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

use crate::{ProtocolError, Result, MAX_CONTENT_ID_LEN};

/// Multicodec for opaque bytes — used for content chunks.
pub const RAW_CODEC: u64 = 0x55;
/// Multicodec for DAG-CBOR — used for the video manifest.
pub const DAG_CBOR_CODEC: u64 = 0x71;
/// Multihash code for SHA2-256.
const SHA2_256: u64 = 0x12;

/// A CIDv1 over a SHA2-256 digest.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentId(Cid);

impl ContentId {
    /// Identify a chunk of opaque bytes.
    pub fn from_raw(data: &[u8]) -> Self {
        Self::build(RAW_CODEC, data)
    }

    /// Identify a DAG-CBOR document, such as a video manifest.
    pub fn from_dag_cbor(data: &[u8]) -> Self {
        Self::build(DAG_CBOR_CODEC, data)
    }

    fn build(codec: u64, data: &[u8]) -> Self {
        let digest = Sha256::digest(data);
        let mh = Multihash::<64>::wrap(SHA2_256, digest.as_slice())
            .expect("32-byte digest fits a 64-byte multihash");
        Self(Cid::new_v1(codec, mh))
    }

    /// Parse an identifier that arrived from the network or from a user.
    pub fn parse(s: &str) -> Result<Self> {
        if s.len() > MAX_CONTENT_ID_LEN {
            return Err(ProtocolError::FieldTooLarge {
                field: "contentId",
                actual: s.len(),
                limit: MAX_CONTENT_ID_LEN,
            });
        }
        let cid = Cid::try_from(s).map_err(|e| ProtocolError::InvalidContentId(e.to_string()))?;
        Self::from_cid(cid)
    }

    pub fn from_cid(cid: Cid) -> Result<Self> {
        if cid.version() != cid::Version::V1 {
            return Err(ProtocolError::InvalidContentId(format!(
                "expected CIDv1, found {:?}",
                cid.version()
            )));
        }
        if cid.hash().code() != SHA2_256 {
            return Err(ProtocolError::InvalidContentId(format!(
                "expected sha2-256 multihash (0x12), found 0x{:x}",
                cid.hash().code()
            )));
        }
        if cid.hash().size() != 32 {
            return Err(ProtocolError::InvalidContentId(format!(
                "expected a 32-byte digest, found {}",
                cid.hash().size()
            )));
        }
        match cid.codec() {
            RAW_CODEC | DAG_CBOR_CODEC => Ok(Self(cid)),
            other => Err(ProtocolError::InvalidContentId(format!(
                "unsupported codec 0x{other:x}"
            ))),
        }
    }

    pub fn codec(&self) -> u64 {
        self.0.codec()
    }

    pub fn is_manifest(&self) -> bool {
        self.0.codec() == DAG_CBOR_CODEC
    }

    /// The raw 32-byte SHA2-256 digest.
    pub fn digest(&self) -> &[u8] {
        self.0.hash().digest()
    }

    /// Binary form, used as a Kademlia key and as a SQLite primary key.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.0.to_bytes()
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let cid =
            Cid::try_from(bytes).map_err(|e| ProtocolError::InvalidContentId(e.to_string()))?;
        Self::from_cid(cid)
    }

    pub fn as_cid(&self) -> &Cid {
        &self.0
    }

    /// Recompute the digest of `data` and compare. This is the check in
    /// section 20: data that does not hash to what we asked for is discarded.
    pub fn verifies(&self, data: &[u8]) -> bool {
        let digest = Sha256::digest(data);
        digest.as_slice() == self.digest()
    }
}

impl fmt::Display for ContentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Debug for ContentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ContentId({})", self.0)
    }
}

impl FromStr for ContentId {
    type Err = ProtocolError;
    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

// On the wire a content id travels as its text form. It is a little larger
// than the binary form but it survives JSON, logs and copy-paste, which is
// what section 3 asks of anything a user might ever see.
impl Serialize for ContentId {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for ContentId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        ContentId::parse(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_bytes_produce_the_same_id() {
        assert_eq!(ContentId::from_raw(b"video"), ContentId::from_raw(b"video"));
        assert_ne!(ContentId::from_raw(b"video"), ContentId::from_raw(b"vide0"));
    }

    #[test]
    fn codec_distinguishes_chunks_from_manifests() {
        let chunk = ContentId::from_raw(b"x");
        let manifest = ContentId::from_dag_cbor(b"x");
        assert_ne!(chunk, manifest);
        assert!(!chunk.is_manifest());
        assert!(manifest.is_manifest());
    }

    #[test]
    fn text_form_roundtrips() {
        let id = ContentId::from_raw(b"hello world");
        let text = id.to_string();
        assert!(text.starts_with('b'), "CIDv1 base32 form: {text}");
        assert_eq!(ContentId::parse(&text).unwrap(), id);
    }

    #[test]
    fn binary_form_roundtrips() {
        let id = ContentId::from_dag_cbor(b"manifest");
        assert_eq!(ContentId::from_bytes(&id.to_bytes()).unwrap(), id);
    }

    #[test]
    fn verifies_detects_corruption() {
        let id = ContentId::from_raw(b"good chunk");
        assert!(id.verifies(b"good chunk"));
        assert!(!id.verifies(b"bad chunk"));
        assert!(!id.verifies(b""));
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(ContentId::parse("").is_err());
        assert!(ContentId::parse("not-a-cid").is_err());
        assert!(ContentId::parse(&"b".repeat(MAX_CONTENT_ID_LEN + 1)).is_err());
    }

    #[test]
    fn cidv0_is_rejected() {
        // A well-formed CIDv0 (base58 sha2-256, dag-pb) is still not ours.
        let v0 = "QmbWqxBEKC3P8tqsKc98xmWNzrzDtRLMiMPL8wBuTGsMnR";
        assert!(ContentId::parse(v0).is_err());
    }

    #[test]
    fn serde_roundtrip_through_cbor() {
        let id = ContentId::from_raw(b"serde");
        let bytes = crate::to_cbor_vec(&id).unwrap();
        let back: ContentId = crate::from_cbor_slice(&bytes).unwrap();
        assert_eq!(back, id);
    }
}
