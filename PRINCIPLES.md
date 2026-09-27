# Founding Principles

Three rules. Everything else in this project is downstream of them, and a
change that violates one is not adopted however good it is technically.

---

## Principle 1 — No central server

**The network's existence and survival MUST NOT depend on any particular
server, company, person, organisation or domain.**

If the authors shut down every machine they run, the existing peers carry on:
connecting, discovering, announcing, transferring, recommending.

Running an official node or a bootstrap node is allowed. Making one *required*
is not.

What this forbids:

* a required API endpoint, of any kind;
* a central database of accounts, videos, or peers;
* a hard-coded domain the software needs in order to work;
* a licence server, a telemetry endpoint, an update check that gates function.

How it is enforced here: acceptance Test H starts three nodes, has two of them
join through the third, then shuts the third down and requires that
connection, discovery, announcement, transfer and recommendation all keep
working. It is in [`crates/node/tests/acceptance.rs`](crates/node/tests/acceptance.rs).

---

## Principle 2 — Recommendations are local

**Recommendation from personal viewing behaviour happens on the user's own
device.**

The following MUST NOT be sent anywhere for recommendation purposes, or for
any other purpose:

* watch history
* watch duration
* watch ratio
* skip history
* preference vectors
* recommendation scores
* recommendation history

There is no recommendation server. There will not be one.

The bar is higher than a privacy setting. Section 32 of the design document
asks for the absence of a send mechanism, not a switch that turns one off. In
this implementation:

* the types that hold viewing data implement no serialisation at all, so no
  encoder can accept them;
* the crates that can talk to the network do not depend on the crates that
  define those types, so they cannot name them;
* both properties are asserted by tests in
  [`crates/node/tests/privacy.rs`](crates/node/tests/privacy.rs).

See [`docs/PRIVACY.md`](docs/PRIVACY.md) for the detail.

---

## Principle 3 — Complexity belongs to the software

**A user MUST NOT need to understand distributed systems to use this.**

Nobody should have to know what a DHT, a CID, a public key, a peer id, a
multiaddr, GossipSub or NAT traversal is.

The V1 bar:

| Task | What the user does |
| --- | --- |
| Create a node | one command |
| Join an existing network | one URL or link |
| Publish a video | choose a file |
| Watch a video | choose a video |

This is not a promise to hide detail from people who want it — `--json` and
`ourvideo recommendation explain` exist precisely so the machinery can be
inspected. It is a promise that nobody is *required* to look.

---

## Applying these

When a change is proposed, the questions are, in order:

1. Does it make the network depend on something a particular party runs?
2. Does it create any path by which personal viewing behaviour leaves the
   device?
3. Does it push distributed-systems detail onto the user?

A "yes" to any of these means the change is not adopted in that form. This is
not a matter of degree, and a performance argument does not override it. If a
principle itself is wrong, that is a conversation to have explicitly — see
[`GOVERNANCE.md`](GOVERNANCE.md) — not something to erode one pull request at
a time.
