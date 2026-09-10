# Oxidarr

A Rust implementation of the \*arr stack, aiming for drop-in API compatibility with the
existing applications so the surrounding ecosystem — Bazarr, Jellyseerr, Recyclarr,
mobile clients, dashboards — keeps working unchanged.

> **Status: libraries only.** The Cardigann definition engine (parsing,
> selectors, templates, filters, extraction) and the HTTP layer (client
> abstraction, request building, login flows, Newznab/Torznab support) are
> implemented as libraries. No server, database, or UI yet.

## Why

The \*arr applications (Sonarr, Radarr, Lidarr, Readarr, Prowlarr) are five forks of one
C#/.NET codebase. They share an architecture but not the code, so the same bug gets fixed
five times. Oxidarr keeps the same per-app binaries and the same public APIs, but puts the
shared logic in shared crates.

## Layout

```
crates/
  oxidarr-core/        domain types and traits; no I/O
  oxidarr-db/          SQLx + SQLite persistence
  oxidarr-cardigann/   Cardigann v11 definition engine
  oxidarr-indexer/     Newznab / Torznab / Cardigann indexers
  oxidarr-http/        shared axum scaffolding
  oxidarr-prowl/       Prowlarr-compatible app (binary)
  oxidarr-ui/          Dioxus frontend
```

Dependencies point one way, toward `oxidarr-core`. No application crate is a dependency
of a library crate.

## First milestone

`oxidarr-prowl` is done when an unmodified, real Sonarr can add it as an application,
sync indexers from it, run an interactive search across both Cardigann and Torznab
indexers, and grab a release.

## Definitions

Oxidarr executes [Prowlarr's Cardigann definitions](https://github.com/Prowlarr/Indexers)
unmodified, targeting schema **v11** (the only version upstream currently supports).

## Building

```sh
cargo build --workspace
```

## Licence

GPL-3.0-only.
