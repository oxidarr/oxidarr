//! `GET /indexer/schema` — one Prowlarr-shaped [`IndexerResource`] per
//! Cardigann definition [`crate::server::AppState::defs`] can list. Mounted
//! at `/api/v1/indexer/schema` by [`crate::api::api_router`], which this
//! module's own [`router`] does not add that prefix itself.
//!
//! # Prowlarr shape
//!
//! Read `src/Prowlarr.Api.V1/ProviderControllerBase.cs`'s `GetTemplates`
//! (`[HttpGet("schema")]`, inherited by `IndexerController`) from
//! `Prowlarr/Prowlarr` at commit `693c7c3b0e8ec6e9dd792c01e5fa1091260b3be1`:
//! it returns one `IndexerResource` per known indexer *implementation* —
//! for Oxidarr, that is one entry per Cardigann definition this store can
//! list, since Cardigann is the only implementation this crate has. See
//! [`crate::api::dto`]'s module docs for exactly which `IndexerResource`
//! fields are modeled and why, and how a `Setting` becomes a `Field`.
//!
//! # A broken definition is skipped, not fatal
//!
//! [`schema`] must never let one unparsable `<id>.yml` take the whole
//! listing down: a [`DefinitionStore::get`] failure for one id (malformed
//! YAML, a file removed between `list_ids` and `get`, ...) drops that id
//! from the response; every other, loadable definition still appears. See
//! `a_broken_definition_file_is_skipped_not_fatal` in
//! `tests/indexer_schema.rs`.
//!
//! # Categories
//!
//! `capabilities.categories` is a flat `[{id, name}]` list: every distinct
//! standard Newznab category id this definition's `caps.categorymappings`
//! resolves to (via [`CategoryMap::from_definition`] and
//! [`CategoryMap::newznab_ids`]), resolved back to its name via
//! [`oxidarr_cardigann::categories::by_id`]. Real Prowlarr's own
//! `IndexerCapabilityResource.Categories` nests subcategories under their
//! parent block; see [`crate::api::dto`]'s `IndexerCategory` docs for why
//! this endpoint deliberately flattens that away instead.

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use oxidarr_cardigann::categories;
use oxidarr_cardigann::catmap::CategoryMap;
use oxidarr_cardigann::model::Definition;

use crate::api::dto::{Field, IndexerCapabilities, IndexerCategory, IndexerResource};
use crate::definitions::DefinitionStore;

/// Builds `GET /indexer/schema`, a relative route meant to be nested under
/// `/api/v1` by [`crate::api::api_router`] — this function does not add
/// that prefix itself.
pub fn router(defs: DefinitionStore) -> Router {
    Router::new()
        .route("/indexer/schema", get(schema))
        .with_state(defs)
}

/// Lists every definition [`DefinitionStore::list_ids`] can see and maps
/// each one that still loads onto an [`IndexerResource`]. See the module
/// docs' "A broken definition is skipped, not fatal" section: a directory
/// listing failure degrades to an empty response rather than a 500, and a
/// single definition failing to load is dropped rather than aborting the
/// whole listing.
async fn schema(State(defs): State<DefinitionStore>) -> Json<Vec<IndexerResource>> {
    let ids = defs.list_ids().await.unwrap_or_default();
    let mut resources = Vec::with_capacity(ids.len());
    for id in ids {
        if let Ok(def) = defs.get(&id).await {
            resources.push(to_resource(&id, &def));
        }
    }
    Json(resources)
}

/// Maps one loaded [`Definition`] (identified by `id`, its file stem) onto
/// Prowlarr's `IndexerResource` shape — see the module docs.
fn to_resource(id: &str, def: &Definition) -> IndexerResource {
    let map = CategoryMap::from_definition(def);
    let categories = map
        .newznab_ids()
        .into_iter()
        .filter_map(|newznab_id| {
            categories::by_id(newznab_id).map(|category| IndexerCategory {
                id: category.id,
                name: category.name.to_string(),
            })
        })
        .collect();
    let fields = def.settings.iter().map(Field::from_setting).collect();
    IndexerResource {
        id: 0,
        name: def.name.clone(),
        implementation: "Cardigann".to_string(),
        definition_name: id.to_string(),
        description: def.description.clone(),
        language: def.language.clone(),
        enable: false,
        priority: 25,
        capabilities: IndexerCapabilities { categories },
        fields,
        sync_error: None,
    }
}
