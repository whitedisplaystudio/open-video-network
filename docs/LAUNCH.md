# Publishing this, and finding the people who can fix the translations

A checklist for the day the repository goes public. Nothing here can be done
while it is private, so it sits in a file until then.

The honest framing first: **translators arrive after users do.** Labels,
topics and aggregator listings only pay off once somebody is already looking
at the repository. Steps 1 and 2 are the ones that matter; the rest is
scaffolding so that the people who do arrive have somewhere obvious to put
what they noticed.

---

## 1. On GitHub, at publish time

### Topics

Settings → the gear beside "About". These are the browsing surface.

```
rust  p2p  peer-to-peer  libp2p  decentralized  video  streaming
privacy  self-hosted  i18n  agpl  help-wanted
```

### Labels

Create these once. `help wanted` and `good first issue` are the two GitHub
itself surfaces, and the two that external aggregators index.

| Label | Colour | For |
| --- | --- | --- |
| `translation` | `#0e8a16` | Anything about the language packs |
| `help wanted` | `#008672` | Work that does not need a maintainer to do it |
| `good first issue` | `#7057ff` | Small, self-contained, needs no Rust |
| `needs native speaker` | `#d4c5f9` | Cannot be resolved by anyone else |

```bash
gh label create "translation"          --color 0e8a16 --description "Language packs and wording"
gh label create "needs native speaker" --color d4c5f9 --description "Only a native speaker can settle this"
```

`help wanted` and `good first issue` already exist on a new repository.

### One pinned issue per language

Three issues, labelled `translation`, `help wanted`, `good first issue`,
`needs native speaker`, and pinned (Issues → the pin icon, up to three). These
are the thing a person can link to and claim. Ready to paste — change the
language name and code for each of `es`, `pt`, `ar`:

> **Title:** Spanish (`es`) needs a native speaker's review
>
> The Spanish language pack was written by someone who does not speak Spanish,
> working from the English. It is complete — every key is there and the tests
> pass — but "complete" and "good" are different things, and this is certainly
> stiff in places and wrong in some.
>
> **You do not need to know Rust, and you do not need to build anything.** The
> whole file is `crates/node/src/ui/locales/es.json`.
>
> What is most useful, roughly in order:
>
> 1. Sentences no native speaker would write — usually English word order
>    carried across intact.
> 2. Promises that read as marketing. Several strings tell someone that nothing
>    was sent anywhere, or that hiding a video affects only their own node.
>    Those are facts, and should read like facts.
> 3. The wrong register. The interface talks to one person about their own
>    machine.
> 4. Invented terms where your language already has a settled one.
> 5. Plurals — see [`docs/TRANSLATING.md`](../docs/TRANSLATING.md).
>
> **One key is a useful pull request.** So is a comment that only says which
> sentence is wrong and why, with no suggested replacement. You do not have to
> take the whole file, and you do not have to finish what you start.
>
> Background: [`docs/TRANSLATING.md`](../docs/TRANSLATING.md). Contributions are
> under the AGPL with a DCO sign-off (`git commit -s`) — no CLA, no copyright
> assignment.

### Aggregators

Both index the `help wanted` label and bring people who are specifically
looking for somewhere to start.

* **up-for-grabs.net** — add the project by pull request to their repository.
* **goodfirstissue.dev** — indexes automatically once the labels exist.

---

## 2. Where the people actually are

This is the part that works. Pick the audience, not the channel.

* **Fediverse / Mastodon** — the best fit by a distance. The PeerTube-adjacent
  crowd overlaps almost exactly with what this is, free-software norms are
  strong there, and it has a lot of Spanish and Portuguese speakers. Tag
  `#PeerTube`, `#Fediverse`, `#Rust`, `#P2P`.
* **r/rust** — for the implementation. Expect scrutiny of the libp2p and
  privacy claims, which is worth having.
* **Lobsters, Hacker News (Show HN)** — for the initial look.
* **Language-specific developer communities** once there is something to
  point at.

What to say about the translations, in the announcement itself: that three of
the five packs were written by someone who does not speak those languages, and
that this is known rather than hidden. People correct a stated flaw far more
readily than they volunteer for a chore.

---

## 3. Once somebody has reviewed a language

Two things, in this order.

**Credit them.** A `TRANSLATORS` section in the README, or a line in the
language pack itself. It costs nothing and it is the only payment available.

**Then add `.github/CODEOWNERS`**, so that later changes to their language get
routed to them automatically rather than being merged by someone who cannot
read them. Create the file only when there are real usernames to put in it —
an entry pointing at nobody makes GitHub complain on every pull request.

```
# Each language belongs to whoever reads it.
/crates/node/src/ui/locales/es.json   @their-username
/crates/node/src/ui/locales/pt.json   @their-username
/crates/node/src/ui/locales/ar.json   @their-username

# English is the key set every other pack is measured against, so a change
# here changes every language.
/crates/node/src/ui/locales/en.json   @whitedisplaystudio
```

---

## 4. A translation platform, later

Worth doing once there is more than one language moving at a time, and not
before — it is infrastructure, and infrastructure with nobody using it is just
maintenance.

**Weblate** is the one to look at: its libre hosting is free for projects under
a licence like this one, it has translators who browse for projects to work on,
and it opens pull requests back here on its own.

**Check the file format before committing to it.** The packs are not a plain
flat JSON map — each one wraps its strings in a `strings` object alongside
`locale`, `direction`, `regions` and the rest. Weblate handles several JSON
shapes, but whether it handles this one without a change to the format is an
open question, and finding out is the first task, not an afterthought.

Crowdin and Transifex both have free plans for open source and are the
alternatives if Weblate does not fit.
