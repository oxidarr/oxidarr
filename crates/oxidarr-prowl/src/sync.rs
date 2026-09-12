//! The app-sync engine: pushes this instance's enabled indexers into a
//! configured Sonarr/Radarr application as Torznab indexers.
//!
//! # Ownership: the mapping table, not name-matching
//!
//! Real Prowlarr also name-matches ([`crate::api::applications`]'s own doc
//! comment already documents several of its other divergences from real
//! Prowlarr, and this is one more): this engine never lists a remote
//! application's indexers to look for one it recognizes. Every synced
//! indexer's identity lives entirely in [`oxidarr_db::MappingRepo`]'s
//! `(app_id, indexer_id) -> remote_indexer_id` table, populated the first
//! time this engine creates that indexer remotely. A `GET /api/v3/indexer`
//! call is never made — the [`crate::server`] module docs' own "one lookup
//! per request" philosophy doesn't apply here; instead, ownership is
//! trusted from the mapping row, and a stale mapping (the remote entry was
//! deleted out from under this instance, by a human clicking around in
//! Sonarr's own UI) is detected lazily, from a `404` on the very `PUT` that
//! assumed the mapping was still good — see "Update: `PUT`, with a `404`
//! fallback to `POST`" below.
//!
//! # Sync levels
//!
//! [`oxidarr_db::SyncLevel`] governs what [`sync_application`] does for one
//! application:
//!
//! - [`SyncLevel::Disabled`](oxidarr_db::SyncLevel::Disabled): the whole
//!   application is skipped — no database read beyond the row already
//!   handed in, and no HTTP call at all.
//! - [`SyncLevel::AddOnly`](oxidarr_db::SyncLevel::AddOnly): every enabled
//!   indexer with no existing mapping is created (`POST`); an enabled
//!   indexer that already has a mapping is left alone (recorded in
//!   [`SyncReport::skipped`], not re-`PUT`); nothing is ever deleted.
//! - [`SyncLevel::FullSync`](oxidarr_db::SyncLevel::FullSync): same create
//!   pass, plus every enabled indexer with an existing mapping is updated
//!   (`PUT`), and every mapping whose local indexer is now disabled (or,
//!   in principle, missing entirely — see the note below) has its remote
//!   entry deleted (`DELETE`) and its mapping row removed.
//!
//! **A mapping can never actually be found pointing at a missing indexer
//! here.** `app_indexer_map.indexer_id` is `ON DELETE CASCADE`, so the
//! instant an indexer row is deleted, every mapping naming it is deleted
//! along with it — by the database, synchronously, not by this engine. By
//! the time any [`sync_application`]/[`sync_all`] call could run afterward,
//! [`MappingRepo::for_app`](oxidarr_db::MappingRepo::for_app) simply no
//! longer has a row to notice as "gone." The `still_enabled` check below
//! still tests for both "missing from `rows`" and "present but disabled"
//! (the two conditions collapse into the same boolean either way, so
//! there's no cost to leaving both), but only the "disabled" half is ever
//! actually reachable through this function. Cleaning up a genuinely
//! *deleted* indexer's remote entry is instead [`crate::api::indexers`]'s
//! own `DELETE /indexer/{id}` handler's job: it snapshots the doomed
//! `(application, remote_id)` pairs via
//! [`MappingRepo::for_indexer`](oxidarr_db::MappingRepo::for_indexer)
//! *before* calling `IndexerRepo::delete` (i.e. before the cascade fires),
//! then calls [`delete_indexer`] on each directly — seeing this module's own
//! delete pass never fire for that case is expected, not a bug.
//!
//! # Payload shape — cited against Sonarr's own source
//!
//! Read `src/Sonarr.Api.V3/Indexers/IndexerResource.cs`,
//! `src/Sonarr.Api.V3/ProviderResource.cs`,
//! `src/NzbDrone.Core/Indexers/Torznab/Torznab.cs`,
//! `src/NzbDrone.Core/Indexers/Torznab/TorznabSettings.cs`,
//! `src/NzbDrone.Core/Indexers/Newznab/NewznabSettings.cs`,
//! `src/Sonarr.Http/ClientSchema/SchemaBuilder.cs`, and
//! `src/Sonarr.Api.V3/ProviderControllerBase.cs` from `Sonarr/Sonarr`
//! (`develop` branch, fetched 2026-09-12). Radarr's own
//! `RadarrSettings`/`Torznab` equivalents mirror Sonarr's byte-for-byte —
//! both are ThingiProvider-based `*arr` forks off the same Servarr
//! lineage — so one payload shape serves both [`oxidarr_db::AppKind`]
//! variants, the same judgment call [`crate::api::applications`]'s own
//! `POST /applications/test` probe already makes for that endpoint.
//!
//! `ProviderResource<T>` contributes `name`, `implementation`,
//! `configContract`, and `fields` (a schema-driven list built by
//! `SchemaBuilder.ToSchema`, whose `GetCamelCaseName` — confirmed by
//! reading `SchemaBuilder.cs` directly — turns each settings property into
//! a `fields[].name` of the same camelCase spelling used everywhere else in
//! this crate's own DTOs, e.g. `baseUrl`/`apiKey`). `IndexerResource` itself
//! adds `enableRss`/`enableAutomaticSearch`/`enableInteractiveSearch` and
//! `priority`. `TorznabSettings` (extending `NewznabSettings`) is where
//! `baseUrl`, `apiPath` (defaulting to `"/api"`), `apiKey`, and `categories`
//! (an `int[]`) live as `fields[]` entries.
//!
//! This engine ships the minimal viable subset of that shape —
//! [`build_indexer_payload`] is the single source of truth for exactly
//! which keys go out on the wire. Deliberately omitted: `protocol` (set
//! server-side, from the resolved `Torznab` provider's own
//! `Protocol => DownloadProtocol.Torrent` override — `ProviderResourceMapper.ToModel`
//! never reads it back off a request body), `supportsRss`/`supportsSearch`
//! (same: provider-computed, never read from the request), and every other
//! `NewznabSettings` field this crate has no value for
//! (`animeCategories`, `additionalParameters`, `multiLanguages`,
//! `failDownloads`, `animeStandardFormatSearch`) — `ReadFromSchema` in
//! `SchemaBuilder.cs` only sets a property when its matching `fields[]`
//! entry is present in the request, and every one of `NewznabSettings`'s own
//! constructor defaults (confirmed in `NewznabSettings.cs`) is a sane
//! empty/off value, so omitting them is honest, not silently wrong.
//!
//! # The `baseUrl`/`apiPath` split — verified, not assumed
//!
//! This instance's own Torznab endpoint is `GET /{indexer_id}/api` (see
//! [`crate::server`]'s module docs) — the path segment already ends in
//! `/api`. Sonarr's `TorznabSettings` (extending `NewznabSettings`) instead
//! keeps `BaseUrl` and `ApiPath` as two separate fields, joined at request
//! time by its own `NewznabRequestGenerator`, with `ApiPath` defaulting to
//! the literal string `"/api"` (`NewznabSettings`'s constructor,
//! `ApiPath = "/api"`, confirmed by reading the file directly — not
//! inferred from the field's `HelpText`/`FieldToken` hint alone). So this
//! engine pushes `baseUrl` = `{external_url}/{indexer_id}` — **without**
//! the trailing `/api` this instance's own route already carries — and a
//! separate `apiPath` = `"/api"` field, letting Sonarr's own join produce
//! exactly `{external_url}/{indexer_id}/api`, matching this instance's real
//! route. Pushing `baseUrl` = `.../api` instead (with the same default
//! `apiPath`) would have Sonarr request `.../api/api`, a 404 against this
//! instance's actual router — this split is load-bearing, not cosmetic.
//!
//! # Display name
//!
//! A synced indexer's remote `name` is `"{name} (Oxidarr)"` — display only.
//! Ownership is never inferred from this string (see "Ownership" above), so
//! nothing breaks if a human renames it in Sonarr's own UI afterward; the
//! next sync still finds it via the mapping row and `PUT`s the (possibly
//! human-renamed) remote entry back to this instance's own name. This is a
//! deliberate, documented tradeoff, not an oversight — Prowlarr does not
//! have this problem because it lets Sonarr be the source of truth for the
//! indexer's *content*, this crate does not carry that concept.
//!
//! # Update: `PUT`, with a `404` fallback to `POST`
//!
//! Reading `ProviderControllerBase.cs`'s `UpdateProvider` confirms the
//! specific status this engine relies on: a `PUT` for an id
//! `_providerFactory.Find` can't resolve (including via the body's own
//! `Id`, its "TODO: remove in next API version" fallback) returns a bare
//! `NotFound()` — a real `404`, not merely undocumented behavior this
//! engine happens to assume. When a `PUT` gets that `404` back, this engine
//! treats the mapping as stale (the remote entry is gone — deleted by a
//! human, or the whole application's indexer list was reset) and falls back
//! to a fresh `POST`, updating the mapping row to the newly assigned remote
//! id. This still counts toward [`SyncReport::updated`], not `created`: to
//! the local indexer this represents, its sync state simply stayed current,
//! the recreation is this engine's own recovery detail. `POST` itself
//! returns `201` (`Created`) and `PUT` `202` (`Accepted`) on real Sonarr —
//! this engine only distinguishes success (`2xx`) from failure, not the
//! exact code, since nothing here needs the difference.
//!
//! # Categories
//!
//! [`categories_for`] resolves a [`IndexerKind::Cardigann`]-kind indexer's
//! Newznab category ids from its Cardigann definition's own
//! `categorymappings`, via [`CategoryMap::from_definition`] — the same
//! resolution [`crate::api::indexer_schema`]'s schema listing and
//! [`crate::torznab::render_caps`]'s `t=caps` response already perform.
//!
//! A [`IndexerKind::Newznab`]/[`IndexerKind::Torznab`]-kind indexer carries
//! no such definition to resolve categories from, and real Sonarr's own
//! `TorznabSettingsValidator` rejects an indexer whose `categories` and
//! `animeCategories` are *both* empty, every time — verified by reading
//! `TorznabSettingsValidator`'s `RuleFor(c => c).Custom(...)` directly, not
//! assumed. Rather than push a payload known to fail Sonarr's own
//! validation, [`sync_application`] skips every `Newznab`/`Torznab`-kind
//! indexer entirely — no `POST`/`PUT` at all — and records it in
//! [`SyncReport::skipped`] with a reason naming the indexer. This is a
//! known, documented limitation: this milestone's acceptance path only
//! exercises `Cardigann`-kind indexers, which always carry a non-trivial
//! `categorymappings` block in practice; a future task could resolve
//! Newznab/Torznab categories from the row's own settings instead of
//! skipping outright.
//!
//! # Failure handling
//!
//! [`sync_application`] aborts on the first failure (database or transport)
//! rather than trying to make partial progress past it — `?` throughout.
//! [`sync_all`] does the opposite one level up: each application's sync
//! runs to completion independently, so one unreachable Sonarr instance
//! never blocks syncing to a sibling application. Both are read by
//! [`crate::api::indexers`]/[`crate::api::applications`]'s CRUD handlers as
//! a single best-effort attempt whose only externally visible outcome is
//! success-or-not — see those modules' own docs for the `syncError`
//! response field this feeds, and why a sync failure still leaves the
//! triggering CRUD write itself at `2xx`.

use oxidarr_cardigann::catmap::CategoryMap;
use oxidarr_core::ids::{AppId, IndexerId, RemoteIndexerId};
use oxidarr_db::{
    ApplicationRepo, ApplicationRow, Db, DbError, IndexerKind, IndexerRepo, IndexerRow,
    MappingRepo, MappingRow, SyncLevel,
};
use oxidarr_indexer::{Body, HttpClient, HttpRequest, HttpResponse, Method};
use serde_json::{Value, json};
use url::Url;

use crate::definitions::{DefinitionError, DefinitionStore};

/// One application's sync outcome: how many indexers were created, updated,
/// or deleted remotely, plus a human-readable reason for each indexer this
/// run deliberately left untouched (e.g. `addOnly` skipping an
/// already-mapped indexer).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// Indexers created remotely (`POST`), including a `PUT`-404 recovery's
    /// fallback `POST` — see the module docs' "Update" section for why that
    /// specific case still counts toward `updated`, not this field.
    pub created: u32,
    /// Indexers updated remotely (`PUT`), including a `PUT`-404 recovery.
    pub updated: u32,
    /// Mappings whose remote entry was deleted (`fullSync` only).
    pub deleted: u32,
    /// One entry per indexer this run left untouched on purpose, naming the
    /// indexer and the reason (currently: `addOnly` leaving an
    /// already-mapped indexer alone).
    pub skipped: Vec<String>,
}

/// Failure syncing one application. Every variant that names an application
/// carries its display `name` (not its id) since this is what
/// [`crate::api`]'s CRUD handlers ultimately fold into a human-readable
/// `syncError` string.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    /// A database operation failed.
    #[error(transparent)]
    Db(#[from] DbError),
    /// The HTTP transport failed outright (DNS, connect, TLS, I/O, …).
    #[error(transparent)]
    Http(#[from] oxidarr_indexer::HttpError),
    /// Loading a `Cardigann`-kind indexer's definition, to resolve its
    /// categories, failed.
    #[error("loading Cardigann definition {id:?} for its categories: {source}")]
    Definition {
        /// The definition id that failed to load.
        id: String,
        /// The underlying load failure.
        #[source]
        source: DefinitionError,
    },
    /// `app`'s own `baseUrl` does not parse as a URL.
    #[error("application {app:?}'s base URL {base_url:?} does not parse: {source}")]
    InvalidBaseUrl {
        /// The application's display name.
        app: String,
        /// The unparseable stored `base_url`.
        base_url: String,
        /// The underlying parse failure.
        #[source]
        source: url::ParseError,
    },
    /// `app` responded to a sync request with a status this engine treats
    /// as a failure (anything outside `2xx`, other than the specific `404`
    /// a `PUT` treats as "stale mapping, fall back to `POST`" — see the
    /// module docs' "Update" section).
    ///
    /// `body` is carried verbatim, not redacted — `Display` (via `#[error]`
    /// below) puts it straight into this error's own message, which
    /// [`crate::api::sync_all_and_describe_failures`]/
    /// [`crate::api::sync_one_and_describe_failure`] then fold into a CRUD
    /// response's `syncError` field. Sonarr/Radarr's own error bodies are
    /// not expected to echo the `X-Api-Key` this request sent (Sonarr never
    /// includes request data in its own validation-failure responses), so
    /// this isn't a credential leak in practice — but it is still an
    /// unredacted third-party response landing in this instance's own API
    /// output verbatim, which a future task should keep in mind if
    /// `syncError` is ever surfaced somewhere with a wider audience (logs
    /// shipped off-box, a UI shown to less-trusted users, ...) than "the
    /// same operator who configured this application's `apiKey` in the
    /// first place."
    #[error("application {app:?} rejected the request with status {status}: {body}")]
    Remote {
        /// The application's display name.
        app: String,
        /// The HTTP status Sonarr/Radarr responded with.
        status: u16,
        /// The response body, decoded lossily as UTF-8 and kept verbatim —
        /// see this variant's own doc comment for why that's not a
        /// redaction concern today.
        body: String,
    },
    /// `app`'s response body did not decode as JSON.
    #[error("application {app:?}'s response did not decode as JSON: {source}")]
    Decode {
        /// The application's display name.
        app: String,
        /// The underlying decode failure.
        #[source]
        source: serde_json::Error,
    },
    /// `app`'s response carried no numeric `"id"` field to record a mapping
    /// against.
    #[error("application {app:?}'s response carried no numeric \"id\" field")]
    MissingRemoteId {
        /// The application's display name.
        app: String,
    },
    /// `app`'s response `"id"` does not fit the `i32` the mapping table's
    /// `remote_indexer_id` column stores.
    #[error("application {app:?}'s response \"id\" does not fit i32: {source}")]
    RemoteIdOverflow {
        /// The application's display name.
        app: String,
        /// The overflow failure.
        #[source]
        source: std::num::TryFromIntError,
    },
}

/// Syncs one application's enabled indexers, per its own
/// [`ApplicationRow::sync_level`] — see the module docs for the full
/// per-level contract.
///
/// `instance_key` is this Oxidarr instance's own API key (from
/// [`oxidarr_db::ConfigRepo::api_key`]), pushed as the synced indexer's
/// `apiKey` field so Sonarr/Radarr can authenticate back against this
/// instance's own Torznab router. `external_url` is this instance's own
/// publicly reachable base URL — see the module docs' "baseUrl/apiPath
/// split" section for exactly how the two combine into the pushed
/// `baseUrl`.
///
/// # Errors
///
/// Returns the first [`SyncError`] encountered — a database failure, a
/// transport failure, an unexpected remote status, or an undecodable
/// response — aborting the rest of this application's sync. See the module
/// docs' "Failure handling" section for why this does not attempt partial
/// progress, and [`sync_all`] for the one-failed-app-does-not-block-others
/// behavior one level up.
pub async fn sync_application<C: HttpClient>(
    client: &C,
    db: &Db,
    defs: &DefinitionStore,
    app: &ApplicationRow,
    external_url: &Url,
    instance_key: &str,
) -> Result<SyncReport, SyncError> {
    if app.sync_level == SyncLevel::Disabled {
        return Ok(SyncReport::default());
    }

    let rows = IndexerRepo::new(db).list().await?;
    let mappings = MappingRepo::new(db).for_app(app.id).await?;
    let mut report = SyncReport::default();

    if app.sync_level == SyncLevel::FullSync {
        for mapping in &mappings {
            let still_enabled = rows
                .iter()
                .any(|row| row.id == mapping.indexer_id && row.enabled);
            if still_enabled {
                continue;
            }
            delete_indexer(client, app, mapping.remote_indexer_id.0).await?;
            MappingRepo::new(db)
                .delete(app.id, mapping.indexer_id)
                .await?;
            report.deleted += 1;
        }
    }

    for row in rows.iter().filter(|row| row.enabled) {
        if matches!(row.kind, IndexerKind::Newznab | IndexerKind::Torznab) {
            report.skipped.push(format!(
                "{}: no categories available for Newznab/Torznab indexers, skipped",
                row.name
            ));
            continue;
        }

        let mapping = mappings
            .iter()
            .find(|mapping| mapping.indexer_id == row.id)
            .copied();

        match mapping {
            None => {
                let categories = categories_for(defs, row).await?;
                let payload =
                    build_indexer_payload(row, external_url, instance_key, &categories, None);
                let remote_id = post_indexer(client, app, payload).await?;
                MappingRepo::new(db)
                    .set(MappingRow {
                        app_id: app.id,
                        indexer_id: row.id,
                        remote_indexer_id: remote_id,
                    })
                    .await?;
                report.created += 1;
            }
            Some(_) if app.sync_level == SyncLevel::AddOnly => {
                report
                    .skipped
                    .push(format!("{}: already synced (addOnly)", row.name));
            }
            Some(mapping) => {
                let categories = categories_for(defs, row).await?;
                let payload = build_indexer_payload(
                    row,
                    external_url,
                    instance_key,
                    &categories,
                    Some(mapping.remote_indexer_id),
                );
                let resp = put_indexer(client, app, mapping.remote_indexer_id.0, payload).await?;
                if resp.status == 404 {
                    // Real Sonarr's Insert throws on a non-zero id, so the
                    // fallback POST needs a fresh payload built with
                    // `remote_id: None` — reusing the PUT's payload (which
                    // still carries the stale remote id) would fail there.
                    let fallback_payload =
                        build_indexer_payload(row, external_url, instance_key, &categories, None);
                    let remote_id = post_indexer(client, app, fallback_payload).await?;
                    MappingRepo::new(db)
                        .set(MappingRow {
                            app_id: app.id,
                            indexer_id: row.id,
                            remote_indexer_id: remote_id,
                        })
                        .await?;
                } else {
                    ensure_success(app, &resp)?;
                }
                report.updated += 1;
            }
        }
    }

    Ok(report)
}

/// Syncs every configured application via [`sync_application`], each
/// independently — one application's failure is captured alongside its id
/// rather than aborting the rest. See the module docs' "Failure handling"
/// section.
///
/// If listing applications itself fails, this returns an empty `Vec`: every
/// caller in this crate ([`crate::api::indexers`]'s CRUD triggers) only
/// reaches this call after its own write to the very same database already
/// succeeded, so a listing failure here would mean the database broke
/// between those two calls — an unreachable-in-practice edge this function
/// still degrades out of safely rather than panicking.
pub async fn sync_all<C: HttpClient>(
    client: &C,
    db: &Db,
    defs: &DefinitionStore,
    external_url: &Url,
    instance_key: &str,
) -> Vec<(AppId, Result<SyncReport, SyncError>)> {
    let apps = ApplicationRepo::new(db).list().await.unwrap_or_default();
    let mut results = Vec::with_capacity(apps.len());
    for app in &apps {
        let result = sync_application(client, db, defs, app, external_url, instance_key).await;
        results.push((app.id, result));
    }
    results
}

/// Resolves `row`'s Newznab category ids — see the module docs'
/// "Categories" section for why only `Cardigann`-kind indexers resolve to
/// anything. In practice, [`sync_application`]'s own per-row loop never
/// calls this for a `Newznab`/`Torznab`-kind row at all (it skips those
/// before ever reaching a mapping/categories decision) — the
/// [`IndexerKind::Newznab`]/[`IndexerKind::Torznab`] arm below stays purely
/// as a defensive, honest total function rather than assuming a caller
/// upholds an invariant this signature doesn't enforce.
async fn categories_for(defs: &DefinitionStore, row: &IndexerRow) -> Result<Vec<u32>, SyncError> {
    match row.kind {
        IndexerKind::Cardigann => {
            let def =
                defs.get(&row.definition_id)
                    .await
                    .map_err(|source| SyncError::Definition {
                        id: row.definition_id.clone(),
                        source,
                    })?;
            Ok(CategoryMap::from_definition(&def).newznab_ids())
        }
        IndexerKind::Newznab | IndexerKind::Torznab => Ok(Vec::new()),
    }
}

/// The display name a synced indexer is pushed under — see the module docs'
/// "Display name" section.
fn display_name(name: &str) -> String {
    format!("{name} (Oxidarr)")
}

/// The `baseUrl` value pushed for `indexer_id` — this instance's own Torznab
/// route, WITHOUT the trailing `/api` `apiPath` supplies separately. See the
/// module docs' "baseUrl/apiPath split" section.
fn torznab_base_url(external_url: &Url, indexer_id: IndexerId) -> String {
    format!(
        "{}/{}",
        external_url.as_str().trim_end_matches('/'),
        indexer_id.0
    )
}

/// Builds the JSON body pushed to Sonarr/Radarr's `POST`/`PUT
/// /api/v3/indexer[/{id}]` — see the module docs' "Payload shape" section
/// for exactly which keys this includes and why every other
/// `IndexerResource`/`TorznabSettings` field is deliberately left out.
/// `remote_id`, when `Some`, is included as the body's own `"id"` (a `PUT`
/// only — real Sonarr's own route already carries the id, but its own UI
/// sends it in the body too, and doing the same costs nothing).
fn build_indexer_payload(
    row: &IndexerRow,
    external_url: &Url,
    instance_key: &str,
    categories: &[u32],
    remote_id: Option<RemoteIndexerId>,
) -> Value {
    let mut obj = serde_json::Map::new();
    if let Some(remote_id) = remote_id {
        obj.insert("id".to_string(), json!(remote_id.0));
    }
    obj.insert("name".to_string(), json!(display_name(&row.name)));
    obj.insert("implementation".to_string(), json!("Torznab"));
    obj.insert("configContract".to_string(), json!("TorznabSettings"));
    obj.insert("enableRss".to_string(), json!(true));
    obj.insert("enableAutomaticSearch".to_string(), json!(true));
    obj.insert("enableInteractiveSearch".to_string(), json!(true));
    obj.insert("priority".to_string(), json!(row.priority));
    obj.insert(
        "fields".to_string(),
        json!([
            {"name": "baseUrl", "value": torznab_base_url(external_url, row.id)},
            {"name": "apiPath", "value": "/api"},
            {"name": "apiKey", "value": instance_key},
            {"name": "categories", "value": categories},
        ]),
    );
    Value::Object(obj)
}

/// Builds the `X-Api-Key` header every Sonarr/Radarr call carries.
fn api_key_header(api_key: &str) -> (String, String) {
    ("X-Api-Key".to_string(), api_key.to_string())
}

/// Builds `app`'s `/api/v3/indexer[/{remote_id}]` URL.
///
/// # Errors
///
/// Returns [`SyncError::InvalidBaseUrl`] if `app.base_url` does not parse.
fn indexer_url(app: &ApplicationRow, remote_id: Option<i32>) -> Result<Url, SyncError> {
    let base = app.base_url.trim_end_matches('/');
    let raw = match remote_id {
        Some(id) => format!("{base}/api/v3/indexer/{id}"),
        None => format!("{base}/api/v3/indexer"),
    };
    raw.parse().map_err(|source| SyncError::InvalidBaseUrl {
        app: app.name.clone(),
        base_url: app.base_url.clone(),
        source,
    })
}

/// Returns `Ok(())` for a `2xx` response, [`SyncError::Remote`] otherwise.
fn ensure_success(app: &ApplicationRow, resp: &HttpResponse) -> Result<(), SyncError> {
    if (200..300).contains(&resp.status) {
        Ok(())
    } else {
        Err(remote_error(app, resp))
    }
}

/// Builds a [`SyncError::Remote`] naming `app` and describing `resp`.
fn remote_error(app: &ApplicationRow, resp: &HttpResponse) -> SyncError {
    SyncError::Remote {
        app: app.name.clone(),
        status: resp.status,
        body: resp.text().into_owned(),
    }
}

/// Decodes a `POST`/`PUT /api/v3/indexer` success response's `"id"` field
/// into a [`RemoteIndexerId`].
///
/// # Errors
///
/// Returns [`SyncError::Decode`] if the body isn't JSON,
/// [`SyncError::MissingRemoteId`] if it has no numeric `"id"`, or
/// [`SyncError::RemoteIdOverflow`] if that id doesn't fit `i32`.
fn parse_remote_id(
    app: &ApplicationRow,
    resp: &HttpResponse,
) -> Result<RemoteIndexerId, SyncError> {
    let value: Value = serde_json::from_slice(&resp.body).map_err(|source| SyncError::Decode {
        app: app.name.clone(),
        source,
    })?;
    let id = value
        .get("id")
        .and_then(Value::as_i64)
        .ok_or_else(|| SyncError::MissingRemoteId {
            app: app.name.clone(),
        })?;
    let id = i32::try_from(id).map_err(|source| SyncError::RemoteIdOverflow {
        app: app.name.clone(),
        source,
    })?;
    Ok(RemoteIndexerId(id))
}

/// `POST`s `payload` to `app`'s `/api/v3/indexer`, returning the newly
/// assigned remote id.
///
/// # Errors
///
/// Returns [`SyncError::Http`] on a transport failure,
/// [`SyncError::InvalidBaseUrl`] if `app.base_url` doesn't parse,
/// [`SyncError::Remote`] on a non-`2xx` response, or one of
/// [`parse_remote_id`]'s own errors decoding the response.
async fn post_indexer<C: HttpClient>(
    client: &C,
    app: &ApplicationRow,
    payload: Value,
) -> Result<RemoteIndexerId, SyncError> {
    let url = indexer_url(app, None)?;
    let resp = client
        .execute(HttpRequest {
            method: Method::Post,
            url,
            headers: vec![api_key_header(&app.api_key)],
            body: Some(Body::Json(payload)),
            follow_redirects: true,
        })
        .await?;
    ensure_success(app, &resp)?;
    parse_remote_id(app, &resp)
}

/// `PUT`s `payload` to `app`'s `/api/v3/indexer/{remote_id}`, returning the
/// raw response so the caller can special-case a `404` (see the module
/// docs' "Update" section) before treating any other non-`2xx` status as a
/// failure.
///
/// # Errors
///
/// Returns [`SyncError::Http`] on a transport failure, or
/// [`SyncError::InvalidBaseUrl`] if `app.base_url` doesn't parse.
async fn put_indexer<C: HttpClient>(
    client: &C,
    app: &ApplicationRow,
    remote_id: i32,
    payload: Value,
) -> Result<HttpResponse, SyncError> {
    let url = indexer_url(app, Some(remote_id))?;
    let resp = client
        .execute(HttpRequest {
            method: Method::Put,
            url,
            headers: vec![api_key_header(&app.api_key)],
            body: Some(Body::Json(payload)),
            follow_redirects: true,
        })
        .await?;
    Ok(resp)
}

/// `DELETE`s `app`'s `/api/v3/indexer/{remote_id}`. A `404` is treated as
/// success (the remote entry is already gone, which is exactly the outcome
/// this call wants) alongside any genuine `2xx`.
///
/// `pub(crate)` (not private): [`crate::api::indexers`]'s `DELETE
/// /indexer/{id}` handler calls this directly, off a mapping snapshot it
/// takes *before* deleting the indexer row — see that handler's own doc
/// comment for why `sync_application`'s own `fullSync` delete pass can
/// never reach a truly-removed indexer's mapping (it's already gone by the
/// time any sync call runs).
///
/// # Errors
///
/// Returns [`SyncError::Http`] on a transport failure,
/// [`SyncError::InvalidBaseUrl`] if `app.base_url` doesn't parse, or
/// [`SyncError::Remote`] on any other non-`2xx` status.
pub(crate) async fn delete_indexer<C: HttpClient>(
    client: &C,
    app: &ApplicationRow,
    remote_id: i32,
) -> Result<(), SyncError> {
    let url = indexer_url(app, Some(remote_id))?;
    let resp = client
        .execute(HttpRequest {
            method: Method::Delete,
            url,
            headers: vec![api_key_header(&app.api_key)],
            body: None,
            follow_redirects: true,
        })
        .await?;
    if (200..300).contains(&resp.status) || resp.status == 404 {
        Ok(())
    } else {
        Err(remote_error(app, &resp))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::path::PathBuf;

    use oxidarr_db::{AppKind, ApplicationRepo, IndexerRepo, NewApplication, NewIndexer};
    use oxidarr_indexer::testing::FakeClient;
    use serde_json::json;

    use super::*;

    fn definitions_dir() -> PathBuf {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/defs"))
    }

    fn defs() -> DefinitionStore {
        DefinitionStore::new(definitions_dir())
    }

    fn external_url() -> Url {
        "http://oxidarr.local:9696".parse().unwrap()
    }

    const INSTANCE_KEY: &str = "instance-key-123";

    async fn seeded_db() -> Db {
        Db::open_in_memory().await.unwrap()
    }

    fn new_application(sync_level: SyncLevel) -> NewApplication {
        NewApplication {
            name: "Sonarr Main".to_string(),
            kind: AppKind::Sonarr,
            base_url: "http://sonarr.example".to_string(),
            api_key: "sonarr-key".to_string(),
            sync_level,
        }
    }

    fn cardigann_indexer(name: &str, enabled: bool) -> NewIndexer {
        NewIndexer {
            name: name.to_string(),
            definition_id: "example".to_string(),
            kind: IndexerKind::Cardigann,
            enabled,
            settings: serde_json::Map::new(),
            priority: 25,
        }
    }

    fn newznab_indexer(name: &str, enabled: bool) -> NewIndexer {
        NewIndexer {
            name: name.to_string(),
            // Unused by `IndexerKind::Newznab` (see the module docs'
            // "Categories" section — this kind carries no Cardigann
            // definition), but `IndexerRepo::insert` still requires a
            // `definition_id`.
            definition_id: "newznab".to_string(),
            kind: IndexerKind::Newznab,
            enabled,
            settings: serde_json::Map::new(),
            priority: 25,
        }
    }

    fn sonarr_indexer_response(id: i64) -> HttpResponse {
        HttpResponse {
            status: 201,
            headers: vec![],
            body: json!({"id": id}).to_string().into_bytes(),
            final_url: "http://sonarr.example/api/v3/indexer".parse().unwrap(),
        }
    }

    fn empty_ok(status: u16) -> HttpResponse {
        HttpResponse {
            status,
            headers: vec![],
            body: b"{}".to_vec(),
            final_url: "http://sonarr.example/api/v3/indexer/1".parse().unwrap(),
        }
    }

    #[test]
    fn torznab_base_url_strips_a_trailing_slash_on_external_url() {
        let with_slash: Url = "http://oxidarr.local:9696/".parse().unwrap();
        let without_slash: Url = "http://oxidarr.local:9696".parse().unwrap();

        assert_eq!(
            torznab_base_url(&with_slash, IndexerId(3)),
            "http://oxidarr.local:9696/3"
        );
        assert_eq!(
            torznab_base_url(&with_slash, IndexerId(3)),
            torznab_base_url(&without_slash, IndexerId(3)),
            "a trailing slash on external_url must not double up before the indexer id"
        );
    }

    #[tokio::test]
    async fn disabled_app_makes_no_requests_and_reports_nothing() {
        let db = seeded_db().await;
        let app = ApplicationRepo::new(&db)
            .insert(&new_application(SyncLevel::Disabled))
            .await
            .unwrap();
        IndexerRepo::new(&db)
            .insert(&cardigann_indexer("Tracker", true))
            .await
            .unwrap();
        let client = FakeClient::new(); // no expectations: any call fails the test

        let report = sync_application(&client, &db, &defs(), &app, &external_url(), INSTANCE_KEY)
            .await
            .unwrap();

        assert_eq!(report, SyncReport::default());
    }

    #[tokio::test]
    async fn first_sync_posts_the_exact_body_and_stores_the_mapping() {
        let db = seeded_db().await;
        let app = ApplicationRepo::new(&db)
            .insert(&new_application(SyncLevel::FullSync))
            .await
            .unwrap();
        let indexer = IndexerRepo::new(&db)
            .insert(&cardigann_indexer("My Tracker", true))
            .await
            .unwrap();
        let expected_base_url = format!("http://oxidarr.local:9696/{}", indexer.id.0);
        let client = FakeClient::new().expect(
            move |req| {
                req.method == Method::Post
                    && req.url.as_str() == "http://sonarr.example/api/v3/indexer"
                    && req
                        .headers
                        .contains(&("X-Api-Key".to_string(), "sonarr-key".to_string()))
                    && matches!(
                        &req.body,
                        Some(Body::Json(value))
                            if *value
                                == json!({
                                    "name": "My Tracker (Oxidarr)",
                                    "implementation": "Torznab",
                                    "configContract": "TorznabSettings",
                                    "enableRss": true,
                                    "enableAutomaticSearch": true,
                                    "enableInteractiveSearch": true,
                                    "priority": 25,
                                    "fields": [
                                        {"name": "baseUrl", "value": expected_base_url},
                                        {"name": "apiPath", "value": "/api"},
                                        {"name": "apiKey", "value": INSTANCE_KEY},
                                        {"name": "categories", "value": [2040, 5030]},
                                    ],
                                })
                    )
            },
            sonarr_indexer_response(7),
        );

        let report = sync_application(&client, &db, &defs(), &app, &external_url(), INSTANCE_KEY)
            .await
            .unwrap();

        assert_eq!(
            report,
            SyncReport {
                created: 1,
                ..SyncReport::default()
            }
        );
        let mapping = MappingRepo::new(&db)
            .get(app.id, indexer.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(mapping.remote_indexer_id, RemoteIndexerId(7));
    }

    #[tokio::test]
    async fn resync_with_an_existing_mapping_puts_not_posts() {
        let db = seeded_db().await;
        let app = ApplicationRepo::new(&db)
            .insert(&new_application(SyncLevel::FullSync))
            .await
            .unwrap();
        let indexer = IndexerRepo::new(&db)
            .insert(&cardigann_indexer("My Tracker", true))
            .await
            .unwrap();
        MappingRepo::new(&db)
            .set(MappingRow {
                app_id: app.id,
                indexer_id: indexer.id,
                remote_indexer_id: RemoteIndexerId(7),
            })
            .await
            .unwrap();
        let client = FakeClient::new().expect(
            |req| {
                req.method == Method::Put
                    && req.url.as_str() == "http://sonarr.example/api/v3/indexer/7"
            },
            empty_ok(202),
        );

        let report = sync_application(&client, &db, &defs(), &app, &external_url(), INSTANCE_KEY)
            .await
            .unwrap();

        assert_eq!(
            report,
            SyncReport {
                updated: 1,
                ..SyncReport::default()
            }
        );
        let mapping = MappingRepo::new(&db)
            .get(app.id, indexer.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            mapping.remote_indexer_id,
            RemoteIndexerId(7),
            "unchanged: PUT never rewrites the mapping"
        );
    }

    #[tokio::test]
    async fn a_404_on_put_falls_back_to_post_and_updates_the_mapping() {
        let db = seeded_db().await;
        let app = ApplicationRepo::new(&db)
            .insert(&new_application(SyncLevel::FullSync))
            .await
            .unwrap();
        let indexer = IndexerRepo::new(&db)
            .insert(&cardigann_indexer("My Tracker", true))
            .await
            .unwrap();
        MappingRepo::new(&db)
            .set(MappingRow {
                app_id: app.id,
                indexer_id: indexer.id,
                remote_indexer_id: RemoteIndexerId(7),
            })
            .await
            .unwrap();
        let client = FakeClient::new()
            .expect(
                |req| {
                    req.method == Method::Put
                        && req.url.as_str() == "http://sonarr.example/api/v3/indexer/7"
                },
                HttpResponse {
                    status: 404,
                    headers: vec![],
                    body: b"{}".to_vec(),
                    final_url: "http://sonarr.example/api/v3/indexer/7".parse().unwrap(),
                },
            )
            .expect(
                |req| {
                    req.method == Method::Post
                        && req.url.as_str() == "http://sonarr.example/api/v3/indexer"
                },
                sonarr_indexer_response(9),
            );

        let report = sync_application(&client, &db, &defs(), &app, &external_url(), INSTANCE_KEY)
            .await
            .unwrap();

        assert_eq!(
            report,
            SyncReport {
                updated: 1,
                ..SyncReport::default()
            }
        );
        let mapping = MappingRepo::new(&db)
            .get(app.id, indexer.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(mapping.remote_indexer_id, RemoteIndexerId(9));

        let requests = client.requests();
        let post_request = requests
            .iter()
            .find(|req| req.method == Method::Post)
            .unwrap();
        let body = post_request
            .body
            .as_ref()
            .and_then(|body| match body {
                Body::Json(value) => Some(value),
                Body::Form(_) => None,
            })
            .unwrap();
        assert!(
            !body.as_object().unwrap().contains_key("id"),
            "fallback POST body must not carry the stale remote id \
             (real Sonarr's Insert throws on a non-zero id): {body:?}"
        );
    }

    // A `full_sync_deletes_the_remote_entry_for_a_removed_local_indexer`
    // test used to live here, built by deleting an indexer (letting its
    // mapping cascade away) and then re-inserting an unrelated *disabled*
    // indexer under a fresh id just to give `MappingRepo::set` a valid
    // foreign key to attach remote id 7 to again. That workaround never
    // actually exercised "mapping present, indexer row absent" — it's
    // identical, code-path-for-code-path, to
    // `full_sync_deletes_the_remote_entry_for_a_now_disabled_indexer` right
    // below, just with an extra indexer inserted and deleted first. There is
    // no way to do better at this layer: `app_indexer_map.indexer_id` is
    // `ON DELETE CASCADE`, so a mapping can never outlive the indexer row it
    // names, and `MappingRepo::set` itself refuses a mapping against a
    // nonexistent indexer id in the first place (see `oxidarr-db`'s own
    // `set_without_existing_parents_returns_err` test) — so "mapping exists,
    // indexer row doesn't" is not a reachable database state at all, not
    // just an untested one. `still_enabled`'s own `rows.iter().any(...)`
    // half (see this module's "Sync levels" doc section) stays in the code
    // as harmless defense in depth; it just has no test of its own here
    // beyond the "disabled" test's identical coverage of the same boolean.
    // The real, reachable scenario — an indexer actually deleted while a
    // `fullSync` application still maps it — is covered end-to-end instead,
    // at the CRUD layer that now handles it correctly: see
    // `oxidarr-prowl/tests/indexers.rs`'s
    // `delete_triggers_a_remote_delete_for_a_fullsync_applications_mapped_indexer`,
    // which exercises `crate::api::indexers::remove`'s snapshot-before-cascade
    // fix directly.

    #[tokio::test]
    async fn full_sync_deletes_the_remote_entry_for_a_now_disabled_indexer() {
        let db = seeded_db().await;
        let app = ApplicationRepo::new(&db)
            .insert(&new_application(SyncLevel::FullSync))
            .await
            .unwrap();
        let indexer = IndexerRepo::new(&db)
            .insert(&cardigann_indexer("Disabled Tracker", false))
            .await
            .unwrap();
        MappingRepo::new(&db)
            .set(MappingRow {
                app_id: app.id,
                indexer_id: indexer.id,
                remote_indexer_id: RemoteIndexerId(3),
            })
            .await
            .unwrap();
        let client = FakeClient::new().expect(
            |req| {
                req.method == Method::Delete
                    && req.url.as_str() == "http://sonarr.example/api/v3/indexer/3"
            },
            empty_ok(200),
        );

        let report = sync_application(&client, &db, &defs(), &app, &external_url(), INSTANCE_KEY)
            .await
            .unwrap();

        assert_eq!(
            report,
            SyncReport {
                deleted: 1,
                ..SyncReport::default()
            }
        );
        let mapping = MappingRepo::new(&db).get(app.id, indexer.id).await.unwrap();
        assert_eq!(mapping, None);
    }

    #[tokio::test]
    async fn add_only_never_puts_or_deletes_an_already_mapped_indexer() {
        let db = seeded_db().await;
        let app = ApplicationRepo::new(&db)
            .insert(&new_application(SyncLevel::AddOnly))
            .await
            .unwrap();
        let mapped = IndexerRepo::new(&db)
            .insert(&cardigann_indexer("Already Mapped", true))
            .await
            .unwrap();
        MappingRepo::new(&db)
            .set(MappingRow {
                app_id: app.id,
                indexer_id: mapped.id,
                remote_indexer_id: RemoteIndexerId(1),
            })
            .await
            .unwrap();
        let disabled_but_mapped = IndexerRepo::new(&db)
            .insert(&cardigann_indexer("Disabled But Mapped", false))
            .await
            .unwrap();
        MappingRepo::new(&db)
            .set(MappingRow {
                app_id: app.id,
                indexer_id: disabled_but_mapped.id,
                remote_indexer_id: RemoteIndexerId(2),
            })
            .await
            .unwrap();
        let unmapped = IndexerRepo::new(&db)
            .insert(&cardigann_indexer("Unmapped", true))
            .await
            .unwrap();
        // No PUT/DELETE expectation at all: if `addOnly` ever issued one,
        // FakeClient would fail the request with its own "unexpected
        // request" error and this test would panic on `.unwrap()` below.
        let client = FakeClient::new().expect(
            move |req| {
                req.method == Method::Post
                    && req.url.as_str() == "http://sonarr.example/api/v3/indexer"
            },
            sonarr_indexer_response(5),
        );

        let report = sync_application(&client, &db, &defs(), &app, &external_url(), INSTANCE_KEY)
            .await
            .unwrap();

        assert_eq!(report.created, 1);
        assert_eq!(report.updated, 0);
        assert_eq!(report.deleted, 0);
        assert_eq!(report.skipped.len(), 1);
        assert!(report.skipped[0].contains("Already Mapped"));
        let new_mapping = MappingRepo::new(&db)
            .get(app.id, unmapped.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(new_mapping.remote_indexer_id, RemoteIndexerId(5));
        // The disabled-but-mapped indexer's own mapping is left completely
        // alone under addOnly — no delete pass runs at all.
        let untouched = MappingRepo::new(&db)
            .get(app.id, disabled_but_mapped.id)
            .await
            .unwrap();
        assert_eq!(
            untouched,
            Some(MappingRow {
                app_id: app.id,
                indexer_id: disabled_but_mapped.id,
                remote_indexer_id: RemoteIndexerId(2),
            })
        );
    }

    #[tokio::test]
    async fn a_newznab_kind_indexer_is_skipped_not_posted_with_empty_categories() {
        let db = seeded_db().await;
        let app = ApplicationRepo::new(&db)
            .insert(&new_application(SyncLevel::FullSync))
            .await
            .unwrap();
        IndexerRepo::new(&db)
            .insert(&newznab_indexer("Usenet Indexer", true))
            .await
            .unwrap();
        // No expectations at all: if this indexer were pushed, the very
        // first POST would hit `FakeClient`'s own "unexpected request"
        // transport error and fail this test via `.unwrap()` below.
        let client = FakeClient::new();

        let report = sync_application(&client, &db, &defs(), &app, &external_url(), INSTANCE_KEY)
            .await
            .unwrap();

        assert_eq!(
            report,
            SyncReport {
                skipped: vec![
                    "Usenet Indexer: no categories available for Newznab/Torznab indexers, \
                     skipped"
                        .to_string()
                ],
                ..SyncReport::default()
            }
        );
    }

    #[tokio::test]
    async fn a_transport_failure_aborts_the_sync() {
        let db = seeded_db().await;
        let app = ApplicationRepo::new(&db)
            .insert(&new_application(SyncLevel::FullSync))
            .await
            .unwrap();
        IndexerRepo::new(&db)
            .insert(&cardigann_indexer("Unreachable", true))
            .await
            .unwrap();
        let client = FakeClient::new(); // no expectations: the first call fails

        let err = sync_application(&client, &db, &defs(), &app, &external_url(), INSTANCE_KEY)
            .await
            .unwrap_err();

        assert!(matches!(err, SyncError::Http(_)));
    }

    #[tokio::test]
    async fn an_unexpected_status_is_a_remote_error() {
        let db = seeded_db().await;
        let app = ApplicationRepo::new(&db)
            .insert(&new_application(SyncLevel::FullSync))
            .await
            .unwrap();
        IndexerRepo::new(&db)
            .insert(&cardigann_indexer("Bad Response", true))
            .await
            .unwrap();
        let client = FakeClient::new().expect(
            |req| req.method == Method::Post,
            HttpResponse {
                status: 500,
                headers: vec![],
                body: b"boom".to_vec(),
                final_url: "http://sonarr.example/api/v3/indexer".parse().unwrap(),
            },
        );

        let err = sync_application(&client, &db, &defs(), &app, &external_url(), INSTANCE_KEY)
            .await
            .unwrap_err();

        assert!(matches!(err, SyncError::Remote { status: 500, .. }));
    }

    #[tokio::test]
    async fn sync_all_runs_every_application_independently() {
        let db = seeded_db().await;
        let mut broken = new_application(SyncLevel::FullSync);
        broken.name = "Broken Sonarr".to_string();
        let broken_app = ApplicationRepo::new(&db).insert(&broken).await.unwrap();
        let mut healthy = new_application(SyncLevel::FullSync);
        healthy.name = "Healthy Sonarr".to_string();
        healthy.base_url = "http://sonarr2.example".to_string();
        let healthy_app = ApplicationRepo::new(&db).insert(&healthy).await.unwrap();
        let indexer = IndexerRepo::new(&db)
            .insert(&cardigann_indexer("Shared Tracker", true))
            .await
            .unwrap();
        // One client, two exact-URL-matched expectations — `sync_all` must
        // reach both (application id order, matching `list()`'s own `ORDER
        // BY id`: `broken_app` first, `healthy_app` second) and must not let
        // the first application's failure stop it from ever calling the
        // second.
        let client = FakeClient::new()
            .expect(
                |req| {
                    req.method == Method::Post
                        && req.url.as_str() == "http://sonarr.example/api/v3/indexer"
                },
                HttpResponse {
                    status: 500,
                    headers: vec![],
                    body: b"boom".to_vec(),
                    final_url: "http://sonarr.example/api/v3/indexer".parse().unwrap(),
                },
            )
            .expect(
                |req| {
                    req.method == Method::Post
                        && req.url.as_str() == "http://sonarr2.example/api/v3/indexer"
                },
                sonarr_indexer_response(2),
            );

        let results = sync_all(&client, &db, &defs(), &external_url(), INSTANCE_KEY).await;

        assert_eq!(results.len(), 2);
        let (first_id, first_result) = &results[0];
        let (second_id, second_result) = &results[1];
        assert_eq!(*first_id, broken_app.id);
        assert!(matches!(
            first_result,
            Err(SyncError::Remote { status: 500, .. })
        ));
        assert_eq!(*second_id, healthy_app.id);
        assert_eq!(
            second_result.as_ref().unwrap(),
            &SyncReport {
                created: 1,
                ..SyncReport::default()
            }
        );
        // The broken app's failed POST never reached the mapping write; the
        // healthy app's succeeded one did.
        assert_eq!(
            MappingRepo::new(&db)
                .get(broken_app.id, indexer.id)
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            MappingRepo::new(&db)
                .get(healthy_app.id, indexer.id)
                .await
                .unwrap()
                .unwrap()
                .remote_indexer_id,
            RemoteIndexerId(2)
        );
    }
}
