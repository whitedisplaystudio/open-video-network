# Open Video Network — Protocol Specification

**Version 1** · Status: draft · Protocol version field value: `1`

This document specifies everything a node must do to interoperate. It is
deliberately independent of the reference implementation: a node written in
Go, C++, Java or anything else that follows this document is a full member of
the network. Rust is a reference implementation, not the protocol.

Where this document and the Rust code disagree, this document is wrong and
should be fixed — but the Rust code has a test for nearly everything below, so
check those first (`crates/protocol/src/`).

---

## 1. Conventions

* "MUST", "MUST NOT", "SHOULD" and "MAY" are used in the usual sense.
* Byte strings are CBOR major type 2. Text strings are CBOR major type 3 and
  MUST be valid UTF-8.
* Timestamps are unsigned integers: seconds since the Unix epoch, UTC.
* "Public key" always means a raw 32-byte Ed25519 public key, never a wrapped
  or multibase form.
* Hex means lowercase hexadecimal.

---

## 2. Identity

A participant is an **Ed25519 key pair**. The public key is the identity.
There is no registration and no authority.

The libp2p `PeerId` MUST be derived from the same key, as libp2p specifies for
Ed25519: the protobuf-encoded public key, wrapped in an identity multihash,
base58btc encoded. A node therefore cannot present one identity to the
application layer and another to the transport.

Signatures are detached Ed25519 signatures, 64 bytes, over the canonical bytes
defined in section 3.

Private keys MUST NOT appear in any protocol message.

---

## 3. Canonical signing encoding

Signatures are **not** computed over a serialised struct. They are computed
over a **definite-length CBOR array** with a fixed element order, whose first
element is a **domain separation tag**. This removes any dependence on how a
particular library orders map keys, and stops a signature over one message
type being replayed as a signature over another.

```
signing_bytes = CBOR( [ domain_tag, field_1, field_2, ..., field_n ] )
```

Rules:

* The array MUST be definite-length.
* Integers MUST use the shortest CBOR encoding (canonical integer form).
* An absent optional field is encoded as CBOR `null`, never omitted.
* A content identifier is encoded as its **text form** (section 5), not as
  bytes.
* The `signature` field itself is never part of the signed bytes.

Domain tags defined by version 1:

| Message | Domain tag |
| --- | --- |
| Video announcement | `ovn/video-announce/v1` |
| Profile update | `ovn/profile-update/v1` |
| Node descriptor | `ovn/node-descriptor/v1` |
| Peer message envelope | `ovn/envelope/v1` |

A verifier MUST reconstruct the signing bytes from the fields it received and
check the signature against them. It MUST NOT trust a signature over bytes
supplied by the sender.

---

## 4. Message encoding

Protocol messages are encoded as **CBOR maps with text keys**, in the field
names given below (camelCase).

* A decoder MUST ignore keys it does not recognise (section 12).
* A decoder MUST reject a message larger than the applicable size limit
  (section 11) **before** parsing it.
* A decoder MUST NOT allocate based on a length declared in the input before
  checking it against these limits.

The reference implementation uses `serde` with `ciborium`; any conformant CBOR
library works.

---

## 5. Content identifiers

Content is identified by what it is, never by where it is.

A content identifier is a **CIDv1** with:

* multihash `sha2-256` (code `0x12`), digest length 32;
* multicodec `raw` (`0x55`) for a content **chunk**;
* multicodec `dag-cbor` (`0x71`) for a **manifest**.

The text form is the standard CIDv1 base32 encoding (a `b` prefix). The binary
form is the standard CID byte encoding and is what is used as a Kademlia
provider key.

A node MUST reject:

* CIDv0;
* any multihash other than `sha2-256` with a 32-byte digest;
* any multicodec other than `raw` or `dag-cbor`;
* a text form longer than 128 characters.

A `videoCid` MUST use the `dag-cbor` codec: a video is a manifest, not a bare
chunk.

---

## 6. Content model

### 6.1 Chunking

A published file is split into fixed-size chunks of **1 048 576 bytes**
(1 MiB). Every chunk except the last is exactly that size; the last is in
`1..=1048576`. Each chunk's identifier is the `raw` CID of its bytes.

A node MUST reject any chunk larger than 1 MiB.

### 6.2 Merkle root

The manifest carries a Merkle root over the chunk list, so that membership can
be proved without transferring the whole list.

```
leaf(i)      = SHA-256( 0x00 || chunk_digest(i) )
node(l, r)   = SHA-256( 0x01 || l || r )
```

where `chunk_digest(i)` is the 32-byte digest inside chunk `i`'s CID. Pairs
are combined left to right; a level with an odd number of nodes promotes the
last node by combining it with itself, `node(x, x)`. The root of an empty list
is 32 zero bytes. The leaf and interior prefixes MUST be included: they are
what prevents an interior hash being presented as a leaf.

### 6.3 Manifest

A manifest is a CBOR map. Its `dag-cbor` CID over **the exact bytes
transferred** is the video's identifier; there is no canonicalisation step, so
two nodes can never disagree about a video's identity.

| Field | Type | Notes |
| --- | --- | --- |
| `version` | uint | `1` |
| `mediaType` | text | e.g. `video/mp4`. A display hint only. |
| `fileName` | text | Original name. Untrusted: sanitise before use as a path. |
| `totalSize` | uint | Bytes in the original file. |
| `chunkSize` | uint | `1048576` in version 1. |
| `chunks` | array of text | Chunk CIDs, in file order. |
| `merkleRoot` | text | Hex, lowercase, of section 6.2. |

A receiver MUST reject a manifest where:

* `version` is not 1;
* `chunks` is empty or longer than 65 536;
* `chunkSize` is zero or greater than 1 MiB;
* any entry in `chunks` is a `dag-cbor` CID;
* `totalSize` is not in `((n-1) × chunkSize, (n-1) × chunkSize + chunkSize]`
  for `n = len(chunks)`;
* `merkleRoot` does not match a recomputation over `chunks`.

---

## 7. Messages

### 7.1 VideoAnnouncement

Announces that a video exists. Carries metadata only — never content.

| Field | Type | Signed order | Notes |
| --- | --- | --- | --- |
| `version` | uint | 1 | Protocol version. |
| `videoCid` | text | 2 | `dag-cbor` CID of the manifest. |
| `creatorPublicKey` | bytes | 3 | 32 bytes. |
| `title` | text | 4 | Non-empty after trimming, ≤ 512 bytes. |
| `description` | text | 5 | ≤ 8192 bytes. Default `""`. |
| `tags` | array of text | 6 | ≤ 32 entries, each ≤ 64 bytes, normalised (section 8). |
| `durationSecs` | uint | 7 | ≤ 604 800. `0` means unknown. |
| `thumbnailCid` | text or null | 8 | Optional. |
| `createdAt` | uint | 9 | Unix seconds. |
| `signature` | bytes | — | 64 bytes over `[tag, 1..9]`. |

Signing bytes: `[ "ovn/video-announce/v1", version, videoCid, creatorPublicKey,
title, description, tags, durationSecs, thumbnailCid, createdAt ]`.

A receiver MUST discard an announcement that fails any structural check or
whose signature does not verify. It MUST NOT store it and MUST NOT relay it.

When a node already holds an announcement for the same `videoCid`, it MUST
ignore one with a `createdAt` less than or equal to what it holds. This stops
a replayed older announcement from undoing a correction.

### 7.2 ProfileUpdate

| Field | Type | Signed order |
| --- | --- | --- |
| `version` | uint | 1 |
| `publicKey` | bytes | 2 |
| `displayName` | text | 3 (non-empty, ≤ 128 bytes) |
| `bio` | text | 4 (≤ 2048 bytes) |
| `avatarCid` | text or null | 5 |
| `updatedAt` | uint | 6 |
| `signature` | bytes | — |

Domain tag `ovn/profile-update/v1`. A receiver MUST ignore an update whose
`updatedAt` is not newer than one it already holds for that key.

### 7.3 NodeDescriptor

How a URL or a link becomes a peer.

| Field | Type | Signed order |
| --- | --- | --- |
| `protocolVersion` | uint | 1 |
| `nodeName` | text | 2 (≤ 128 bytes) |
| `peerId` | text | 3 (base58btc) |
| `publicKey` | bytes | 4 (32 bytes) |
| `addresses` | array of text | 5 (≤ 16 multiaddrs, each ≤ 256 bytes) |
| `capabilities` | array of text | 6 (≤ 16, each ≤ 32 bytes) |
| `createdAt` | uint | 7 |
| `signature` | bytes | — |

Domain tag `ovn/node-descriptor/v1`.

A verifier MUST check, in this order:

1. `protocolVersion` is supported;
2. the size limits above;
3. `createdAt` is not more than 300 seconds in the future;
4. `publicKey` is a valid Ed25519 public key;
5. **`peerId` equals the peer id derived from `publicKey`**;
6. the signature verifies.

Step 5 is what makes a descriptor safe to fetch over plain HTTP: a hostile web
server can serve a descriptor, but it cannot serve somebody else's identity.

Capabilities are free-form strings so that an unknown one from a newer node
does not make a descriptor unparseable. Version 1 defines `video-store`,
`dht-server`, `gossip-relay` and `bootstrap`.

### 7.4 Envelope

The frame for direct peer-to-peer messages.

| Field | Type | Signed order |
| --- | --- | --- |
| `protocolVersion` | uint | 1 |
| `messageType` | text | 2 |
| `sender` | bytes | 3 (32-byte public key) |
| `timestamp` | uint | 4 |
| `payload` | bytes | 5 (an encoded message) |
| `signature` | bytes | — |

Domain tag `ovn/envelope/v1`. Verifying an envelope says who sent it and that
it was not altered. It says nothing about the payload, which MUST be validated
separately after decoding.

Message types defined in version 1: `PEER_HELLO`, `VIDEO_ANNOUNCE`,
`VIDEO_QUERY`, `VIDEO_PROVIDER`, `PROFILE_UPDATE`, `FOLLOW`.

An unrecognised `messageType` MUST NOT be an error. A node that does not
understand a type ignores the message and carries on.

---

## 8. Tag normalisation

Tags drive each node's local recommendation model, so every implementation
must agree on what a tag is.

```
normalise(tag):
    trim leading and trailing whitespace
    for each character:
        whitespace or '_'  -> mark a separator, emit nothing
        otherwise          -> if a separator is pending and output is non-empty,
                              emit '-'; then emit the character lowercased
```

Lowercasing is Unicode simple lowercasing. `"  Gaming "` → `"gaming"`.
`"Indie Game"` → `"indie-game"`. `"rust_lang"` → `"rust-lang"`. A tag that
normalises to the empty string is dropped. Duplicates after normalisation are
dropped, keeping first occurrence order.

A publisher MUST normalise before signing. A receiver indexes the tags as
they were signed.

---

## 9. Transport and libp2p protocols

| Purpose | Name |
| --- | --- |
| Identify protocol | `/ovn/1.0.0` |
| Kademlia DHT | `/ovn/kad/1.0.0` |
| Block transfer | `/ovn/chunk/1.0.0` |
| Announcement topic | `/ovn/video-announce/1` |
| Profile topic | `/ovn/profile-update/1` |

Transports: QUIC (`quic-v1`) and TCP with Noise and Yamux. Default port
`4800` for both, though nothing depends on it.

The Kademlia protocol name is deliberately not the IPFS one. This network has
its own DHT.

### 9.1 GossipSub

Announcements and profile updates propagate over GossipSub with strict
validation (libp2p message signing on, in addition to the application
signature). Message ids MUST be derived from the message **content**, so that
the same announcement arriving by two paths is one message.

Maximum transmit size is 131 072 bytes. Video data MUST NOT be broadcast on a
gossip topic.

### 9.2 Provider records

A node that holds a video advertises it by calling Kademlia `start_providing`
with the video CID's **binary form** as the key. A node looking for content
calls `get_providers` with the same key, and SHOULD also try peers it is
already connected to: on a small or new network the DHT may not have the
record yet.

### 9.3 Block transfer

A request-response protocol over `/ovn/chunk/1.0.0`, CBOR encoded.

Request:

```
{ "cid": <text> }
```

Response, a tagged union on `status`:

```
{ "status": "Found",    "data": <bytes> }
{ "status": "NotFound" }
{ "status": "Refused" }
```

`Refused` means the peer could serve it but chose not to — a rate limit, or
content it has blocked locally.

The requester MUST hash the returned bytes and compare against the requested
CID. Bytes that do not match MUST be discarded and MUST NOT be stored or
served on. A peer that does this repeatedly SHOULD be deprioritised.

The same protocol serves manifests and chunks: a manifest is just a block
whose CID uses the `dag-cbor` codec.

---

## 10. Joining from a URL or a link

### 10.1 Well-known URL

A node reachable over HTTP MAY publish its descriptor as **JSON** at:

```
/.well-known/ovn/node.json
```

Given `https://video.example.jp`, a client resolves the document URL by
appending that path when the URL has no path component. A URL that already
names a document is used as given.

The client MUST:

* accept only `http` and `https`;
* limit redirects (the reference implementation allows 3);
* refuse a body larger than 16 384 bytes, checking the declared length first
  and the actual length regardless;
* verify the descriptor per section 7.3 before dialling anything.

### 10.2 Share link

A descriptor can also travel inside the link itself, which needs no web server
at all:

```
ourvideo://<base64url-nopad( CBOR(NodeDescriptor) )>
```

The same verification applies. A trailing `/` is ignored. A link longer than
32 768 characters MUST be refused before decoding.

### 10.3 What a link is for

A URL or link is an **entrance**, not a dependency. Once a node has one peer
it discovers others through the DHT and MUST keep working when the URL stops
resolving. An implementation MUST persist peers it has met and dial them on
the next start.

---

## 11. Limits

Every value below is part of the protocol, not local policy. A message that
exceeds one is invalid, and every implementation rejects it the same way.

| Limit | Value |
| --- | --- |
| Message (envelope, total) | 1 048 576 bytes |
| Gossip message | 131 072 bytes |
| Node descriptor document | 16 384 bytes |
| Chunk | 1 048 576 bytes |
| Title | 512 bytes |
| Description | 8 192 bytes |
| Tags · tag | 32 · 64 bytes |
| Node name | 128 bytes |
| Addresses · address | 16 · 256 bytes |
| Capabilities · capability | 16 · 32 bytes |
| Content id, text form | 128 characters |
| Display name · bio | 128 · 2 048 bytes |
| Query · results | 256 bytes · 64 |
| Providers per response | 32 |
| Video duration | 604 800 seconds |
| Clock skew into the future | 300 seconds |
| Chunks per manifest | 65 536 |

---

## 12. Versioning

Every message carries a protocol version. A version 1 node accepts version 1.

* **Unknown optional fields MUST be ignored.** This is how a field can be
  added without a version bump.
* **Unknown enumerated values MUST NOT be fatal.** Unknown message types and
  unknown capabilities are ignored, not rejected.
* A **breaking** change — a field removed, a field's meaning changed, the
  signing order changed — requires a new protocol version.

A node MAY speak several versions. When it does, it MUST respond in the
version the peer used.

---

## 13. Security requirements

A conformant node MUST:

1. treat everything received from the network as untrusted input;
2. verify every signature before storing or acting on a message;
3. verify content hashes before storing or serving content;
4. enforce every limit in section 11 before allocating;
5. limit the rate of inbound gossip and block requests per peer;
6. limit concurrent connections, in total and per peer;
7. reject a timestamp more than 300 seconds in the future;
8. reject a protocol version it does not speak;
9. survive malformed CBOR without panicking or exhausting memory;
10. use a reviewed cryptography library rather than its own.

A conformant node MUST NOT transmit, in any message, in any field:

* watch history, watch duration or watch ratio;
* skip history;
* preference vectors;
* recommendation scores or recommendation history.

No message in this specification has a field for any of them, and none may be
added. See [`../docs/PRIVACY.md`](../docs/PRIVACY.md).

---

## 14. Conformance

An implementation is conformant when it:

* produces signatures other implementations verify, and verifies theirs;
* produces the same content identifier as another implementation for the same
  file;
* joins a network from a link produced by another implementation;
* rejects every invalid message listed above;
* transmits none of the data in section 13.

The Rust implementation's tests are a usable conformance suite for the first
two: `crates/protocol/src/` for encoding and signing,
`crates/content/src/` for chunking and identifiers.
