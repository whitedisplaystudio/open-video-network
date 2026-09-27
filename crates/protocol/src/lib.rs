//! The Open Video Network wire protocol.
//!
//! Everything a second implementation needs to speak to a Rust node lives in
//! this crate: the message types, the canonical byte encoding that signatures
//! are computed over, the content identifier format, and the validation limits
//! that make untrusted input safe to parse.
//!
//! Two rules shape the design:
//!
//! * **Nothing personal is representable here.** Watch history, watch ratios,
//!   preference vectors and recommendation scores have no type in this crate,
//!   so no amount of refactoring can accidentally put them on the wire.
//! * **Canonical encoding is explicit.** Signatures are computed over a CBOR
//!   array with a fixed field order and a domain separation tag, not over
//!   whatever a particular serialiser happens to emit. See
//!   `protocol/SPECIFICATION.md`.

mod announcement;
mod codec;
mod content_id;
mod descriptor;
mod envelope;
mod limits;

pub use announcement::{NewVideo, ProfileUpdate, VideoAnnouncement};
pub use codec::{from_cbor_slice, to_cbor_vec};
pub use content_id::{ContentId, DAG_CBOR_CODEC, RAW_CODEC};
pub use descriptor::{Capability, NodeDescriptor};
pub use envelope::{Envelope, MessageType, VideoProvider, VideoQuery};
pub use limits::*;

/// The protocol version this implementation speaks.
pub const PROTOCOL_VERSION: u16 = 1;

/// The lowest protocol version this implementation will talk to.
pub const MIN_SUPPORTED_PROTOCOL_VERSION: u16 = 1;

/// libp2p GossipSub topic carrying [`VideoAnnouncement`]s.
pub const TOPIC_VIDEO_ANNOUNCE: &str = "/ovn/video-announce/1";
/// libp2p GossipSub topic carrying [`ProfileUpdate`]s.
pub const TOPIC_PROFILE_UPDATE: &str = "/ovn/profile-update/1";
/// libp2p request-response protocol used to fetch content chunks.
pub const PROTOCOL_CHUNK: &str = "/ovn/chunk/1.0.0";
/// libp2p identify protocol name.
pub const PROTOCOL_IDENTIFY: &str = "/ovn/1.0.0";
/// Kademlia protocol name. Keeping our own name stops us from polluting, and
/// being polluted by, the public IPFS DHT.
pub const PROTOCOL_KADEMLIA: &str = "/ovn/kad/1.0.0";

/// Well-known HTTPS path where a node publishes its [`NodeDescriptor`], so that
/// `ourvideo peer add https://video.example.jp` needs nothing but a hostname.
pub const WELL_KNOWN_DESCRIPTOR_PATH: &str = "/.well-known/ovn/node.json";

/// URI scheme for share links that carry a descriptor inline.
pub const SHARE_LINK_SCHEME: &str = "ourvideo";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("unsupported protocol version {found} (this node speaks {min}..={max})")]
    UnsupportedVersion { found: u16, min: u16, max: u16 },
    #[error("field `{field}` exceeds its limit: {actual} > {limit}")]
    FieldTooLarge {
        field: &'static str,
        actual: usize,
        limit: usize,
    },
    #[error("field `{0}` is empty")]
    FieldEmpty(&'static str),
    #[error("invalid content id: {0}")]
    InvalidContentId(String),
    #[error("invalid public key")]
    InvalidPublicKey,
    #[error("invalid signature length {0}, expected 64")]
    InvalidSignatureLength(usize),
    #[error("signature verification failed")]
    BadSignature,
    #[error("timestamp {timestamp} is {skew}s in the future (max {max}s)")]
    TimestampInFuture { timestamp: u64, skew: u64, max: u64 },
    #[error("message is {actual} bytes, limit is {limit}")]
    MessageTooLarge { actual: usize, limit: usize },
    #[error("malformed CBOR: {0}")]
    MalformedCbor(String),
}

pub type Result<T> = std::result::Result<T, ProtocolError>;

/// Check a peer's advertised protocol version against what we can speak.
pub fn check_version(found: u16) -> Result<()> {
    if found < MIN_SUPPORTED_PROTOCOL_VERSION || found > PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedVersion {
            found,
            min: MIN_SUPPORTED_PROTOCOL_VERSION,
            max: PROTOCOL_VERSION,
        });
    }
    Ok(())
}

/// Seconds since the Unix epoch.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Normalise a tag to its canonical form: trimmed, lowercased, inner
/// whitespace collapsed to `-`.
///
/// Tags are the input to the local recommendation model, so two nodes must
/// agree on what "Gaming" and "gaming" mean.
pub fn normalize_tag(tag: &str) -> String {
    let mut out = String::with_capacity(tag.len());
    let mut pending_sep = false;
    for ch in tag.trim().chars() {
        if ch.is_whitespace() || ch == '_' {
            pending_sep = !out.is_empty();
            continue;
        }
        if pending_sep {
            out.push('-');
            pending_sep = false;
        }
        for lower in ch.to_lowercase() {
            out.push(lower);
        }
    }
    out
}

pub(crate) fn check_len(field: &'static str, actual: usize, limit: usize) -> Result<()> {
    if actual > limit {
        return Err(ProtocolError::FieldTooLarge {
            field,
            actual,
            limit,
        });
    }
    Ok(())
}

pub(crate) fn check_not_future(timestamp: u64) -> Result<()> {
    let now = now_secs();
    if timestamp > now.saturating_add(MAX_CLOCK_SKEW_SECS) {
        return Err(ProtocolError::TimestampInFuture {
            timestamp,
            skew: timestamp - now,
            max: MAX_CLOCK_SKEW_SECS,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_normalise_to_a_shared_form() {
        assert_eq!(normalize_tag("  Gaming "), "gaming");
        assert_eq!(normalize_tag("Indie Game"), "indie-game");
        assert_eq!(normalize_tag("rust_lang"), "rust-lang");
        assert_eq!(normalize_tag("ゲーム"), "ゲーム");
        assert_eq!(normalize_tag("   "), "");
    }

    #[test]
    fn version_check_rejects_unknown_versions() {
        assert!(check_version(PROTOCOL_VERSION).is_ok());
        assert!(check_version(0).is_err());
        assert!(check_version(PROTOCOL_VERSION + 1).is_err());
    }
}
