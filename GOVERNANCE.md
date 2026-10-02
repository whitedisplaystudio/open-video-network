# Governance

## Model

The project uses a BDFL model — a Benevolent Dictator For Life — for its
initial phase.

```
Founder / BDFL
      │  final direction
      ▼
Core Maintainers
      │  review, merge
      ▼
Contributors
      │  pull requests
      ▼
Community
      └─ issues, discussions, RFCs
```

## The BDFL's role

The BDFL does **not** monopolise day-to-day implementation. The
responsibilities are:

* maintaining the [Founding Principles](PRINCIPLES.md);
* project direction;
* major architectural decisions;
* protocol philosophy;
* deciding matters an RFC has not resolved.

A technically excellent change that violates a Founding Principle is not
adopted. That is the one thing the BDFL role exists to guarantee.

## Core maintainers

Core maintainers review and merge. They are appointed by the BDFL, usually
from people who have been contributing consistently and who have shown they
understand why the principles are written the way they are.

A maintainer may merge changes in their area without escalation. Anything that
touches the wire protocol, the privacy boundary, or the shape of the network
goes to the BDFL.

## RFCs

Changes to the protocol, or anything affecting compatibility between
implementations, go through an RFC:

1. Open a discussion describing the problem, not the solution.
2. Write the RFC: motivation, design, compatibility impact, what it costs, and
   which alternatives were rejected and why.
3. It must say explicitly how it stands with respect to each Founding
   Principle.
4. Core maintainers review. The BDFL decides if consensus does not form.

An RFC that would break an existing implementation must include a migration
path and a protocol version bump.

## Decisions that are not up for negotiation

Within V1:

* the network working without any operator-run infrastructure;
* viewing behaviour staying on the device;
* the protocol being open and implementable by anyone.

Everything else — codecs, storage, discovery mechanics, the recommendation
model, the CLI, the eventual GUI — is open to being redesigned by anyone with
a better idea.

## Succession

A BDFL model has an obvious failure mode. Before the project depends on anyone
in particular, the BDFL is expected to name a successor and to document the
handover. If the BDFL becomes unavailable without doing so, the core
maintainers choose a successor among themselves by simple majority.

## Forking

The licence permits forking and nobody needs permission. Given Principle 1,
a fork that implements this protocol is not even a separate network: nodes
from both still talk to each other. That is intended. It is the practical
limit on how much any governance structure here can matter.

Two things come with it. The [Apache License](LICENSE) asks a fork to carry the
credit: section 4 requires the copyright notice, the licence, and the contents
of [`NOTICE`](NOTICE) to travel with any distribution, and requires significant
changes to be stated. And [`TRADEMARK.md`](TRADEMARK.md) asks a modified fork to
take its own name — not to slow anyone down, but so that a user handed a build
can tell whose it is.

What the licence does not do is stop a fork being closed. That was a deliberate
choice: a copyleft licence would have prevented it, at the cost of making the
code unusable by anyone whose employer forbids copyleft and awkward to ship in
an app store. Credit travelling with the code was judged worth more here than
compelling the code to stay open.
