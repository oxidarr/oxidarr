# Oxidarr

A Rust implementation of the \*arr stack, aiming for drop-in API compatibility with the
existing applications so the surrounding ecosystem — Bazarr, Jellyseerr, Recyclarr,
mobile clients, dashboards — keeps working unchanged.

> **Status: Prowlarr-compatible control plane, with an optional web UI.**
> `oxidarr-prowl` is a runnable binary: the Cardigann definition engine, the
> Newznab/Torznab HTTP layer, a SQLite-backed store, and a Prowlarr-shaped
> `/api/v1` (application and indexer CRUD, schema, search, health/status)
> all ship. A real, unmodified Sonarr can add it as an application, sync a
> Cardigann indexer from it, and search/grab through it — see
> [End-to-end acceptance](#end-to-end-acceptance-sonarr) below. A Dioxus web
> UI over that same `/api/v1` surface ships behind the `ui` cargo feature
> (off by default) — see [Web UI](#web-ui) below; the default, API-only
> binary is unchanged. Packaging exists in-repo — a `Dockerfile` and a
> [Docker](#docker) compose file — but no image is published anywhere yet and there
> are no versioned releases, so running the compose file today means building that
> image yourself first.

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
  oxidarr-ui/          Dioxus frontend over /api/v1 (behind oxidarr-prowl's `ui` feature)
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
They are third-party and unlicensed, so they are never committed, and no released
artefact — container image, `.crate`, release archive — contains them either.

By default, `oxidarr-prowl` fetches and refreshes them itself at runtime: a fresh
instance serves immediately while the first fetch runs in the background, and every
`definitions_interval` afterwards it fetches again, replacing the installed set only
once the new one is fully downloaded and extracted. Set
`OXIDARR_DEFINITIONS_AUTO_UPDATE=false` to disable this and hand the `definitions/`
directory back to yourself — in that case fetch it by hand with
`scripts/fetch-definitions.sh <dest>` (default dest: `.definitions`), as shown in
step 2 below.

## Quick start

1. Build the binary:

   ```sh
   cargo build -p oxidarr-prowl
   ```

2. **Only if you set `OXIDARR_DEFINITIONS_AUTO_UPDATE=false`** (see the table in step 3):
   fetch the Cardigann definitions yourself. `scripts/fetch-definitions.sh <dest>` writes
   `<dest>/v11/*.yml`, but `oxidarr-prowl` reads a *flat* `{data_dir}/definitions/*.yml`
   directory (see `crates/oxidarr-prowl/src/definitions.rs`) — one directory level up
   from where the fetch script writes. Point `definitions` at the fetched `v11`
   subdirectory directly, e.g. with a symlink:

   ```sh
   ./scripts/fetch-definitions.sh .definitions
   mkdir -p data
   ln -s "$(pwd)/.definitions/v11" data/definitions
   ```

   Leave auto-update at its default (`true`) and skip this step entirely: the server
   fetches definitions itself in the background and owns `{data_dir}/definitions`
   outright, replacing whatever it finds there on its first run — a hand-made symlink
   included, silently, with nothing logged about it. Only do the above by hand when
   auto-update is disabled.

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
   proxy, ...) and its file/env precedence, including the definitions updater's own
   settings:

   | Field | Env var | Default | Notes |
   |---|---|---|---|
   | `definitions_auto_update` | `OXIDARR_DEFINITIONS_AUTO_UPDATE` | `true` | set `false` to manage `definitions/` yourself |
   | `definitions_interval` | `OXIDARR_DEFINITIONS_INTERVAL` | `86400` | seconds; must be greater than zero |
   | `definitions_url` | `OXIDARR_DEFINITIONS_URL` | Prowlarr Indexers master tarball | override for a mirror or pinned snapshot |

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

## Docker

```sh
# No image is published yet. Build it locally first, under the exact tag
# the compose file pins:
docker build -t ghcr.io/oxidarr/oxidarr-prowl:0.1.0 .

docker compose -p oxidarr -f docs/docker-compose.yml up -d
docker compose -p oxidarr -f docs/docker-compose.yml logs | grep 'api key'
```

That compose file (`docs/docker-compose.yml`) is the recommended way to run `oxidarr-prowl`;
the startup banner's instance API key is the only place it is surfaced, and the `grep` above
is the fastest way to it. The image is pinned to a specific tag rather than `latest`, so an
unattended `docker compose pull` cannot hand you a different major version overnight.

Set `OXIDARR_EXTERNAL_URL` to the address other machines reach this container on before you
start it. It is written verbatim into every indexer Oxidarr syncs into Sonarr or Radarr, so
if it is left at its container-internal default, every one of those syncs hands Sonarr and
Radarr a `baseUrl` they cannot reach — the indexer shows up on their side, but every search
and grab against it fails.

The container serves immediately on first start; definitions are fetched in the background
the same way a bare-metal instance fetches them (see [Definitions](#definitions) above), so
an instance that reports zero definitions in its first minute is behaving correctly, not
failing. No image ships with definitions baked in — they are third-party and unlicensed, so
they are fetched on the operator's own machine, on the operator's own machine's time, every
time. For an air-gapped host, or one you want pinned to a definition set you already
vetted, set `OXIDARR_DEFINITIONS_AUTO_UPDATE=false` in the compose file's `environment:`
block and manage `/data/definitions` yourself.

Populating it yourself means something different in a container than on bare metal: the
startup notice's instructions to run `scripts/fetch-definitions.sh` and see "Quick start
step 2" assume a source checkout, and neither the script nor the rest of the repo exists
inside the image, which holds only the two compiled binaries. Instead, put a flat set of
`*.yml` files directly into the named volume's `definitions/` directory yourself — for
example `docker cp` them in before or after first start, or prepare a bind mount ahead of
time — mindful of the ownership note below.

The compose file mounts a named volume, `oxidarr-data`, rather than a host directory,
because the image only seeds `/data`'s ownership for a named volume — Docker copies the
image directory's contents and ownership into it on first use. A bind mount such as
`-v ./data:/data` keeps the host directory's existing ownership instead, so if you swap in
one of those, `chown 10001:10001` that host directory first or the container will fail to
write its database and definitions with `EACCES`.

## Web UI

`oxidarr-ui` is a Dioxus web frontend over the same `/api/v1` surface `oxidarr-prowl`
serves: indexer and application CRUD, a cross-indexer search screen, and a status page.
It talks to the server exclusively through that public API — no privileged access — so
running it doubles as a compatibility check of the API itself. It ships behind the `ui`
cargo feature, off by default: the plain `cargo build -p oxidarr-prowl` binary (default
features) from [Quick start](#quick-start) above is completely unaffected by any of this.

**Building the bundle is a hard prerequisite of the `ui` feature, not an optional step.**
`oxidarr-prowl`'s `ui` feature embeds `crates/oxidarr-ui/dist` into the binary at compile
time (`include_dir!`); nothing in this repository creates that directory or ships a
placeholder for it, so enabling the feature (`cargo build`/`check`/`test --features ui`)
without building it first fails outright, at macro expansion, with a message that gives no
hint what to do about it:

```
error: proc macro panicked
  --> crates/oxidarr-prowl/src/ui.rs:88:29
   |
88 | static DIST: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../oxidarr-ui/dist");
   |                             ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
   |
   = help: message: ".../crates/oxidarr-ui/dist" is not a directory
```

(`crates/oxidarr-prowl/build.rs` catches the same condition earlier, with a one-line panic
that points back to the command below — but the underlying requirement is identical either
way.) So: build the bundle first (needs [dioxus-cli](https://dioxuslabs.com) 0.7.10 and the
`wasm32-unknown-unknown` target):

```sh
cargo install dioxus-cli --locked --version 0.7.10
rustup target add wasm32-unknown-unknown
./scripts/build-ui.sh
```

That script runs `dx build --release` in `crates/oxidarr-ui` and copies the result into
`crates/oxidarr-ui/dist` — dioxus-cli 0.7.10's `dx build` does not write there itself for
a web build (see the script's own comment for exactly where it does write, and why).

> Deno also ships a binary called `dx`, and on a machine where its install directory
> precedes `~/.cargo/bin` on `PATH` it wins the name. The script checks `dx --version` and
> uses the first one that identifies itself as `dioxus`, so this resolves itself; set `DX`
> to an explicit path if yours lives somewhere unusual. Calling `dx build` by hand instead
> will hit the shadowed binary and fail with `Unable to choose binary for build`.
>
> The same ordering affects `rustc`: if a non-rustup Rust (Homebrew's, typically) comes
> first on `PATH`, `dx` will use it and stop with `Missing rust target
> wasm32-unknown-unknown` even though `rustup target add` reported success — it added the
> target to a toolchain that is not the one being run. Put `~/.cargo/bin` first, or run
> `PATH="$HOME/.cargo/bin:$PATH" ./scripts/build-ui.sh`.

Then run the server with the UI compiled in:

```sh
cargo run -p oxidarr-prowl --features ui
```

and open the printed `external url` in a browser. The UI has no login of its own — on
first load it prompts for the instance's API key, the same one the startup banner prints
(see step 3 of [Quick start](#quick-start)) — and keeps it in the browser's own storage
after that, until cleared or until the server answers `401`.

The interface is built around one question: which of your things are working? Every row
and panel carries a coloured left edge reading healthy, failed, or dormant — a failure
outranks being disabled, so an indexer that is both still reads as broken. Everything
around that stays deliberately quiet.

Type is IBM Plex Sans, with IBM Plex Mono reserved for machine data (Torznab URLs, keys,
sizes, swarm counts). Both are bundled as latin-subset `woff2` files in
`crates/oxidarr-ui/assets` rather than fetched from a font CDN, since these servers are
frequently offline; they are licensed under the SIL Open Font License 1.1, included there
as `PLEX-LICENSE.txt`. The stylesheet is a single file, `assets/app.css`, with the design's
reasoning in its header comment.

Screenshots will land here once the look has had some use behind it.

## Building

```sh
cargo build --workspace
```

## Licence

GPL-3.0-only.
