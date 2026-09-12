//! Wire-shape DTOs mirroring `oxidarr-prowl`'s `/api/v1` response and
//! request bodies exactly. `crates/oxidarr-ui/tests/wire_parity.rs` proves
//! every one of these deserializes a committed fixture and re-serializes it
//! byte-for-byte — the same fixtures
//! `crates/oxidarr-prowl/tests/wire_fixtures.rs` pins from the server's own
//! types. Read `crates/oxidarr-prowl/src/api/dto.rs` and
//! `crates/oxidarr-prowl/src/api/system.rs` for the canonical field-by-field
//! rationale (Prowlarr source citations, honest-subset choices, ...) — this
//! module does not duplicate that reasoning, only the shape itself. Only
//! genuine, deliberate differences from the server-side types are called
//! out below.
//!
//! # Deliberate differences from the server-side types
//!
//! - [`SystemStatus`]'s fields are `String`, not `&'static str`: this side
//!   deserializes a response it does not own the memory of, so nothing here
//!   can borrow `'static`.
//! - Every type here derives both [`serde::Serialize`] and
//!   [`serde::Deserialize`] — [`IndexerResource`]/[`ApplicationResource`]
//!   already did on the server (their wire shape is reused for request
//!   bodies there too), but [`ReleaseResource`] gains `Deserialize` here
//!   (this client only ever reads one back from `GET /search`) and
//!   [`SystemStatus`] gains both (the server's own type only serializes).
//!   The wire-parity test needs both directions to prove a round trip, and
//!   nothing is lost by a response type also being constructible.
//! - [`ReleaseResource`]'s optional string fields (`guid`, `info_url`,
//!   `download_url`, `magnet_url`, `publish_date`) carry `#[serde(default)]`
//!   here, which the server's write-only type has no need for — this side
//!   must tolerate a fixture/response that omits one of them entirely, not
//!   just one that sends it as `null`.
//! - [`SearchParams`] is this client's own request-builder type, with no
//!   server-side counterpart to mirror: see its own doc comment.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Mirrors `oxidarr_prowl::api::system::SystemStatus`'s wire shape exactly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemStatus {
    pub app_name: String,
    pub instance_name: String,
    pub version: String,
    pub is_production: bool,
    pub is_docker: bool,
    pub start_time: String,
    pub app_data: String,
    pub authentication: String,
    pub url_base: String,
}

/// Mirrors `oxidarr_prowl::api::dto::IndexerResource` exactly, field for
/// field, `serde` attribute for `serde` attribute.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexerResource {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub id: i32,
    pub name: String,
    pub implementation: String,
    pub definition_name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub enable: bool,
    #[serde(default = "default_priority")]
    pub priority: i32,
    #[serde(default)]
    pub capabilities: IndexerCapabilities,
    #[serde(default)]
    pub fields: Vec<Field>,
    /// Populated when the server's own create/update handlers hit a
    /// best-effort app-sync failure — see the server-side
    /// `IndexerResource::sync_error`'s own doc comment. Never sent by this
    /// client; only ever read back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_error: Option<String>,
}

/// [`IndexerResource::priority`]'s default when a fixture/response omits the
/// field entirely — matches the server's own default exactly.
const fn default_priority() -> i32 {
    25
}

/// Mirrors the server's own `is_zero` — see [`IndexerResource::id`] and
/// [`ApplicationResource::id`].
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(id: &i32) -> bool {
    *id == 0
}

/// Mirrors `oxidarr_prowl::api::dto::IndexerCapabilities`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexerCapabilities {
    #[serde(default)]
    pub categories: Vec<IndexerCategory>,
}

/// Mirrors `oxidarr_prowl::api::dto::IndexerCategory`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexerCategory {
    pub id: u32,
    pub name: String,
}

/// Mirrors `oxidarr_prowl::api::dto::Field`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Field {
    pub name: String,
    #[serde(default)]
    pub label: String,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub select_options: Option<Vec<SelectOption>>,
}

/// Mirrors `oxidarr_prowl::api::dto::SelectOption`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelectOption {
    pub value: usize,
    pub name: String,
}

/// Mirrors `oxidarr_db::SyncLevel`'s wire shape (`"disabled"` / `"addOnly"`
/// / `"fullSync"`) — declared locally rather than depending on that
/// native-only (`sqlx`-carrying) crate for one enum's shape, matching this
/// crate's wasm-independence requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncLevel {
    Disabled,
    AddOnly,
    FullSync,
}

/// Mirrors `oxidarr_prowl::api::dto::ApplicationResource`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationResource {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub id: i32,
    pub name: String,
    pub implementation: String,
    pub sync_level: SyncLevel,
    #[serde(default)]
    pub fields: Vec<Field>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_error: Option<String>,
}

/// Mirrors `oxidarr_prowl::api::dto::ReleaseResource` — see this module's
/// own "Deliberate differences" section for the one attribute difference
/// (`#[serde(default)]` on the optional string fields).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseResource {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub guid: Option<String>,
    pub title: String,
    pub size: u64,
    pub seeders: Option<u32>,
    pub leechers: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub info_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub download_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub magnet_url: Option<String>,
    pub indexer_id: i32,
    pub indexer: String,
    pub categories: Vec<u32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub publish_date: Option<String>,
    pub grabs: Option<u32>,
    pub download_volume_factor: f32,
    pub upload_volume_factor: f32,
}

/// This client's own query parameters for `GET /api/v1/search` — mirrors
/// the server's private `SearchParams`
/// (`crates/oxidarr-prowl/src/api/search.rs`), minus the `type`/
/// `search_type` parameter: that module's own doc comment documents the
/// server accepting but never acting on it, so this client has no honest
/// use for sending it either.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchParams {
    pub query: Option<String>,
    pub indexer_ids: Vec<i32>,
    pub categories: Vec<u32>,
}

impl SearchParams {
    /// Builds this request's query string: `""` (no leading `?`) when every
    /// field is at its default, otherwise a leading `?` followed by
    /// whichever of `query`/`indexerIds`/`categories` are set, in that
    /// order. `indexerIds`/`categories` are comma-joined — one of the two
    /// list encodings the server's own `parse_search_params` accepts (see
    /// that function's own doc comment for why it accepts both a
    /// comma-joined value and repeated keys).
    #[must_use]
    pub fn to_query_string(&self) -> String {
        let mut parts = Vec::new();
        if let Some(query) = &self.query {
            parts.push(format!("query={}", percent_encode(query)));
        }
        if !self.indexer_ids.is_empty() {
            parts.push(format!("indexerIds={}", join(&self.indexer_ids)));
        }
        if !self.categories.is_empty() {
            parts.push(format!("categories={}", join(&self.categories)));
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!("?{}", parts.join("&"))
        }
    }
}

/// Comma-joins `values`' own `Display` rendering — used for `indexerIds`/
/// `categories`.
fn join<T: std::fmt::Display>(values: &[T]) -> String {
    values
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// A minimal RFC 3986 query-component percent-encoder: unreserved bytes
/// (`ALPHA` / `DIGIT` / `-` / `.` / `_` / `~`) pass through verbatim; every
/// other byte becomes `%XX` (uppercase hex). This only needs to produce a
/// query string the server's own `url::form_urlencoded`-based parser
/// (`crates/oxidarr-prowl/src/api/search.rs`) reads back correctly, not to
/// match any particular browser or library's own encoder byte-for-byte.
fn percent_encode(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            other => {
                out.push('%');
                out.push(HEX[(other >> 4) as usize] as char);
                out.push(HEX[(other & 0x0F) as usize] as char);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn an_empty_search_params_builds_no_query_string() {
        assert_eq!(SearchParams::default().to_query_string(), "");
    }

    #[test]
    fn a_query_value_is_percent_encoded() {
        let params = SearchParams {
            query: Some("a b&c".to_string()),
            ..SearchParams::default()
        };
        assert_eq!(params.to_query_string(), "?query=a%20b%26c");
    }

    #[test]
    fn indexer_ids_and_categories_are_comma_joined() {
        let params = SearchParams {
            indexer_ids: vec![1, 2, 3],
            categories: vec![5000, 5030],
            ..SearchParams::default()
        };
        assert_eq!(
            params.to_query_string(),
            "?indexerIds=1,2,3&categories=5000,5030"
        );
    }

    #[test]
    fn all_three_params_combine_in_a_fixed_order() {
        let params = SearchParams {
            query: Some("ubuntu".to_string()),
            indexer_ids: vec![1],
            categories: vec![5000],
        };
        assert_eq!(
            params.to_query_string(),
            "?query=ubuntu&indexerIds=1&categories=5000"
        );
    }

    #[test]
    fn a_zero_id_is_not_serialized_on_an_indexer_resource() {
        let resource = IndexerResource {
            id: 0,
            name: "Example".to_string(),
            implementation: "Cardigann".to_string(),
            definition_name: "example".to_string(),
            description: String::new(),
            language: String::new(),
            enable: false,
            priority: 25,
            capabilities: IndexerCapabilities::default(),
            fields: Vec::new(),
            sync_error: None,
        };
        let json = serde_json::to_string(&resource).unwrap();
        assert!(!json.contains("\"id\""), "json was: {json}");
    }

    #[test]
    fn sync_level_uses_the_servers_own_camel_case_strings() {
        assert_eq!(
            serde_json::to_string(&SyncLevel::AddOnly).unwrap(),
            "\"addOnly\""
        );
        assert_eq!(
            serde_json::to_string(&SyncLevel::FullSync).unwrap(),
            "\"fullSync\""
        );
        assert_eq!(
            serde_json::to_string(&SyncLevel::Disabled).unwrap(),
            "\"disabled\""
        );
    }
}
