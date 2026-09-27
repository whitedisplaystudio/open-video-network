# Security

## Reporting a vulnerability

Please do not open a public issue. Report it privately to the maintainers,
with enough detail to reproduce it. You will get an acknowledgement, and
credit in the fix unless you would rather not have it.

A way for personal viewing data to leave the device counts as a
vulnerability, not a bug.

## Threat model

Everything that arrives from the network is written by someone who may be
hostile. Assume:

* peers lie about who they are;
* peers serve bytes that are not what you asked for;
* peers send malformed, oversized and adversarially shaped messages;
* peers flood you;
* a web server that hands out a node descriptor is not trustworthy;
* several peers may be one person.

What is **not** in the V1 threat model:

* an attacker with local filesystem access — they have your identity key;
* global traffic analysis;
* hiding that you are on the network at all.

## The checks

### Identity and authenticity

Every piece of metadata is signed with Ed25519 and verified before it is
stored or acted on. Signatures cover a canonical CBOR array with a domain
separation tag first, so a signature over one message type cannot be replayed
as a signature over another.

A node descriptor carries both a public key and a peer id, and the peer id
must be derivable from the key. That binding is what makes it safe to fetch a
descriptor over plain HTTP: a hostile server can serve a descriptor, but not
somebody else's identity.

A tampered announcement is discarded and never relayed. An announcement older
than one already held for the same content id is ignored, so an old version
cannot be replayed over a correction.

### Content integrity

Content is addressed by SHA2-256. Bytes received from a peer are hashed and
compared against the identifier they were requested under, before anything is
written. A peer that fails this is skipped and the next provider is tried.

Blocks are verified on read as well as write. A block that no longer matches
its identifier is deleted rather than served on — this catches disk corruption
as well as a bug.

Manifests are validated structurally as well: chunk count, chunk size, size
consistency, and a recomputed Merkle root.

### Resource limits

Every limit is in section 11 of the
[protocol specification](../protocol/SPECIFICATION.md) and is applied **before
allocating**:

* message sizes are checked before parsing, not after;
* gossip is capped at 128 KiB and content chunks at 1 MiB;
* a node descriptor body is capped at 16 KiB, using the declared length where
  there is one and the actual length regardless;
* a manifest may not claim more than 65 536 chunks;
* the stored address list for a peer is bounded, so a peer announcing a new
  ephemeral port every second cannot grow the database without limit.

### Rate limiting

Each peer gets a token bucket for inbound gossip and another for block
requests. A peer that empties its bucket is ignored until it refills. Buckets
are keyed by peer id and survive disconnects, so reconnecting is not a way
around the limit. Idle buckets are pruned so that a long-lived node does not
accumulate state for every peer it has ever met.

Connections are capped in total and per peer, enforced by libp2p's
connection-limits behaviour before anything else in the stack sees the
connection.

### Parsing

CBOR is parsed by `ciborium`, a safe-Rust decoder. Malformed input returns an
error; it does not panic and does not allocate on a declared length. Content
identifiers are validated for version, multihash, digest length and codec, and
anything else is rejected.

Unknown fields and unknown enumerated values are ignored rather than rejected,
so a newer peer's extra field is not a parse failure — but that tolerance
applies only to *ignoring*, never to acting on something unverified.

### Cryptography

No primitive is implemented here. Ed25519 comes from `libp2p-identity`,
SHA2-256 from the RustCrypto `sha2` crate, and transport security from
`libp2p`'s Noise and QUIC/TLS implementations. If a primitive appears to be
missing, the answer is a reviewed library, not a new implementation.

Secret keys are written with `0600` permissions and never appear in a
protocol message, a log line, or a `Debug` rendering — `Identity`'s `Debug`
impl prints only the public key, and a test asserts the secret is absent.

### The local API

Bound to loopback. Bearer token from `runtime.json`, which is `0600`. Token
comparison is length-checked and constant-time over the bytes. One route is
unauthenticated by design — the public node descriptor.

### Paths

A manifest's `fileName` comes from a stranger. It is sanitised to a single
path component before being joined to the downloads directory; a test asserts
that a range of hostile names — traversal, absolute paths, separators, nulls —
all produce exactly one component under the intended directory.

## Known weaknesses in V1

Stated plainly because the alternative is someone discovering them later:

* **Sybil resistance is limited.** Identities are free. A single party can run
  many nodes. V1 relies on content addressing and signatures, which make
  content unforgeable but do not make identities scarce.
* **Eclipse attacks on the DHT are possible.** Kademlia gives no strong
  guarantee that a node's routing table is not dominated by one attacker.
* **No transport-level anonymity.** Fetching a video makes you a visible
  provider for it.
* **No moderation beyond the local device.** See
  [`MODERATION.md`](MODERATION.md).
* **Announcement spam is rate-limited, not prevented.** A patient attacker
  within the rate limits can still fill your local index with junk.

## Dependencies

The dependency tree is deliberately small and the versions are pinned in
`Cargo.lock`. Run `cargo audit` before a release. A new dependency in a pull
request should come with a sentence about why it is worth the added surface.
