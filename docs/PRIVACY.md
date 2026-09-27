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

### 3. Nothing appears on the wire

An acceptance test runs two real nodes, has one watch, like and skip its way
through several videos, waits, and then checks every field the other node
stores. The check is an **allowlist** of field names, not a search for
suspicious words — so anything new that starts appearing in what a peer holds
has to be added to that list by a person, deliberately.

## The one place conversion happens

A local GUI will want to display your history and your preference weights.
That conversion exists, in exactly one file:
[`crates/node/src/dto.rs`](../crates/node/src/dto.rs). It is written out by
hand, field by field, and the resulting types are served only on loopback,
only with the API token.

Doing it by hand is the point. A derive would make the next one invisible.

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
