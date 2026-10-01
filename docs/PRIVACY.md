# Privacy

## The claim

What you watch stays on your machine. Not "we do not sell it", not "you can
turn it off" — there is no mechanism that could send it.

## What is local-only

| Data | Where it lives |
| --- | --- |
| Watch history | `node.db`, table `watch_history` |
| Watch duration and ratio | `node.db`, table `watch_history` |
| Skip history | `node.db`, table `watch_history` |
| Likes | `node.db`, table `watch_history` |
| Preference vector | `node.db`, table `preferences` |
| Recommendation scores | computed on demand, never stored or sent |
| Recommendation history | not recorded at all |
| Search queries | never leave the process |

## How it is enforced

Three layers, all of them checkable.

### 1. The types cannot be serialised

`WatchEvent`, `WatchRecord`, `WatchSummary`, `TagWeight`, `PreferenceModel`,
`Recommendation` and `Contribution` implement neither `Serialize` nor
`Deserialize`.

This is not a style choice. Every encoder in the stack — CBOR for the wire,
JSON for the API — requires `Serialize`. A type that does not implement it
cannot be put in a message, and the compiler enforces that, not a reviewer.

The test that keeps it true:

```rust
assert!(!is_serializable!(WatchRecord));
assert!(!is_serializable!(PreferenceModel));
assert!(!is_serializable!(Recommendation));
```

with a control asserting the probe actually detects `Serialize` when it is
there. See [`crates/node/tests/privacy.rs`](../crates/node/tests/privacy.rs).

### 2. The network crates cannot name those types

`ovn-network` and `ovn-protocol` are everything that touches the wire. Neither
depends on `ovn-database`, `ovn-storage` or `ovn-recommendation`. They could
not reference a `WatchRecord` if someone tried.

Symmetrically, `ovn-recommendation` does not depend on `ovn-network`,
`ovn-discovery`, `libp2p` or `reqwest`. The code that knows what you like has
nothing to send it with.

Both directions are asserted by tests that read the manifests.

### 3. Nothing appears in what a peer stores

An acceptance test runs two real nodes, has one watch, like and skip its way
through several videos, waits, and then checks every field the other node
stores. The check is an **allowlist** of field names, not a search for
suspicious words — so anything new that starts appearing in what a peer holds
has to be added to that list by a person, deliberately.

### 4. Nothing appears on the wire

The three checks above all read the source in one way or another. This one does
not.

A bare `ovn-network` peer joins the network — a real libp2p node speaking this
protocol, with no database, no content layer and no recommendation engine, so
it cannot do any deriving itself. It keeps every byte it is sent. A second node
publishes three videos that differ only in their identity, and a third watches
exactly one of them, twelve times, forms preferences from it, builds a feed and
asks for an explanation.

The recording is then asked what it can tell:

* **No blocks were requested.** The viewer fetched nothing, so it should have
  asked for nothing.
* **The duration it watched for does not appear**, in decimal, in CBOR, or as
  a big- or little-endian integer of either width. A leak cannot hide behind a
  choice of encoding.
* **The watched video is indistinguishable from the two that were not.** The
  viewer forwards gossip, so all three announcements pass through it; what must
  not happen is the one it watched standing out. The test waits until all three
  are on the tape before comparing, so it cannot pass by hearing nothing.
* **Every payload decodes as a message this protocol defines** — an
  announcement or a profile update. A side channel would show up here as bytes
  that do not decode.

Finally the viewer publishes a video of its own, and the recording has to pick
it up. A test that hears nothing proves nothing unless it can hear something.

## The one place conversion happens

A local GUI will want to display your history and your preference weights.
That conversion exists, in exactly one file:
[`crates/node/src/dto.rs`](../crates/node/src/dto.rs). It is written out by
hand, field by field, and the resulting types are served only on loopback,
only with the API token.

Doing it by hand is the point. A derive would make the next one invisible.

## What the creator's server sees

This one is a cost, and it is new.

Video files are served from the creator's own server. Your node fetches them,
so **that server learns your IP address, and that you watched, and roughly
when** — the same as any website you visit. It is in their logs, and this
project cannot do anything about what they keep.

Under the earlier design, where the bytes came from whoever happened to have
them, a creator could not tell who had watched. That is no longer true, and it
is the price of not asking every viewer to store and redistribute other
people's video.

What has not changed: **which** videos you choose to watch, how long you
watched, what you liked, who you subscribe to and what gets recommended to you
all stay on your device. A creator can see that somebody fetched one file. No
part of this system assembles that into a picture of a person, and there is no
message for sending one.

If that matters for a particular video, a VPN or Tor does the same job here as
it does for the rest of the web, because the fetch is an ordinary HTTPS request.

## What the network does see

Running a node is not anonymous, and pretending otherwise would be worse than
useless. Other peers can observe:

* your peer id and public key — they are your identity;
* your IP address, as with any direct connection;
* videos you publish, with your signature on them;
* videos you hold and offer, because you advertise them to the DHT so others
  can fetch them;
* announcements you relay.

What a peer can infer from that: if you fetch a video, you become a provider
for it, and other peers can see that. **Fetching is visible; watching is
not.** A node cannot tell whether you played the file, how much of it, whether
you liked it, or what it changed about your feed.

V1 does not attempt anonymous transport. Onion routing, cover traffic and
metadata resistance are not in scope, and section 4 of the design says so. If
you need to hide the fact that you are on the network at all, this is not yet
the tool for that.

## The live event stream

The web UI needs to know when a peer arrives or a download progresses, so the
node broadcasts events on `GET /v1/events`. That channel carries content and
connection facts only: which video was discovered, how many chunks of a
download are done. There is no event for a play, a pause, a like or a score,
and a test enumerates every variant and checks its fields.

## The web interface

Both pages are served by the node on loopback under a `Content-Security-Policy`
that permits nothing from any other origin — no fonts, no analytics, no CDN.
There is nothing for them to phone home to.

They authenticate with an `HttpOnly`, `SameSite=Strict` cookie, exchanged
once from the API token, because a browser cannot put an `Authorization`
header on a `<video src>` or an `EventSource`. `SameSite=Strict` means
another site cannot make your browser send it, and the node additionally
refuses any request whose `Host` is not a loopback name, which closes DNS
rebinding.

The admin page's privacy panel shows exactly what has been recorded and
erases it in one click. It reads the same `dto.rs` conversion described
above — the only place local-only data is serialised at all.

## The local API

It binds to `127.0.0.1` and nothing else, and every route that can read
viewing data requires a bearer token stored in `runtime.json` with `0600`
permissions. Loopback keeps the network out; the token keeps other user
accounts on the same machine out.

`/.well-known/ovn/node.json` is deliberately open, and carries only the
node's public descriptor — peer id, public key, addresses, capabilities.
A test asserts it contains nothing else.

## Your data is yours

```bash
ourvideo privacy show          # what has been recorded
ourvideo privacy preferences   # the model derived from it
ourvideo privacy clear         # erase both
```

`clear` erases the viewing history and the preference model and leaves your
content alone. There is nothing to erase anywhere else, because there is
nowhere else.

To take everything with you, copy the data directory. To disappear, delete
it — `identity.key` is your identity, and nothing outside that directory
records that you existed.

## Telemetry

There is none. No crash reporting, no usage statistics, no update check, no
phone home. Adding any of them would need a Founding Principle to change
first.

## Reporting a privacy problem

A path by which personal data could leave the device is a security
vulnerability. Report it the way you would any other — see
[`SECURITY.md`](SECURITY.md) — not as a public issue.
