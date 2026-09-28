# Contributing

Thanks for looking. This project is small enough that a good pull request
makes a real difference.

## Before you start

Read [`PRINCIPLES.md`](PRINCIPLES.md). It is short, and it is the thing most
likely to get a change rejected — not code style.

For anything that touches the wire protocol, open a discussion first. Protocol
changes affect every implementation, including ones nobody here has seen.

## Setting up

```bash
git clone <this repository>
cd openVideoNetwork
cargo test --workspace
```

You need Rust 1.85 or newer. Nothing else. If `cc` fails on macOS with an
Xcode licence message, see the troubleshooting section of the
[README](README.md#troubleshooting).

## What we look for

**Tests.** Every behaviour change comes with a test, and the test runs
locally before the pull request is opened. There is no separate test
environment and no test-only database — tests exercise the same code path the
real thing does, against the same schema. If you need a node, start a real one
on an ephemeral port; `crates/node/tests/support/mod.rs` has the helper.

**Documentation in the same change.** If externally visible behaviour changes,
the docs change with it, in the same commit:

* new or changed command, flag, or endpoint → [`README.md`](README.md)
* new or changed message, field, or limit → [`protocol/SPECIFICATION.md`](protocol/SPECIFICATION.md)
* new or changed port, container, or directory → [`README.md`](README.md)
* new or changed interface text → `crates/node/src/ui/locales/en.json`, and
  the other shipped packs, in the same change
* anything touching what stays on the device → [`docs/PRIVACY.md`](docs/PRIVACY.md)
* a new threat or a new check → [`docs/SECURITY.md`](docs/SECURITY.md)

A change that alters behaviour without updating the docs will be asked to.

**Comments that explain why.** The code says what it does. A comment earns its
place by saying why it is that way — a constraint, a trade-off, an attack it
prevents. Skip comments that restate the line below them.

**Clippy and rustfmt.**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets
```

Both should be clean.

## Things that will be rejected

* Anything that makes the network depend on infrastructure somebody runs.
* Anything that adds a path for viewing data to leave the device — including
  "opt-in" telemetry, crash reporting that includes content, or an analytics
  endpoint.
* Adding `Serialize` to a local-only type. If a local UI genuinely needs those
  numbers, convert to a display type in `crates/node/src/dto.rs`, by hand,
  where a reviewer will see it.
* A new cryptographic primitive implemented here rather than taken from a
  reviewed library.
* Requiring the user to understand a distributed-systems concept in order to
  do something basic.

## Translations

The easiest contribution, and a genuinely useful one. A language is a single
JSON file and needs no Rust: see [`docs/TRANSLATING.md`](docs/TRANSLATING.md).

If you add a string to the interface, add it to `en.json` in the same change.
Leaving the other packs to a translator is fine — they fall back to English
until someone gets to them — but a *shipped* pack must be complete, and the
tests enforce that.

## Security issues

Do not open a public issue for a vulnerability. See
[`docs/SECURITY.md`](docs/SECURITY.md) for how to report one.

## Licence

Contributions are licensed under the same terms as the project: Apache-2.0 or
MIT, at the user's option. By opening a pull request you agree to that.
