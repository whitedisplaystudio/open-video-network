# Translating the interface

A language is one JSON file. You do not need Rust, a build, or a pull request
to try one — drop the file in and reload the page.

Five ship with the node: English, 日本語, Español, Português and العربية.

---

## Adding a language, the quick way

1. Copy the English pack as a starting point:

   ```bash
   curl -s http://127.0.0.1:4801/v1/locales/en > ~/de.json
   ```

2. Edit the header and translate the values. Leave the keys alone.

   ```json
   {
     "locale": "de",
     "name": "Deutsch",
     "englishName": "German",
     "direction": "ltr",
     "formatVersion": 1,
     "strings": {
       "nav.browse": "Durchsuchen",
       "nav.library": "Bibliothek"
     }
   }
   ```

3. Drop it into the node's `locales` directory and reload the page:

   ```bash
   mkdir -p ~/.local/share/ourvideo/locales
   cp ~/de.json ~/.local/share/ourvideo/locales/
   ```

   On macOS that directory is
   `~/Library/Application Support/network.OpenVideoNetwork.ourvideo/locales`.
   `ourvideo status` prints the data directory if you are unsure.

The language appears in the picker in both interfaces. There is no restart,
no rebuild and nothing to register.

You can start with ten strings. Anything you have not translated falls back
to English, and the picker shows how far along each pack is.

A pack in that directory replaces a built-in one with the same tag, so you
can also use this to correct a shipped translation without waiting for a
release.

---

## The file

| Field | Meaning |
| --- | --- |
| `locale` | BCP 47 tag: `de`, `pt-BR`, `zh-Hant-TW`. Letters, digits and hyphens. |
| `name` | The language's name **in that language**. This is what the picker shows, so a speaker can find it without reading a language they do not know. |
| `englishName` | Its name in English, so an operator can read the list. |
| `direction` | `ltr`, or `rtl` for Arabic, Hebrew, Persian and friends. |
| `formatVersion` | `1`. |
| `regions` | ISO 3166 countries where this language is spoken. Optional, but it is what lets the interface pick your language for someone whose browser gave no usable hint. |
| `strings` | Key to translated text. |

---

## How a default is chosen

The interface works out a language from local signals only, in this order:

1. what was chosen in the picker here before;
2. what the operator set on the node (`ourvideo start --locale ja`, or
   `OURVIDEO_LOCALE`);
3. what the browser asks for — `navigator.languages`;
4. the country the browser's **time zone** is in, matched against each pack's
   `regions`;
5. the language the machine running the node is set to;
6. English.

Step 4 is the only inference, and it sits below the browser's own preference
deliberately: someone reading Japanese in Frankfurt should not be handed
German. It exists for the case that actually matters — a browser left on
English, on a device in São Paulo.

**There is no IP lookup, and there will not be one.** It would mean telling a
third party both where the user is and that they are running this, which
Principle 1 rules out and which the browser makes unnecessary: it already
knows its own time zone.

The region also sharpens formatting. Brazil and Portugal share the `pt` pack
but not a date format, so a browser reporting `pt-BR` gets the pack for `pt`
and Brazilian dates, numbers and relative times.

### Refreshing the time zone table

`crates/node/src/ui/zones.js` maps IANA time zones to countries. It is
transcribed from the public-domain `zone.tab` in the system time zone
database (`/usr/share/zoneinfo/zone.tab`, or `/var/db/timezone/zoneinfo`
on macOS), with the legacy zone names some browsers still report added by
hand. Countries rarely move; regenerate it if the database gains a zone your
pack needs. A test checks that every country a shipped pack claims is
actually in the table.

---

## Placeholders

Text in braces is filled in at runtime and must survive translation:

```json
"admin.peers.connected": "Connected to {name}.",
"viewer.toast.saved":    "Saved to {path}"
```

Move them wherever the sentence needs them, but keep every one. A test
compares the placeholders in each translation against the English, so a
dropped `{count}` fails the build rather than silently losing a number.

## Plurals

Keys ending in `_one` and `_other` are plural forms, chosen with the
browser's own CLDR data.

Your language may need forms English does not have, and you may simply add
them — `_zero`, `_two`, `_few`, `_many`:

```json
"conn.peers_one":   "{count} قرين",
"conn.peers_two":   "{count} قرينان",
"conn.peers_few":   "{count} أقران",
"conn.peers_many":  "{count} قرينًا",
"conn.peers_other": "{count} قرين"
```

Japanese, which does not inflect for number, can leave `_one` and `_other`
identical. The Arabic pack is the worked example if you want to see all six.

## What you do *not* have to translate

Dates, relative times ("3 minutes ago"), numbers and percentages are
formatted by the browser from the locale tag, so there are no strings for
them. Durations stay as `h:mm:ss`, which video players show the same way
everywhere.

---

## Right-to-left languages

Set `"direction": "rtl"` and you are done. The stylesheet is written in
logical properties, so the entire layout mirrors from that one field. Content
identifiers and byte counts stay left-to-right inside right-to-left text,
because an identifier is not a word.

---

## Style

* **Translate the meaning, not the words.** If a sentence is awkward in your
  language, write the sentence your language would use.
* **Keep the tone.** The interface explains rather than announces, and says
  plainly what a thing does and does not do. Several strings exist to tell
  someone that nothing was sent anywhere, or that blocking affects only their
  own node; those are promises, and they should read like promises rather
  than marketing.
* **Peer, node, chunk and content id** are the vocabulary of the protocol.
  Use whatever your language's technical writing already uses; do not invent
  a word if a familiar one exists.

---

## Contributing it back

Once a pack is reasonably complete, open a pull request adding it to
`crates/node/src/ui/locales/` and listing it in `BUILT_IN` in
`crates/node/src/i18n.rs`. The tests then apply to it as well:

```bash
cargo test -p ovn-node i18n
```

which checks that it defines every key English does, defines nothing English
does not, and keeps every placeholder. A shipped pack has to be complete;
one in your `locales` directory does not.

See [`../CONTRIBUTING.md`](../CONTRIBUTING.md) for the rest.
