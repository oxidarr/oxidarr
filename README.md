# Oxidarr

A Rust implementation of the \*arr stack, aiming for drop-in API compatibility with the
existing applications so the surrounding ecosystem — Bazarr, Jellyseerr, Recyclarr,
mobile clients, dashboards — keeps working unchanged.

> **Status: Prowlarr-compatible control plane, no UI.** `oxidarr-prowl` is a
> runnable binary: the Cardigann definition engine, the Newznab/Torznab HTTP
> layer, a SQLite-backed store, and a Prowlarr-shaped `/api/v1` (application
> and indexer CRUD, schema, search, health/status) all ship. A real,
> unmodified Sonarr can add it as an application, sync a Cardigann indexer
> from it, and search/grab through it — see
> [End-to-end acceptance](#end-to-end-acceptance-sonarr) below. There is no
> web UI yet (every `/api/v1` interaction is by `curl`/API client) and no
> release-engineering (packaging, Docker image, versioned releases).

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
  oxidarr-migrate/     standalone schema-migration CLI over oxidarr-db
  oxidarr-ui/          Dioxus frontend (not wired up yet)
  oxidarr/             empty umbrella crate (crates.io placeholder)
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
They are third-party and unlicensed, so they are never committed — fetch them with
`scripts/fetch-definitions.sh <dest>` (default dest: `.definitions`).

## Quick start

1. Build the binary:

   ```sh
   cargo build -p oxidarr-prowl
   ```

2. Fetch the Cardigann definitions. `scripts/fetch-definitions.sh <dest>` writes
   `<dest>/v11/*.yml`, but `oxidarr-prowl` reads a *flat* `{data_dir}/definitions/*.yml`
   directory (see `crates/oxidarr-prowl/src/definitions.rs`) — one directory level up
   from where the fetch script writes. Point `definitions` at the fetched `v11`
   subdirectory directly, e.g. with a symlink:

   ```sh
   ./scripts/fetch-definitions.sh .definitions
   mkdir -p data
   ln -s "$(pwd)/.definitions/v11" data/definitions
   ```

3. Run it. The database (`oxidarr.db`, migrated automatically on open — `oxidarr-migrate`
   is only needed if you want to apply/inspect migrations without starting the server)
   and the instance API key both live under `OXIDARR_DATA_DIR`:

   ```sh
   OXIDARR_DATA_DIR=./data OXIDARR_EXTERNAL_URL=http://127.0.0.1:9696 \
     cargo run -p oxidarr-prowl
   ```

   The startup banner is the only place the instance API key is surfaced (there is no
   UI yet):

   ```
   oxidarr-prowl 0.0.0
   listening on 0.0.0.0:9696
   data dir: ./data
   external url: http://127.0.0.1:9696/
   definitions loaded: 548
   api key: <this instance's key>
   ```

   See `crates/oxidarr-prowl/src/config.rs` for every `OXIDARR_*` setting (bind address,
   proxy, ...) and its file/env precedence.

4. Add it to a real Sonarr as an application:

   ```sh
   curl -X POST http://127.0.0.1:9696/api/v1/applications \
     -H "X-Api-Key: <oxidarr's instance key>" -H 'Content-Type: application/json' \
     -d '{
       "name": "Sonarr",
       "implementation": "Sonarr",
       "syncLevel": "fullSync",
       "fields": [
         {"name": "baseUrl", "value": "http://<sonarr host>:8989"},
         {"name": "apiKey", "value": "<sonarr's own api key>"}
       ]
     }'
   ```

5. Add a Cardigann indexer — creating it immediately pushes it into every `fullSync`
   application from step 4:

   ```sh
   curl -X POST http://127.0.0.1:9696/api/v1/indexer \
     -H "X-Api-Key: <oxidarr's instance key>" -H 'Content-Type: application/json' \
     -d '{"name": "EZTV", "implementation": "Cardigann", "definitionName": "eztv", "enable": true}'
   ```

`scripts/e2e-sonarr.sh` automates steps 2–5 end to end against a throwaway Sonarr
container — see the next section.

## End-to-end acceptance (Sonarr)

`scripts/e2e-sonarr.sh` is this milestone's acceptance check: it starts a throwaway
Sonarr container, builds and runs `oxidarr-prowl` against a temp data dir, wires Sonarr
up as a `fullSync` application, pushes one Cardigann indexer, and confirms both sides
agree — Sonarr's own `GET /api/v3/indexer` lists it, and Sonarr's own
`POST /api/v3/indexer/test` passes against our Torznab endpoint. It ends by printing a
manual "grab a release" step, since that needs a monitored series and a download client
already configured, which the script does not set up.

It needs a working `docker` and network access, and is **not** run in CI:

```sh
./scripts/e2e-sonarr.sh                # full run, cleans up after itself
./scripts/e2e-sonarr.sh --keep         # leave everything running to poke at
./scripts/e2e-sonarr.sh --skip-docker  # point at an already-running Sonarr instead
```

Run `./scripts/e2e-sonarr.sh --help` for every environment variable it reads (which
Cardigann definition to add, ports, the Sonarr image, ...).

## Building

```sh
cargo build --workspace
```

## Licence

GPL-3.0-only.
