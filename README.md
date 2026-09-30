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

### Install

Download the build for your machine from the
[releases page](../../releases), unpack it, and run `ourvideo`. It is a
single self-contained file — the web interface and every language pack are
inside the binary.

| File | For |
| --- | --- |
| `ourvideo-macos-arm64.tar.gz` | Macs with Apple silicon |
| `ourvideo-macos-x86_64.tar.gz` | Intel Macs |
| `ourvideo-linux-x86_64.tar.gz` | Most Linux machines |
| `ourvideo-windows-x86_64.zip` | Windows |

**The binaries are not code signed, so your system will say the publisher is
unknown.** Signing would mean paying Apple and a certificate authority every
year for permission to distribute software whose entire point is not
depending on anyone in particular, so this project does not. What it does
instead is publish a checksum for every file, and a
[build attestation](https://docs.github.com/actions/security-guides/using-artifact-attestations)
linking it to the commit it was built from.

Check what you downloaded before running it:

```bash
shasum -a 256 -c SHA256SUMS --ignore-missing     # macOS, Linux
```

```powershell
Get-FileHash .\ourvideo-windows-x86_64.zip -Algorithm SHA256    # Windows
```

Then, to get past the warning:

* **macOS** — right-click the binary and choose *Open*, once. Or remove the
  quarantine flag: `xattr -d com.apple.quarantine ourvideo`.
* **Windows** — *More info* → *Run anyway*.
* **Linux** — `chmod +x ourvideo`. No warning.

### Build it yourself instead

Every release is built by a
[public workflow](.github/workflows/release.yml) from the commit its tag
points at, and you can do the same:

* Rust 1.85 or newer (`rustup` recommended)
* Nothing else. FFmpeg is optional: with it, published videos get thumbnails
  and a duration.

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

### Open the interface

```bash
ourvideo ui            # the viewer
ourvideo ui --admin    # the node's own control panel
```

Both are served by the node itself on loopback. There is no separate web
server, no build step and no JavaScript toolchain: `cargo build` is the whole
of it.

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
                                  # (the UI streams instead, without downloading first)
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
| `ourvideo doctor` | Check the installation and say what to do about anything wrong. |
| `ourvideo stop` | Stop the running node. |
| `ourvideo share-link` | A link others can use to reach you. |
| `ourvideo channel link` | A link others can use to subscribe to **you**, not this machine. |
| `ourvideo channel subscribe <link>` | Subscribe to a creator, and fetch what they have already published. |
| `ourvideo channel list` | Channels you subscribe to. |
| `ourvideo channel show <KEY>` | Everything this device knows one creator published. |
| `ourvideo channel refresh` | Go and ask whether your channels have anything new. |
| `ourvideo channel unsubscribe <KEY>` | Stop. Videos already discovered are kept. |
| `ourvideo ui [--admin] [--print]` | Open the web interface in a browser. |
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
| `ourvideo block cid <CID>` | Hide one video here, and discard its data. |
| `ourvideo block creator <KEY>` | Hide everything from a creator here, and discard it. |
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
| `--locale` | detected | Interface language, e.g. `ja` or `pt-BR`. Unset, it is worked out from the browser and this machine. |
| `--ui-auth` | `token` | `none` lets a bookmarked URL work with no sign-in. Only for a machine you do not share — see below. |

### Watching from a phone

The local API answers on `127.0.0.1` only, so nothing else on your network can
reach it. `--lan` opens it to the network you are on, for the case where the
node runs on a desktop and you want to watch on a phone:

```bash
ourvideo start --lan
```

It prints a URL with a token in it. Open that once on the phone; it is
exchanged for a session cookie, and afterwards `http://<address>:4801/ui`
works on its own and can be bookmarked.

Two things this does **not** relax. A token is required — `--lan --ui-auth
none` is refused, because together they would let anyone on the network read
your viewing history and control the node. And the node still answers only to
its own address, never to a hostname, which is what DNS rebinding needs.

Anyone holding that URL has the node. Delete `api.token` and restart to revoke
every session.

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
| `OURVIDEO_LOCALE` | Default interface language, same as `--locale`. |
| `OURVIDEO_UI_AUTH` | `token` or `none`, same as `--ui-auth`. |
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
uploads/        Staging for a browser upload. Emptied as soon as it is chunked.
locales/        Drop a language pack here to add or correct a translation.
runtime.json    How the CLI finds the running node. 0600: it holds the API token.
api.token       The local API's bearer token, 0600. Kept so a browser stays
                signed in across restarts; delete it and restart to revoke.
```

#### Back up `identity.key`

There is no account, no password and no sign-up here, and the reason is that
there is no server holding one. The other side of that is worth stating
plainly:

* **Lose the file and the identity is gone.** Nobody can reissue it. Videos you
  published stay on the network, but you can never add to that name again.
* **Copy the file and somebody is you.** There is no way to revoke it, because
  revoking means telling a central authority, and there isn't one.

It is 32 bytes. Copy it somewhere safe and keep it as private as it is on
disk — anywhere you would keep an SSH key.

Moving to a new computer is the same file: copy `identity.key` across and your
channel, your subscribers' subscriptions and everything you have published
follow you.

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
| `GET`/`HEAD` `/v1/videos/{cid}/stream` | Play it. Honours `Range`; chunks are fetched a few ahead of the player. |
| `GET /v1/videos/{cid}/thumbnail` | Its thumbnail, fetched from a peer if needed. |
| `POST /v1/upload?fileName=…&title=…&tags=…` | Publish a file sent as the request body. |
| `GET /v1/events` | Server-sent events: peers, discoveries, download progress. |
| `GET /v1/locales` · `GET /v1/locales/{code}` | **Public.** Available languages, and one pack. |
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
| `GET /ui` · `GET /admin` | **Public.** The two interfaces. They hold no data. |
| `GET /auth?token=…&next=…` | Exchange the token for a session cookie. |

`/.well-known/ovn/node.json` is unauthenticated on purpose: it is what turns a
URL into a way to join. Put a reverse proxy in front of it and
`https://your.domain` becomes something you can hand to a newcomer.

---

## The web interface

Two pages, both served by the node, both talking to the same local API the
CLI uses. `ourvideo ui` opens one; the token is exchanged once for an
`HttpOnly`, `SameSite=Strict` cookie, because a page cannot attach an
`Authorization` header to a `<video src>` or an `EventSource`.

**You only do that once per browser.** After it, bookmark
`http://127.0.0.1:4801/ui` and it keeps working — the token is kept in the
data directory, so restarting the node does not sign you out. To revoke every
session, delete `api.token` and restart.

### Skipping the sign-in step

On a machine you do not share, you can drop the token entirely and have the
bookmarked URL work with nothing to set up:

```bash
ourvideo start --ui-auth none
```

What that trades: loopback binding and the `Host` check still keep the
network and other websites out, but **any other account on this machine could
then control the node and read your viewing history**. That is the only thing
the token was protecting, and it is why `token` is the default — viewing
history is exactly the data this project promises to keep to itself.

**The viewer** (`/ui`) is for watching: a feed ranked on this device, browse
and local search, and a player that **streams** — chunks are fetched from
peers a few ahead of the player, so playback starts on the first chunk rather
than the last and does not stall waiting for the next one. Seeking works,
because the node answers `Range` requests. Every recommendation can be expanded into the exact terms that
produced its score.

Both are available in English, 日本語, Español, Português and العربية. The
language is worked out from local signals only — what you picked last, what
the operator set with `--locale`, what your browser asks for, and failing
those, the country your browser's time zone is in. **There is no IP lookup**:
it would mean telling a third party where you are and that you are running
this, and the browser already knows its own time zone. **A language is one JSON file**: drop
it into the node's `locales` directory and it appears in the picker on the
next reload — no rebuild, no restart, nothing to register. Right-to-left
languages need only `"direction": "rtl"`; the whole layout mirrors from that
one field. See [`docs/TRANSLATING.md`](docs/TRANSLATING.md).

**The admin page** (`/admin`) is for running the node: status, peers, joining
by link, publishing (drag a file in — it is streamed to disk and chunked,
never held in memory), storage and cache, local moderation, and a panel
showing everything this device has recorded about you, with a button that
erases it.

Both pages show live progress from the event stream: peers arriving,
announcements landing, and a progress bar per download.

**Both work from the keyboard and with a screen reader.** A skip link comes
first in the tab order, so reaching the content does not mean tabbing through a
dozen header controls; changing page moves focus into it. Everything operable
has a visible focus ring. Progress, errors and the connection state are live
regions, so they are announced rather than only drawn in a corner. Every
colour pair in the palette is checked by a test against WCAG AA — 4.5:1 for
text on each surface it appears on, 3:1 for the edge of anything you can
operate — in both the light and dark themes. Animation is dropped entirely for
anyone whose system asks for less motion.

The pages are served under a strict `Content-Security-Policy` that allows
nothing from anywhere but this node, and the node refuses any request whose
`Host` is not a loopback name, which closes DNS rebinding.

Thumbnails are generated with FFmpeg when it is installed, stored as ordinary
content blocks, and fetched from peers like anything else. Without FFmpeg,
videos simply have no thumbnail.

---

## Channels

A share link points at a machine. A **channel link** points at a person.

```bash
# Give this to anyone who wants to follow what you publish.
ourvideo channel link

# On their side, once:
ourvideo channel subscribe 'ourvideo://c/…'
```

Subscribing does two things that following a machine cannot. It **finds what
was already published** — including videos announced before you had ever heard
of that creator, which gossip can never deliver to you. And it lets you
**check**: `ourvideo channel refresh` goes and asks, rather than waiting and
hoping you were connected at the right moment.

It keeps working when the creator is offline. Every announcement is signed by
the creator's own key, so any node that kept one can pass it on without being
trusted: it cannot alter an announcement, cannot invent one, and cannot pass
off somebody else's video as theirs. A subscriber therefore asks *whoever is
around* — the addresses in the link, whoever the DHT says can answer, and
peers it is connected to anyway — and verifies every answer itself.

A channel is a public key, not an address. The creator can move to another
computer, change network, or replace their node entirely, and the subscription
still points at them.

**Who you subscribe to never leaves your device.** There is no message for
announcing a subscription, no subscriber count, and no way for a creator to
learn that you subscribed. That is the same rule as the rest of section 32:
who you choose to watch is part of what you watch.

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
  TRANSLATING.md   Adding a language
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
cargo test --workspace      # 326 tests, including the acceptance suite
cargo clippy --workspace --all-targets
cargo fmt --all
```

Three suites are skipped by default because they are measured in minutes
rather than seconds. They are where the failures live that only appear after
a node has been up for a while:

```bash
# Millions of malformed messages through every decoder that touches the wire.
OVN_FUZZ_ITERATIONS=5000000 cargo test -p ovn-node --test untrusted_input --release

# A network left running: publish, fetch, watch, peers arriving and leaving,
# a cache under constant eviction pressure.
OVN_SOAK_SECONDS=900 cargo test -p ovn-node --test soak --release -- --ignored --nocapture
```

[CI](.github/workflows/ci.yml) runs the same three on GitHub's machines, and
only when asked:

```bash
gh workflow run ci.yml                        # Linux
gh workflow run ci.yml -f platforms=all       # Linux, macOS and Windows
```

Nothing starts on its own, because runner minutes are metered while this
repository is private and the multipliers are unequal — Linux counts 1x,
Windows 2x, macOS 10x. Run it before tagging a release, and after anything
that touches platform-specific code. Making the repository public removes
the metering, and the reason for the restraint.

The acceptance tests in `crates/node/tests/acceptance.rs` start real nodes —
real identities, real SQLite files, real libp2p swarms on ephemeral loopback
ports — and put them through sections 37's Test A to Test H. Nothing outside
the test process is contacted. There is no separate test environment and no
test-only database: the tests exercise the same code path `ourvideo start`
does.

### Troubleshooting

Start here:

```bash
ourvideo doctor
```

It works whether or not a node is running, changes nothing, and exits non-zero
when it found something broken — so it is safe to put in a script. It checks
the data directory and whether it can be written to, that the identity key and
API token are not readable by other accounts on the machine, that the database
is intact and the search index answers, that a sample of stored blocks still
hash to their ids, free disk space, whether FFmpeg is installed, whether the
ports are free, and — if a node is answering — its peers and whether anyone can
reach it. Each finding comes with what to do about it.

```
  ok    database         node.db is intact
  ok    block integrity  128 of 4021 blocks rehashed, all correct
  warn  reachability     behind a router, with no peer relaying yet
                         This node can watch but others cannot reach it to fetch
                         what it publishes. It will keep looking for a relay.
```

`--verify-blocks N` changes how many blocks are rehashed (0 skips the cache
entirely, which matters on a full one). `--port` and `--api-port` say which
ports to test when no node is running.

Specific problems:


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

**The web interface says it is not authorised** — that browser has never been
signed in, or you revoked the token. Run `ourvideo ui` and open the link it
prints; the bookmark works again afterwards.

---

## Known limits in V1

These are deliberate, and listed in the design document rather than hidden:

* **Videos are not guaranteed to survive.** If the creator goes offline and
  every cache has evicted a video, it is gone. Persistent and NAS nodes are a
  V1.5 candidate.
* **No transcoding and no adaptive streaming.** The original file is what is
  distributed.
* **No adaptive bitrate.** The web player streams the original file; there is
  one quality, and a slow connection means buffering rather than a lower
  resolution.
* **Moderation is local only.** Signed, shareable moderation lists are a V1.5
  candidate.
* **Hole punching does not always work.** Some routers refuse it, in which
  case the connection stays on a relay — slower, and dependent on the relay
  staying up.
* **The command line is English only.** The web interface is translated; the
  CLI's own output and help text are not, yet.

---

## Governance and licence

Governance is a BDFL model; see [`GOVERNANCE.md`](GOVERNANCE.md). Contributions
are welcome — start with [`CONTRIBUTING.md`](CONTRIBUTING.md) and
[`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md). Translations are the easiest place
to begin: [`docs/TRANSLATING.md`](docs/TRANSLATING.md).

### Help wanted: the translations

English and 日本語 have been reviewed by people who write them. **Español,
Português and العربية have not** — they were written from the English by
someone who does not speak them. They are complete, and that is not the same
as good.

If you read one of those three: opening
`crates/node/src/ui/locales/<code>.json` and saying which sentence is wrong is
the single most useful thing anyone can do here. No Rust, no build, and one key
is a perfectly good pull request. See
[`docs/TRANSLATING.md`](docs/TRANSLATING.md).

### Licence

The code is under the [GNU Affero General Public License v3 or later](LICENSE).

Free to use, free to modify, no payment and no permission needed. The one
condition: if you distribute a modified version, or run one as a service other
people reach over a network, you have to publish your full source under the
same licence. Nobody can take this, close it, and sell it back.

**The protocol is not under that licence.** The wire format in
[`protocol/SPECIFICATION.md`](protocol/SPECIFICATION.md) is
[CC BY 4.0](protocol/LICENSE), and implementing it needs no permission from
anyone, under any licence, open or not. A protocol only one codebase may speak
is one codebase's protocol, which is the thing Principle 1 exists to prevent.

**The name is separate from the code.** See [`TRADEMARK.md`](TRADEMARK.md).
Fork it, change it, ship it — but call it something else, so that a user handed
a modified build can tell. No licence can forbid a copy; what this stops is a
copy passing itself off as the original.
