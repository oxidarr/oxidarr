//! `GET /system/status` and `GET /health` — Prowlarr's own
//! system-identification and health-check endpoints. Both are mounted at
//! `/api/v1` by [`crate::api::api_router`], the caller these routes are
//! relative to.
//!
//! # Prowlarr shapes
//!
//! Read `src/Prowlarr.Api.V1/System/{SystemController,SystemResource}.cs`
//! and `src/Prowlarr.Api.V1/Health/HealthController.cs` from
//! `Prowlarr/Prowlarr` at commit `693c7c3b0e8ec6e9dd792c01e5fa1091260b3be1`.
//!
//! `SystemController.GetStatus` (`[HttpGet("status")]`, mounted at
//! `/api/v1/system` by `[V1ApiController]`'s routing convention — hence
//! `/api/v1/system/status`) returns a `SystemResource` carrying 32 fields.
//! [`SystemStatus`] ships the honest subset a client actually needs to
//! confirm it's talking to an Oxidarr instance and which version:
//!
//! | Field | Value | Rationale |
//! |---|---|---|
//! | `appName` | `"Oxidarr"` | identifies the server, same purpose as Prowlarr's `BuildInfo.AppName` |
//! | `instanceName` | `"Oxidarr"` | no per-instance naming config exists yet; a constant until one does |
//! | `version` | `env!("CARGO_PKG_VERSION")` | the one field a version-gated client actually reads |
//! | `isProduction` | `true` | no dev/prod distinction is tracked; a fixed constant rather than `!cfg!(debug_assertions)`, so it does not flip depending on whether this was built in test/debug or release |
//! | `isDocker` | `false` | no container-runtime detection is implemented |
//! | `startTime` | RFC 3339 (e.g. `"2024-01-01T00:00:00+00:00"`) | see [`router`]'s `start_time` parameter |
//! | `appData` | `""` | no configured data-directory surface exists yet |
//! | `authentication` | `"none"` | Prowlarr's `AuthenticationType` enum names a *UI login* method Oxidarr has none of; this is unrelated to, and does not replace, the `apikey` every `/api/v1` route already requires |
//! | `urlBase` | `""` | no reverse-proxy base-path support exists yet |
//!
//! Skipped entirely — not modeled on [`SystemStatus`] at all, so never
//! serialized: `buildTime`, `isDebug`, `isAdmin`, `isUserInteractive`,
//! `startupPath`, `osName`, `osVersion`, `isNetCore`, `isLinux`, `isOsx`,
//! `isWindows`, `isContainerized`, `mode`, `branch`, `databaseType`,
//! `databaseVersion`, `migrationVersion`, `runtimeVersion`, `runtimeName`,
//! `packageVersion`, `packageAuthor`, `packageUpdateMechanism`,
//! `packageUpdateMechanismMessage` — every one of these names a
//! Windows/.NET-runtime, update-mechanism, or database-engine concept this
//! Rust/SQLite server either has no equivalent of or nothing meaningful to
//! report for yet. `buildTime` in particular mirrors real Prowlarr's own
//! serialization convention for "nothing to report" rather than being cut
//! purely for scope: `src/NzbDrone.Common/Serializer/Newtonsoft.Json/Json.cs`
//! (same commit) configures `NullValueHandling.Ignore`, so a field Prowlarr
//! itself has no value for is omitted from the response, never serialized
//! as `null`.
//!
//! `HealthController.GetHealth` returns `List<HealthResource>` — `[]` when
//! no health check has run or found a problem. [`health`] always returns
//! `[]`: Oxidarr has no health-check subsystem yet, so "no known problems"
//! is the only honest answer it can give.
//!
//! All field names use Prowlarr's own casing convention: real Prowlarr
//! serializes through Newtonsoft's `CamelCasePropertyNamesContractResolver`
//! (same `Json.cs`) — camelCase field names — which [`SystemStatus`]
//! mirrors with `#[serde(rename_all = "camelCase")]`.

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::Serialize;

/// State behind [`router`]'s two routes: just the fixed instant `router`
/// itself was built with, echoed back as `SystemStatus::start_time`.
#[derive(Clone)]
struct SystemState {
    start_time: DateTime<Utc>,
}

/// Builds `GET /system/status` and `GET /health`, relative routes meant to
/// be nested under `/api/v1` by [`crate::api::api_router`] — this function
/// does not add that prefix itself.
///
/// `start_time` is reported verbatim as `SystemStatus::start_time` on every
/// `/system/status` response for as long as the returned [`Router`] lives.
/// It is a plain parameter rather than a clock read internally so a test
/// can pin it to an exact value instead of asserting against a moving
/// target; see [`crate::server::app`] for where a real caller supplies
/// `Utc::now()`.
pub fn router(start_time: DateTime<Utc>) -> Router {
    Router::new()
        .route("/system/status", get(status))
        .route("/health", get(health))
        .with_state(SystemState { start_time })
}

/// Prowlarr's `SystemResource`, cut to the fields this server can answer
/// honestly. See the module doc's field table for what each value is and
/// why, and for the full list of what's skipped.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct SystemStatus {
    app_name: &'static str,
    instance_name: &'static str,
    version: &'static str,
    is_production: bool,
    is_docker: bool,
    start_time: String,
    app_data: &'static str,
    authentication: &'static str,
    url_base: &'static str,
}

async fn status(State(state): State<SystemState>) -> Json<SystemStatus> {
    Json(SystemStatus {
        app_name: "Oxidarr",
        instance_name: "Oxidarr",
        version: env!("CARGO_PKG_VERSION"),
        is_production: true,
        is_docker: false,
        start_time: state.start_time.to_rfc3339(),
        app_data: "",
        authentication: "none",
        url_base: "",
    })
}

/// `GET /health`: always `[]`. See the module doc's "Prowlarr shapes"
/// section for why an empty JSON array is the correct, honest shape here.
async fn health() -> Json<Vec<serde_json::Value>> {
    Json(Vec::new())
}
