# oxidarr-prowl

A Prowlarr-compatible indexer manager, in Rust. Part of [Oxidarr](https://github.com/oxidarr/oxidarr),
a Rust rewrite of the \*arr stack that aims for drop-in API compatibility with the existing
applications.

`oxidarr-prowl` is a runnable binary: the Cardigann definition engine, the Newznab/Torznab
HTTP layer, a SQLite-backed store, and a Prowlarr-shaped `/api/v1` (application and indexer
CRUD, schema, search, health/status) all ship in this crate. A real, unmodified Sonarr can
add it as an application, sync a Cardigann indexer from it, and search/grab through it.

## Indexer definitions are fetched at runtime, not vendored

This crate does not bundle any third-party Cardigann indexer definitions. They are
third-party, unlicensed-for-redistribution `.yml` files, so `oxidarr-prowl` fetches its own
definition corpus over HTTP the first time it needs one (auto-update is on by default) rather
than shipping a copy in the crate or the binary.

## The `ui` feature

`oxidarr-prowl` can also serve a Dioxus web frontend over its own `/api/v1` surface, behind
the `ui` cargo feature (off by default — the plain `cargo install oxidarr-prowl`/`cargo build`
binary is unaffected). Building the bundle that feature embeds is a hard prerequisite, not
optional: see the parent repository's README, "Web UI" section, for how to build it before
enabling the feature.

## More

See the [repository](https://github.com/oxidarr/oxidarr) for installation instructions,
architecture, and the rest of the workspace.
