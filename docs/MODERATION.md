# Moderation

## The honest position

There is no central authority here, so there is no global takedown. Nobody —
including the authors — can remove a video from the network. Pretending
otherwise would be dishonest.

What the design does require is that moderation remains *possible*: content
must stay identifiable, so that people and communities can act on it, even
though no single party can act for everyone.

## What makes moderation possible

Every piece of content is identifiable by three things that cannot be forged:

* **Content id** — the SHA2-256 based CID. The same bytes always produce the
  same id, everywhere, so a decision about a piece of content transfers
  between people.
* **Creator public key** — the identity that published it.
* **Signature** — proof the creator published exactly that.

A blocklist is therefore portable in principle: an id means the same thing on
every node.

## What V1 implements

Local blocking. Your node, your decision, nobody else affected.

```bash
ourvideo block cid <CID> --reason "not for me"
ourvideo block creator <PUBLIC_KEY> --reason "spam"
ourvideo block list
ourvideo block cid <CID> --undo
```

Blocking has three effects:

1. The content disappears from your listings, your search results and your
   recommendations.
2. New announcements from a blocked creator are discarded on arrival — they
   are never stored.
3. Your node stops serving blocked content to other peers: a block request
   for it is answered `Refused`.

That third point matters. Blocking is not only about what you see; it is also
about not participating in distributing something.

## What V1 does not do

* **No global takedown.** There is no mechanism, by design.
* **No shared blocklists.** Signed, subscribable moderation lists are a V1.5
  candidate — see below.
* **No automated classification.** No model decides what you may see.
* **No reporting to anyone.** There is nobody to report to.

## Where this is heading

Signed moderation lists are the V1.5 candidate. The shape:

* anyone can publish a list of blocked content ids and creator keys;
* the list is signed by its publisher, like anything else here;
* a user may subscribe to a list, and may unsubscribe at any time;
* subscribing is always a choice, and lists compose.

That keeps the property that matters: moderation is something communities do
for themselves, not something done to them. A list nobody subscribes to has no
effect, and no list is built in.

It is explicitly **not** planned to let a list be imposed, to have an official
list, or to make any list a default.

## For people running a public node

If you run a node others reach, you are storing and serving content other
people published. Consider:

* your local blocks stop your node serving that content, immediately;
* your cache limit bounds how much fetched content you hold at all — set it
  to what you are comfortable with;
* content you publish is pinned and never evicted; content you fetched is
  evicted least-recently-used;
* `ourvideo video list` and `ourvideo status` show what you are holding.

The legal position of running such a node varies by jurisdiction and this
document is not legal advice.

## Rationale

Centralised moderation and a network that cannot be switched off are
incompatible. Choosing Principle 1 means giving up the ability to remove
things globally — and, with it, the ability for anyone else to remove things
globally. That is the trade, stated plainly.

What is left is the decision each person makes about their own node, and
whatever communities choose to build on top of shared, signed, voluntary
lists.
