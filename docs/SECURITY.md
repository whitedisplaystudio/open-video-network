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

Every decoder between a stranger's bytes and this node's state is fuzzed:
announcements, profiles, descriptors, envelopes, manifests, content
identifiers, share links and `Range` headers. Valid messages are mutated —
bits flipped, lengths forged, bodies truncated and spliced — and fed back in,
alongside pure noise. The contract asserted is narrow and absolute: never
panic, never hang, never allocate without bound. Returning an error is always
an acceptable answer; crashing is not, because a peer that can crash one node
can crash every node. See `crates/node/tests/untrusted_input.rs`.

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

### The local API and the web UI

Bound to loopback. Bearer token from `runtime.json`, which is `0600`. Token
comparison is length-checked and constant-time over the bytes. Browsers
authenticate with an `HttpOnly`, `SameSite=Strict` cookie exchanged from the
same token, because a page cannot add a header to a `<video src>` or an
`EventSource`.

Unauthenticated by design: the public node descriptor, and the two UI pages
with their assets, which contain no data.

Three further checks matter here, because this server is same-origin with a
browser page:

* **Host checking.** Every request is refused unless its `Host` is a loopback
  name. Binding to `127.0.0.1` does not by itself stop DNS rebinding — an
  attacker's domain can be made to resolve here, and then their page is
  same-origin with the node. Checking the name they asked for closes that.
* **A media-type allowlist.** A stream's `Content-Type` comes from a manifest
  a stranger wrote. Only known video, audio and image types are echoed back;
  anything else is `application/octet-stream`, with `nosniff`. Otherwise a
  peer could publish `text/html` and have it run in the UI's origin.
  Thumbnails are additionally checked to begin and end with JPEG markers.
* **A strict Content-Security-Policy.** The pages may load nothing from any
  other origin and may not use inline scripts or styles, so a string that
  escaped one of the DOM builders still could not execute.

Uploads through the browser are streamed to a staged file rather than
buffered, capped at 8 GiB, and the staged copy is removed once the content is
in the block store. The file name is sanitised to a single path component
before it touches the filesystem.

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

## Fetching a stranger's URL

An announcement names where its file is served from, and a node fetches it
because the announcement said to. That is a request made on a viewer's behalf to
an address a stranger chose, so:

* only `https` and `http` are fetched;
* a URL whose host is a **literal** loopback, private, link-local or
  unspecified address is refused, so publishing `http://192.168.0.1/` cannot be
  used to make strangers' nodes probe their own networks;
* every hop of a redirect is checked the same way, not just the signed URL;
* redirects are capped, so a chain cannot be used as a probe either.

**The limit, stated rather than pretended away:** a *hostname* that resolves to
a private address is not caught. Closing that needs the resolved address at
connect time, which the HTTP client does not currently expose. On a home network
the practical consequence is that a node can be made to issue one GET to
something behind the router and learn whether it answered — not read the
response, which still has to hash to what the creator signed.

`--allow-private-sources` turns the address check off, for the case it exists
for: somebody serving to their own household. It re-opens what it was closing,
which is why it is off by default.

## Impersonation

What is cryptographically impossible here, and tested:

* publishing under somebody else's identity — every announcement is signed,
  and an unsigned or wrongly signed one is discarded rather than stored or
  relayed;
* altering an announcement in flight;
* attaching your own name to somebody else's key in a channel link, because the
  display name is inside the signature;
* claiming another identity in a node descriptor, because the peer id must be
  derivable from the public key.

What remains possible, and cannot be removed:

**Anyone may use anyone's display name.** Generate a fresh key, set the name to
somebody else's, and to a reader the two are the same. No key theft is
involved. This cannot be prevented, because preventing it means a register of
names that decides who may use which, and that register would be the central
authority Principle 1 exists to do without. A name is human-meaningful and the
system is decentralised; the third property, that a name identifies exactly
one party, is the one that cannot also hold.

What the software does instead:

* **The key is shown wherever the name is.** A short fingerprint beside the
  display name, and the full key on the channel page. The key is what a
  subscription is to; the name is a label on it.
* **A collision is reported when it happens.** Subscribing to a name this
  device already knows under a different key warns, names the other key, and
  marks both rows in the channel list from then on. Matching is
  case-insensitive with whitespace collapsed.
* Subscribing is not refused. A name is not owned, so a collision is a thing
  to be told about, not an error.

The honest limits of that:

* It catches an **identical** name. It does not catch one that merely looks
  similar — a Cyrillic `а`, a full-width character, `Studio А` with a
  different letter. Comparing the key is the only reliable check.
* It only knows about identities **this device** has seen. A first encounter
  with an impostor, having never seen the real one, looks like nothing unusual.
* A **stolen identity key** is indistinguishable from its owner, and there is
  no revocation: revoking means telling an authority, and there isn't one.
  Back up `identity.key` and keep it as private as an SSH key.
