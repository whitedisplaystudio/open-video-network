# Open Video Network

A video network with no centre.

The network holds the videos. Your device holds what it knows about you. The
software handles the distributed part so you do not have to.

This repository is the reference implementation: a Rust core, a `libp2p`
peer-to-peer stack, and a command line client called `ourvideo`. The protocol
is specified separately in [`protocol/SPECIFICATION.md`](protocol/SPECIFICATION.md)
so that a node can be written in any language.

> **Status: V1 draft.** The eight acceptance tests from the design document
> pass ([`crates/node/tests/`](crates/node/tests/)), including "the network
> keeps working after everything the developers run is switched off".

日本語のクイックスタートは [README.ja.md](README.ja.md) にあります。

---

## What makes it different

Three rules, in [`PRINCIPLES.md`](PRINCIPLES.md), that everything else follows
from:

1. **No central server.** If every machine the authors run disappeared
   tomorrow, the network keeps going. There is no API, no database and no
   account server, and there is no bootstrap node you are required to use.
2. **Recommendations are computed on your device.** Watch history, watch
   ratios, skips, preference weights and recommendation scores never leave the
   machine. Not "you can turn the upload off" — there is no upload. The types
   that hold viewing data have no serialisation at all, and
   [a test enforces that](crates/node/tests/privacy.rs).
3. **The complexity is the software's problem.** You never type a peer id, a
   multiaddr, a CID or a public key. Starting a node is one command. Joining a
   network is one link.

---

## Quickstart

### Requirements

* Rust 1.85 or newer (`rustup` recommended)
* Nothing else. FFmpeg is optional and only used to read a video's duration.

### Build and start

```bash
cargo build --release
./target/release/ourvideo start
```

That is the whole of a first run. On start the node will, without asking you
anything:

1. generate an Ed25519 identity and store it with `0600` permissions,
2. create its data directory,
3. create and migrate its SQLite database,
4. start listening on QUIC and TCP,
5. look for neighbours on the local network (mDNS),
6. join the DHT and the gossip topics,
7. start a local HTTP API on `127.0.0.1` for the CLI and any future GUI.

It prints a share link. Anyone with that link can reach you.

### Join someone else's network

In another terminal:

```bash
ourvideo peer add ourvideo://…            # a share link
ourvideo peer add https://video.example.jp  # or a URL
```

One string is all you need. The link is an entrance, not a dependency: once
your node has met one peer it finds the rest through the DHT, and the link can
stop working without affecting you.

### Publish and watch

```bash
ourvideo video publish holiday.mp4 --title "Holiday" --tag travel --tag family
ourvideo video list
ourvideo video get <CID>          # fetch from the network and save a playable file
ourvideo search holiday           # searched locally; the query never leaves
ourvideo watch <CID> --seconds 120 --completed
ourvideo recommendation list      # your feed, computed here
ourvideo recommendation explain <CID>
```

---

## Command reference

| Command | What it does |
| --- | --- |
| `ourvideo start` | Run a node. Everything else needs one running. |
| `ourvideo status` | Peers, videos, cache, listening addresses. |
| `ourvideo stop` | Stop the running node. |
| `ourvideo share-link` | A link others can use to reach you. |
| `ourvideo peer list` | Known peers and how you met them. |
| `ourvideo peer add <URL-or-link>` | Join through a URL, an `ourvideo://` link, or a multiaddr. |
| `ourvideo peer remove <PEER_ID>` | Forget a peer. |
| `ourvideo video publish <FILE>` | Chunk, address, sign and announce a file. |
| `ourvideo video list [--local]` | Discovered videos, or only yours. |
| `ourvideo video info <CID>` | Everything known about one video. |
| `ourvideo video get <CID> [--out PATH]` | Fetch the data and write a playable file. |
| `ourvideo search <QUERY>` | Full-text search over discovered metadata, locally. |
| `ourvideo recommendation list` | Your feed. |
| `ourvideo recommendation explain <CID>` | The exact terms that produced a score. |
| `ourvideo watch <CID> --seconds N` | Record a viewing event, locally. |
| `ourvideo privacy show` | What this device has recorded about you. |
| `ourvideo privacy preferences` | The tag weights derived from it. |
| `ourvideo privacy clear` | Erase both. |
| `ourvideo block cid <CID>` | Hide one video on this node. |
| `ourvideo block creator <KEY>` | Hide everything from a creator on this node. |
| `ourvideo block list` | What is hidden. |
| `ourvideo profile --name "…"` | Publish a display name for your identity. |
| `ourvideo follow <KEY>` | Follow a creator; their videos rank higher for you. |

Add `--json` to any command for the raw response, and `--data-dir PATH` to
work with a node other than the default one.

### `start` options

| Flag | Default | Notes |
| --- | --- | --- |
| `--port` | `4800` | QUIC (UDP) and TCP for peer traffic. |
| `--api-port` | `4801` | Local API, bound to `127.0.0.1` only. |
| `--name` | host name | Published in your node descriptor. |
| `--no-mdns` | off | Do not look for peers on the local network. |
| `--no-api` | off | No local API. The CLI cannot reach the node without it. |
| `--bootstrap <MULTIADDR>` | none | Optional entry points. Repeatable. |
| `--external-addr <MULTIADDR>` | none | Advertise a reachable address, for a node behind NAT. |
| `--cache-limit-gib` | `10` | Ceiling on fetched content. Published content is pinned and not counted. |

### Ports

`4800` (UDP and TCP) and `4801` (TCP, loopback) are the defaults. They were
picked to stay out of the way of the usual suspects — 80, 443, 3000, 5000,
5432, 6379, 8080. Check before you commit to them:

```bash
lsof -i :4800
lsof -i :4801
```

To run a second node on the same machine, give it its own ports and directory:

```bash
ourvideo --data-dir ~/.ovn-second start --port 4810 --api-port 4811
```

### Environment variables

| Variable | Effect |
| --- | --- |
| `OURVIDEO_DATA_DIR` | Data directory, same as `--data-dir`. |
| `OURVIDEO_NODE_NAME` | Default node name. |
| `OURVIDEO_LOG` | Log filter, e.g. `OURVIDEO_LOG=ovn_network=debug`. |

### Where things live

The data directory defaults to the platform's per-user data location
(`~/Library/Application Support/network.OpenVideoNetwork.ourvideo` on macOS,
`~/.local/share/ourvideo` on Linux). Inside it:

```
identity.key    Ed25519 secret key, 0600. Back this up; it is your identity.
node.db         SQLite: peers, discovered videos, cache accounting, and
                your viewing data, which never leaves this file.
blocks/         Content, one file per block, named by its content id.
downloads/      Where `video get` writes playable files by default.
runtime.json    How the CLI finds the running node. 0600: it holds the API token.
```

---

## Local HTTP API

The CLI is a client of the node, not part of it. Anything else can be too — a
desktop GUI, a web UI, a script. The API binds to loopback and requires the
bearer token from `runtime.json`.

```bash
TOKEN=$(jq -r .apiToken ~/.local/share/ourvideo/runtime.json)
curl -s -H "Authorization: Bearer $TOKEN" http://127.0.0.1:4801/v1/status | jq
```

| Method and path | Purpose |
| --- | --- |
| `GET /v1/status` | Node status. |
| `GET /v1/peers` · `POST /v1/peers` | List peers · add one by URL or link. |
| `DELETE /v1/peers/{peerId}` | Forget a peer. |
| `GET /v1/share-link` | This node's share link. |
| `GET /v1/videos` · `POST /v1/videos` | Discovered videos · publish a file. |
| `GET /v1/videos/local` | Videos published here. |
| `GET /v1/videos/{cid}` | One video. |
| `POST /v1/videos/{cid}/fetch` | Fetch its blocks from the network. |
| `POST /v1/videos/{cid}/export` | Write a playable file. |
| `GET /v1/search?q=…` | Local search. |
| `GET /v1/recommendations` | Your feed. |
| `GET /v1/recommendations/{cid}` | Why that score. |
| `POST /v1/watch` · `GET /v1/watch` · `DELETE /v1/watch` | Record · read · erase viewing data. |
| `GET /v1/preferences` | Your tag weights. |
| `POST /v1/profile` | Publish a display name. |
| `POST`/`DELETE` `/v1/follow/{publicKey}` | Follow or unfollow. |
| `GET`/`POST`/`DELETE` `/v1/blocked/…` | Local moderation. |
| `POST /v1/shutdown` | Stop the node. |
| `GET /.well-known/ovn/node.json` | **Public.** This node's signed descriptor. |

That last one is unauthenticated on purpose: it is what turns a URL into a way
to join. Put a reverse proxy in front of it and `https://your.domain` becomes
something you can hand to a newcomer.

---

## Running a node people can reach

A node behind NAT can still fetch and publish, but nobody can dial it. To be
an entry point:

1. Forward UDP and TCP `4800` to the machine.
2. Start with the address you are reachable on:
   ```bash
   ourvideo start --external-addr /ip4/203.0.113.10/udp/4800/quic-v1
   ```
3. Optionally, serve `/.well-known/ovn/node.json` from your domain by proxying
   it from the local API, so people can join with `ourvideo peer add
   https://your.domain`.

None of this makes your node special. It is a convenience for newcomers, and
the network is required to work without it.

---

## Project layout

```
crates/
  protocol/        Wire types, canonical signing bytes, CIDs, limits
  identity/        Ed25519 keys, signatures, peer ids
  network/         libp2p: Kademlia, GossipSub, mDNS, QUIC/TCP, block transfer
  content/         Chunking, Merkle tree, manifests, the block store
  database/        SQLite: peers, videos, FTS5 search, cache, viewing data
  discovery/       URL and share link → verified peer
  storage/         Cache policy: pinning, limits, eviction
  recommendation/  The on-device model and the explanations for it
  node/            Everything wired together, plus the local HTTP API
apps/
  cli/             `ourvideo`
protocol/
  SPECIFICATION.md The wire protocol, for other implementations
docs/
  ARCHITECTURE.md  How the pieces fit
  PRIVACY.md       What is local-only, and how that is enforced
  SECURITY.md      Threat model and the checks that answer it
  MODERATION.md    What moderation means without a centre
```

`ovn-network` and `ovn-protocol` do not depend on `ovn-database`,
`ovn-storage` or `ovn-recommendation`. The crates that can reach the network
cannot name the types that hold your viewing data. This is
[checked by a test](crates/node/tests/privacy.rs).

---

## Development

```bash
cargo test --workspace      # 234 tests, including the acceptance suite
cargo clippy --workspace --all-targets
cargo fmt --all
```

The acceptance tests in `crates/node/tests/acceptance.rs` start real nodes —
real identities, real SQLite files, real libp2p swarms on ephemeral loopback
ports — and put them through sections 37's Test A to Test H. Nothing outside
the test process is contacted. There is no separate test environment and no
test-only database: the tests exercise the same code path `ourvideo start`
does.

### Troubleshooting

**`cc` fails with "You have not agreed to the Xcode license agreements"**
(macOS). Either accept it once:

```bash
sudo xcodebuild -license accept
```

or point the build at the Command Line Tools instead, which do not need it:

```bash
export DEVELOPER_DIR=/Library/Developer/CommandLineTools
```

**"the local API could not bind to 127.0.0.1:4801"** — a node is already
running. Use `ourvideo status`, or start the second one on other ports.

**`ourvideo status` says no node is running** — `runtime.json` is missing or
stale. Start a node, or check you are pointing at the right `--data-dir`.

---

## Known limits in V1

These are deliberate, and listed in the design document rather than hidden:

* **Videos are not guaranteed to survive.** If the creator goes offline and
  every cache has evicted a video, it is gone. Persistent and NAS nodes are a
  V1.5 candidate.
* **No transcoding and no adaptive streaming.** The original file is what is
  distributed.
* **`video get` downloads before it plays.** There is no streaming player yet.
* **Moderation is local only.** Signed, shareable moderation lists are a V1.5
  candidate.
* **NAT traversal is basic.** No relay or hole punching yet.

---

## Governance and licence

Governance is a BDFL model; see [`GOVERNANCE.md`](GOVERNANCE.md). Contributions
are welcome — start with [`CONTRIBUTING.md`](CONTRIBUTING.md) and
[`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md).

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT licence](LICENSE-MIT) at your option. This dual licence is the Rust
ecosystem's convention and is the project's working default; the design
document lists the licence as undecided, so the BDFL may still change it
before the first release.
