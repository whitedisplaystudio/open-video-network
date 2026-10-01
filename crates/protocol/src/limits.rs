//! Hard limits applied to every value that arrives from the network.
//!
//! Section 31 of the design requires that all network input be treated as
//! untrusted. These constants are part of the wire specification, not local
//! policy: a node that exceeds them is sending an invalid message, and every
//! implementation must reject it the same way.

/// Largest accepted encoded message, including the envelope and signature.
pub const MAX_MESSAGE_SIZE: usize = 1024 * 1024;
/// Largest accepted encoded gossip payload.
pub const MAX_GOSSIP_MESSAGE_SIZE: usize = 128 * 1024;
/// Largest accepted node descriptor document.
pub const MAX_DESCRIPTOR_SIZE: usize = 16 * 1024;

pub const MAX_TITLE_LEN: usize = 512;
pub const MAX_DESCRIPTION_LEN: usize = 8192;
pub const MAX_TAGS: usize = 32;
pub const MAX_TAG_LEN: usize = 64;
pub const MAX_NODE_NAME_LEN: usize = 128;
pub const MAX_ADDRESSES: usize = 16;
pub const MAX_ADDRESS_LEN: usize = 256;
pub const MAX_CAPABILITIES: usize = 16;
pub const MAX_CAPABILITY_LEN: usize = 32;
pub const MAX_CONTENT_ID_LEN: usize = 128;
pub const MAX_DISPLAY_NAME_LEN: usize = 128;
pub const MAX_BIO_LEN: usize = 2048;
/// A URL long enough for any real one, short enough that an announcement
/// cannot be padded out with it.
pub const MAX_SOURCE_URL_LEN: usize = 2048;
pub const MAX_QUERY_LEN: usize = 256;
pub const MAX_QUERY_RESULTS: usize = 64;
pub const MAX_PROVIDERS_PER_RESPONSE: usize = 32;

/// Longest video duration we will accept in an announcement (7 days).
pub const MAX_DURATION_SECS: u64 = 7 * 24 * 60 * 60;

/// How far into the future a timestamp may be before we call it a lie.
pub const MAX_CLOCK_SKEW_SECS: u64 = 300;

/// Chunk size used when a file is imported. Also the ceiling on a chunk we
/// will accept from a peer.
pub const CHUNK_SIZE: usize = 1024 * 1024;
