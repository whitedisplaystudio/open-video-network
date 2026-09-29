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

## Getting through a router

Almost every home connection is behind a router that accepts nothing inbound.
A node there can dial out but cannot be dialled, which would make it a
consumer only — and would leave the network depending on whoever happens to
have a public address. Four behaviours address that:

```
start
 ├─ upnp        ask the router to forward the port
 ├─ autonat     have other peers try to dial us, so we know rather than guess
 │    └─ reachable?  ── yes ─→ serve content, and relay for others
 │                    └─ no ─→ reserve a slot on a peer that is
 │                              └─ others dial us through it
 │                                  └─ dcutr: both sides dial at once,
 │                                     the relay drops out
 └─ --external-addr skips straight to "yes"
```

Two properties matter:

* **Every reachable node relays.** Not a volunteer subset, not a configured
  list: if you can be dialled, you relay, with modest limits so it cannot be
  used as a proxy. A network where only a few relay is a network with a
  dependency.
* **Relays are found, not published.** Identify tells us which peers speak
  the relay protocol, and libp2p's relay server only advertises it once the
  node knows it is reachable itself — so a node never offers a service it
  cannot provide.

A reservation we stop needing is left to expire rather than closed. Closing
the listener drops libp2p's bookkeeping for that connection, and an
acceptance or renewal already in flight then panics a runtime worker inside
`libp2p-relay`. Renewals run on a timer, so there is no moment that is
reliably safe; a node that becomes reachable simply stops asking for new
slots. Windows CI found this, having been the only platform where the race
actually landed.

A relayed address is deliberately *not* treated as being reachable. The
router still refuses everything; it is somewhere others can find us, not us
becoming dialable. Conflating the two would stop the node looking for further
relays and would have it advertise relay service it cannot provide.

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

## Streaming

`GET /v1/videos/{cid}/stream` answers `Range` requests, which is what makes a
browser willing to scrub through a video rather than download it first.

```
Range: bytes=1048570-2097160
 └─ resolve the manifest (fetching it if we lack it)
     └─ work out which chunks the range touches
         └─ for each, in order:
             ├─ held locally?  read it
             └─ otherwise      fetch it from a provider, verify, store
                 └─ trim the first and last chunk to the requested bytes
```

The body is a stream, so the first chunk goes out while the second is still
being fetched. Providers are resolved once, before the response starts,
rather than per chunk.

The `Content-Type` comes from a manifest a stranger wrote, so it is matched
against an allowlist of media types; anything unrecognised is served as
`application/octet-stream` with `nosniff`. Otherwise a peer could publish
something claiming to be `text/html` and get it executed same-origin with the
UI.

## Thumbnails

Generated with FFmpeg at 10% of the duration — the start of a video is often
black — scaled to 640px and stored as an ordinary `raw` block, pinned like
everything else the node publishes. The id goes in the announcement's
`thumbnailCid`, so a peer fetches it over the same block protocol as any
chunk, and it is checked to actually be a JPEG before being handed to a
browser.

FFmpeg is optional. Without it a video simply has no thumbnail; a publish
never fails over one.

## Live events

A `tokio::sync::broadcast` in the node, exposed as server-sent events on
`GET /v1/events`. Peers arriving and leaving, videos discovered, and per-chunk
download progress.

Nothing on this channel describes viewing behaviour — it is about content and
connections. The event types are enumerated in `crates/node/src/progress.rs`
and a test asserts their fields stay within that boundary.

The stream ends when the node emits `shuttingDown`, because a connection that
never closes would otherwise hold up a graceful shutdown for as long as a
browser tab stays open.

## The local API

The CLI is a client. The API is the seam a GUI will use. It binds to loopback
and requires a bearer token from `runtime.json`, because it can read viewing
history: loopback keeps the network out, the token keeps other accounts on the
machine out.

Three routes are unauthenticated: `/.well-known/ovn/node.json` (the node's
signed public descriptor, which is what makes "put a reverse proxy in front
of it and hand out a URL" work), and the two UI pages with their assets,
which contain no data of their own.

A browser cannot attach an `Authorization` header to a `<video src>`, an
`<img src>` or an `EventSource`, so `/auth?token=…` exchanges the token once
for an `HttpOnly`, `SameSite=Strict` cookie and the middleware accepts
either. The token lives in `api.token` and survives restarts, so that
exchange happens once per browser and the interface can be bookmarked. Every request is also rejected unless its `Host` is a loopback name,
which is what stops a rebound DNS name from making an attacker's page
same-origin with the node.

## The web UI

Two pages — `/ui` to watch, `/admin` to run the node — served straight from
the binary with `include_str!`. No bundler, no npm, no build step: `cargo
build` remains the entire toolchain, which matters for a project whose first
promise is that one command gets you running.

They share `app.css`, a small token-based design system that follows the
system light or dark setting, and `common.js`, which holds the API client,
formatting, a DOM builder that cannot be handed raw HTML, and the event
stream subscription.

A strict `Content-Security-Policy` allows nothing from any other origin, and
no inline script or style — so anything dynamic is a CSS custom property set
from JavaScript rather than a `style` attribute. `crates/node/tests/ui_wiring.rs`
checks that every element the scripts look up exists in the page and that
every API path they call is a route the node serves, which is the part a
compiler would otherwise do.

## Languages

A language is a JSON file with a small header and a flat key-to-text map.
Five are compiled in with `include_str!`; anything in `<data dir>/locales`
is read at request time, so dropping a file in adds a language on the next
page load. An installed pack replaces a built-in one with the same tag,
which is how a shipped translation gets corrected without a release.

English is the canonical key set. Packs are served merged over it, so the
interface always has every key and a partial translation shows English for
the rest rather than a bare identifier. The listing reports coverage per
pack so a translator can see what is left.

Plural categories are the one place a language may exceed English: a pack
may define `_zero`, `_two`, `_few` and `_many` alongside `_one` and
`_other`, and the browser's `Intl.PluralRules` picks between them. Arabic
uses all six.

Dates, relative times, numbers and percentages have no strings at all —
`Intl` formats them from the locale tag. That removes most of what a
translator would otherwise have to get right, and it is why "3 minutes ago"
is correct in every language without anyone writing it.

Which language to show is decided from local signals only, in order: the
stored choice, the operator's `--locale`, `navigator.languages`, the country
of the browser's time zone matched against each pack's `regions`, the
machine's own locale, then English. The time-zone step is the only inference
and sits below the browser's stated preference on purpose — a stated
preference beats a guess. `zones.js` holds the IANA zone-to-country table;
there is no IP lookup, which Principle 1 would rule out anyway and which the
browser makes unnecessary.

The region is kept separate from the pack. A browser reporting `pt-BR` gets
the `pt` pack for words and `pt-BR` for `Intl`, so Brazil and Portugal share
a translation without sharing a date format.

Right-to-left is one field. The stylesheet is written in logical properties
(`inset-inline-end`, `border-inline-start`, `text-align: start`), so setting
`dir="rtl"` mirrors the layout with no second stylesheet. Identifiers and
byte counts are marked `unicode-bidi: plaintext`, because a content id is
not an Arabic word.

Tests hold the two halves together: `i18n.rs` checks that every shipped pack
matches English key for key and placeholder for placeholder, and
`ui_wiring.rs` checks the other direction — that the interface never asks
for a key nobody wrote, and that no English sentence is left hard-coded in
the markup.

## Where to start reading

* the wire: `crates/protocol/src/announcement.rs`
* the network: `crates/network/src/event_loop.rs`
* the node: `crates/node/src/node.rs` and `crates/node/src/events.rs`
* the proof: `crates/node/tests/acceptance.rs`
* adding a language: `docs/TRANSLATING.md`
