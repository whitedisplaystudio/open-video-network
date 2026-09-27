# Architecture

## The shape of it

```
┌────────────────────────────────────────────┐
│ Application                                │
│   ourvideo CLI          (a future GUI)     │
└──────────────────┬─────────────────────────┘
                   │ local HTTP API, 127.0.0.1, bearer token
┌──────────────────▼─────────────────────────┐
│ ovn-node                                   │
│   wiring · event handling · HTTP API       │
├────────────────────────────────────────────┤
│ ovn-identity   ovn-content   ovn-database  │
│ ovn-discovery  ovn-storage   ovn-recommendation │
│ ovn-protocol                               │
├──────────────────┬─────────────────────────┤
│ ovn-network      │                         │
│   Kademlia · GossipSub · mDNS · QUIC/TCP   │
└──────────────────┬─────────────────────────┘
                   │
              other peers
```

There is no central API and no central database, at any layer.

## Crates, and why they are separate

| Crate | Responsibility | Depends on |
| --- | --- | --- |
| `ovn-protocol` | Wire types, canonical signing bytes, content ids, limits | `ovn-identity` |
| `ovn-identity` | Ed25519 keys, signatures, peer ids | — |
| `ovn-network` | The libp2p swarm and everything that touches the wire | `ovn-protocol` |
| `ovn-content` | Chunking, Merkle tree, manifests, the block store | `ovn-protocol` |
| `ovn-database` | SQLite: peers, videos, FTS5, cache accounting, viewing data | `ovn-protocol` |
| `ovn-discovery` | URL and share link → verified peer | `ovn-protocol` |
| `ovn-storage` | Cache policy over the block store | `ovn-content`, `ovn-database` |
| `ovn-recommendation` | The on-device model | `ovn-database` |
| `ovn-node` | Wiring, event handling, local API | all of the above |
| `ovn-cli` | `ourvideo` | `ovn-node` |

The split is not decoration. Two edges in that table are load-bearing:

* **`ovn-network` does not depend on `ovn-database`, `ovn-storage` or
  `ovn-recommendation`.** The code that can send bytes to a peer cannot name
  the types that hold viewing data. A test asserts this.
* **`ovn-recommendation` does not depend on `ovn-network`, `ovn-discovery`,
  `libp2p` or `reqwest`.** The code that knows what you like has nothing to
  send it with.

## The swarm lives in one task

`libp2p`'s `Swarm` is not `Sync`, and connection state is easier to reason
about when there is exactly one place it can change. So:

* one tokio task owns the swarm and runs `select!` over swarm events, a
  command channel and a periodic DHT bootstrap;
* everything else holds a cheap `Network` handle that sends commands and
  awaits replies through oneshot channels;
* the swarm publishes `NetworkEvent`s on a bounded channel.

That channel is bounded on purpose, and `emit` is deliberately not `async`:
if the node stops draining events, dropping one is better than blocking the
swarm, which would stall every peer we are connected to.

An inbound block request is the only case that needs to travel out and come
back. The event carries a `BlockResponder`, which holds the libp2p response
channel and a clone of the command sender; answering it sends a
`RespondBlock` command back to the swarm task.

## Publishing a video

```
file
 └─ chunk into 1 MiB pieces        ovn-content
     └─ each piece → raw CID
         └─ chunk list → Merkle root
             └─ manifest (CBOR) → dag-cbor CID  ← this is the video's id
                 └─ sign an announcement        ovn-protocol
                     ├─ store locally, pinned   ovn-storage
                     ├─ Kademlia start_providing
                     └─ GossipSub announce
```

Every block is pinned, so a node never evicts what it published.

## Fetching a video

```
video CID
 └─ providers: Kademlia get_providers, plus peers already connected
     └─ fetch the manifest block, verify its hash, validate its contents
         └─ fetch missing chunks, four at a time
             ├─ verify each against its CID before writing   ← section 20
             └─ a peer that fails this is skipped
                 └─ all chunks held → start_providing, so we serve it too
                     └─ enforce the cache limit
```

Connected peers are tried alongside DHT providers because on a small or new
network the DHT may not have the record yet, and the peer standing next to you
usually does have the file.

## Discovery

```
start
 ├─ peers we met before   ← the one that matters after everything else is gone
 ├─ mDNS                  ← same LAN, no configuration
 ├─ a URL or link the user pasted
 └─ configured bootstrap addresses (optional, never required)
        └─ one peer
             └─ Kademlia
                  └─ more peers
```

Peers learned from Kademlia routing updates are recorded with their
addresses. That is what lets a node reconnect directly to a peer it has never
spoken to, after the node they both joined through has disappeared.

## Storage

Two layers, deliberately:

* **`ovn-content::BlockStore`** owns bytes. One file per block, named by its
  content id, sharded one directory deep by the first byte of the digest.
  Writes go to a temporary name and are renamed, so a crash cannot leave a
  truncated file under a valid id. Blocks are verified on read as well as
  write; a corrupt block is deleted so the node refetches rather than serving
  bad bytes.
* **`ovn-storage::Storage`** owns policy: what is pinned, what the limit is,
  what gets evicted. It reconciles with the disk at startup, so a crash
  between writing a block and recording it does not leave the accounting wrong
  forever.

## The database

One SQLite file, WAL mode. The schema is in
[`crates/database/src/schema.rs`](../crates/database/src/schema.rs) and has a
line in it dividing the network-facing tables from the local-only ones.

Search is FTS5 over title, description and tags, with BM25 weighting that
favours a title match. User input is turned into quoted FTS5 phrase tokens
rather than interpolated, so a query containing FTS operators is searched for
rather than executed.

## Recommendation

Entirely in `ovn-recommendation`, entirely local.

```
watch events (+ the tags of what was watched)
  └─ signal = ratio + like + completion − skip
      └─ decayed by age, 30-day half-life
          └─ spread across the video's tags
              └─ normalised to −1.0 … 1.0
                  = the preference vector
```

Scoring a candidate sums: tag match, a following bonus, freshness (14-day
half-life), a discovery nudge for videos whose tags are entirely unknown, and
a penalty for something already watched. Every term is kept, so a score can be
explained rather than asserted.

The discovery term is not decoration either: without it the feed converges on
whatever was watched first and never recovers.

## The local API

The CLI is a client. The API is the seam a GUI will use. It binds to loopback
and requires a bearer token from `runtime.json`, because it can read viewing
history: loopback keeps the network out, the token keeps other accounts on the
machine out.

One route is unauthenticated: `/.well-known/ovn/node.json`, the node's signed
public descriptor. That is what makes "put a reverse proxy in front of it and
hand out a URL" work.

## Where to start reading

* the wire: `crates/protocol/src/announcement.rs`
* the network: `crates/network/src/event_loop.rs`
* the node: `crates/node/src/node.rs` and `crates/node/src/events.rs`
* the proof: `crates/node/tests/acceptance.rs`
