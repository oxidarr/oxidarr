//! Shared Prowlarr-shaped response DTOs, reused by more than one `/api/v1`
//! endpoint. Task 4 ([`crate::api::indexer_schema`]) starts this module with
//! the indexer schema shapes; Task 5 ([`crate::api::indexers`]) and Task 6
//! ([`crate::api::applications`]) extend it with indexer/application CRUD
//! payloads.
//!
//! # Prowlarr shapes
//!
//! Read `src/Prowlarr.Api.V1/Indexers/IndexerResource.cs`,
//! `src/Prowlarr.Api.V1/ProviderResource.cs`,
//! `src/Prowlarr.Http/REST/RestResource.cs`,
//! `src/Prowlarr.Http/ClientSchema/Field.cs`,
//! `src/Prowlarr.Api.V1/Indexers/IndexerCapabilityResource.cs`,
//! `src/NzbDrone.Core/Indexers/IndexerCategory.cs`,
//! `src/NzbDrone.Core/ThingiProvider/ProviderDefinition.cs`,
//! `src/NzbDrone.Core/Indexers/IndexerDefinition.cs`,
//! `src/Prowlarr.Api.V1/Applications/ApplicationResource.cs`, and
//! `src/NzbDrone.Core/Applications/Sonarr/SonarrSettings.cs` from
//! `Prowlarr/Prowlarr` at commit `693c7c3b0e8ec6e9dd792c01e5fa1091260b3be1`
//! (the same commit [`crate::api::system`] cites). See [`ApplicationResource`]'s
//! own doc comment for the application-specific citations.
//!
//! [`IndexerResource`] ships the honest subset of `ProviderResource<T>` +
//! `IndexerResource`'s ~20 combined fields that this crate's own indexer
//! schema/CRUD endpoints actually need. The acceptance consumer is a human
//! reading this JSON, plus this plan's own indexer CRUD — not Prowlarr's
//! Angular UI, which is the only consumer of fields this DTO omits entirely:
//! `sortName`, `infoLink`, `added`, `tags`, `message`, `redirect`,
//! `protocol`, `privacy`, `supportsRss`/`supportsSearch`/`supportsRedirect`/
//! `supportsPagination`, `encoding`, `appProfileId`, `downloadClientId`,
//! `indexerUrls`/`legacyUrls`, and `status`.
//!
//! | Field | Source |
//! |---|---|
//! | `id` | `RestResource.Id`, declared `[JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingDefault)]` — omitted (not serialized as `0`) for a resource with no database row yet, which is every entry [`crate::api::indexer_schema`] returns |
//! | `name` | `ProviderResource<T>.Name` |
//! | `implementation` | `ProviderResource<T>.Implementation` — always `"Cardigann"` here, this crate's only indexer implementation |
//! | `definitionName` | `IndexerResource.DefinitionName`, overwritten by `IndexerResourceMapper.ToResource` with `CardigannSettings.DefinitionFile` for a Cardigann indexer — the definition's file stem |
//! | `description` | `IndexerResource.Description` |
//! | `language` | `IndexerResource.Language` |
//! | `enable` | `ProviderDefinition.Enable`, a `virtual bool` with no initializer — `false` by default, the value every not-yet-configured schema entry carries |
//! | `priority` | `IndexerDefinition.Priority = 25`, that class's own default (not `0`) |
//! | `capabilities` | [`IndexerCapabilities`] |
//! | `fields` | [`Field`], one per `Setting` via [`Field::from_setting`] |
//!
//! # Field type mapping
//!
//! `IndexerResourceMapper.MapCardigannField` (in `IndexerResource.cs`, same
//! commit) maps a Cardigann `Setting`'s `type:` onto a `Field.Type` string:
//! `text` becomes `"textbox"` (`field.Type = setting.Type == "text" ?
//! "textbox" : setting.Type`); every other type — `password`, `checkbox`,
//! `select`, `info`, ... — passes through unchanged. [`Field::from_setting`]
//! mirrors that: `"text"` → `"textbox"`; every other `type:` string
//! (`"password"`, `"checkbox"`, `"select"`, ...) passes through verbatim.
//!
//! `selectOptions` for a `select` field comes from `Setting.options`. Real
//! Prowlarr sorts options by key and encodes each as `{value: <index>, name:
//! <option's map value>}`, with `Field.Value` itself an index into that same
//! sorted list (`sorted.Select(x => x.Key).ToList().IndexOf(setting.Default)`)
//! — an integer, not the underlying string. That index encoding exists to
//! match Prowlarr's own Angular `<select>` component; nothing in Oxidarr
//! renders that component, and the acceptance consumer here is a human (or
//! this plan's own CRUD) reading the JSON directly. [`Field::from_setting`]
//! keeps `selectOptions[i]` as the same `{value: i, name: <option's map
//! value>}` in the same key-sorted order (`Setting::options` is already a
//! `BTreeMap`, so no explicit sort is needed) but reports the field's own
//! `value` as the raw default string (e.g. `"seeders"`), not its index —
//! this module's one deliberate deviation from real Prowlarr.

use std::collections::BTreeMap;

use chrono::SecondsFormat;
use oxidarr_cardigann::model::Setting;
use oxidarr_core::Release;
use oxidarr_db::SyncLevel;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Prowlarr's `IndexerResource`, cut to the fields in this module's doc
/// table.
///
/// Deserializes as well as serializes: Task 5's indexer CRUD reuses this
/// exact shape for `POST`/`PUT /indexer` request bodies, not just
/// `GET /indexer/schema` responses. `implementation` is a plain `String`
/// (not `&'static str`, unlike this doc table's Rust-side source) purely so
/// a client-supplied value can deserialize into it at all; every field a
/// client might reasonably omit on create (`id`, `description`, `language`,
/// `enable`, `priority`, `capabilities`, `fields`) carries `#[serde(default)]`
/// so a partial body still parses.
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
}

/// [`IndexerResource::priority`]'s default when a client's request body
/// omits the field entirely — `IndexerDefinition.Priority`'s own default
/// (see this module's doc table), matching what
/// [`crate::api::indexer_schema`] already reports for every not-yet-
/// configured schema entry.
const fn default_priority() -> i32 {
    25
}

/// Mirrors `RestResource.Id`'s own `JsonIgnoreCondition.WhenWritingDefault`
/// — see this module's doc table.
///
/// Takes `&i32` rather than `i32` even though `i32` is `Copy` — serde's
/// `skip_serializing_if` always calls this as `fn(&T) -> bool`, so a
/// by-value signature would not compile as the attribute's target.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(id: &i32) -> bool {
    *id == 0
}

/// Prowlarr's `IndexerCapabilityResource`, cut to `categories` — the only
/// piece [`crate::api::indexer_schema`] needs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexerCapabilities {
    #[serde(default)]
    pub categories: Vec<IndexerCategory>,
}

/// Prowlarr's `IndexerCategory`, cut to `id`/`name`. Real Prowlarr nests
/// subcategories under their parent block (`IndexerCategory.SubCategories`,
/// via `GetTorznabCategoryTree` — the same nesting
/// [`crate::torznab::render_caps`]'s Torznab XML renders); see
/// [`crate::api::indexer_schema`]'s module docs for why this endpoint
/// deliberately flattens that away instead.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexerCategory {
    pub id: u32,
    pub name: String,
}

/// Prowlarr's `Field` (`Prowlarr.Http.ClientSchema.Field`), cut to the
/// subset mapped from a Cardigann [`Setting`]. See the module docs' "Field
/// type mapping" section for `kind`/`value`/`select_options` semantics.
///
/// Deserializes as well as serializes — see [`IndexerResource`]'s own doc
/// comment. Only `name` is required on input: Task 5's write path
/// (`crate::api::indexers`) reads only `name`/`value` off each `Field` to
/// build an indexer's settings map, so `label`/`type`/`select_options` all
/// default when a client omits them.
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

impl Field {
    /// Maps a Cardigann [`Setting`] onto Prowlarr's `Field` shape. See the
    /// module docs' "Field type mapping" section for the exact mapping and
    /// its one deliberate deviation from real Prowlarr.
    #[must_use]
    pub fn from_setting(setting: &Setting) -> Self {
        let kind = match setting.kind.as_str() {
            "text" => "textbox".to_string(),
            other => other.to_string(),
        };
        let select_options = (setting.kind == "select").then(|| select_options(&setting.options));
        Self {
            name: setting.name.clone(),
            label: setting.label.clone(),
            kind,
            value: field_value(setting),
            select_options,
        }
    }
}

/// Builds `selectOptions` from a `select` setting's `options`, in key order
/// (already the case: [`Setting::options`] is a `BTreeMap`) — see the
/// module docs' "Field type mapping" section.
fn select_options(options: &BTreeMap<String, String>) -> Vec<SelectOption> {
    options
        .values()
        .enumerate()
        .map(|(value, name)| SelectOption {
            value,
            name: name.clone(),
        })
        .collect()
}

/// A `checkbox` setting's default renders as a real JSON boolean (matching
/// real Prowlarr's own `bool.TryParse` coercion); every other kind's default
/// renders as its literal string. `None` when the setting declares no
/// `default:` at all.
fn field_value(setting: &Setting) -> Option<Value> {
    let default = setting.default.as_deref()?;
    if setting.kind == "checkbox" {
        Some(Value::Bool(default == "true"))
    } else {
        Some(Value::String(default.to_string()))
    }
}

/// One `select` field's option, `{value, name}` in Prowlarr's own shape —
/// see the module docs' "Field type mapping" section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelectOption {
    pub value: usize,
    pub name: String,
}

/// Prowlarr's `ApplicationResource`
/// (`src/Prowlarr.Api.V1/Applications/ApplicationResource.cs`, same commit
/// as this module's other citations), cut to the fields
/// [`crate::api::applications`]'s CRUD actually needs.
///
/// `ApplicationResource` itself is a thin subclass of the same
/// `ProviderResource<T>` base [`IndexerResource`] draws `id`/`name`/
/// `implementation`/`fields` from, plus its own `syncLevel`. This crate
/// omits every `ProviderResource` field [`IndexerResource`] already omits
/// (`configContract`, `infoLink`, `message`, `tags`, `status`, ...) for the
/// same reason: no consumer here reads Prowlarr's Angular UI shape, only a
/// human or this plan's own CRUD reading the JSON directly.
///
/// `fields` carries `baseUrl`/`apiKey` — confirmed against
/// `src/NzbDrone.Core/Applications/Sonarr/SonarrSettings.cs` (same commit),
/// whose `BaseUrl`/`ApiKey` properties are exactly [`crate::api::applications`]'s
/// two settings (Radarr's own `RadarrSettings.cs` declares the identical
/// pair). Real Prowlarr's `SonarrSettingsValidator` requires `ApiKey` to be
/// non-empty and `BaseUrl` to be a valid URL — [`crate::api::applications`]'s
/// own validation mirrors both rules.
///
/// Deserializes as well as serializes, for the same reason
/// [`IndexerResource`] does: `POST`/`PUT /applications` request bodies reuse
/// this exact shape.
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
}

/// Prowlarr's `ReleaseResource`
/// (`src/Prowlarr.Api.V1/Search/ReleaseResource.cs` at commit
/// `693c7c3b0e8ec6e9dd792c01e5fa1091260b3be1`, unchanged on `develop` as of
/// this writing), cut to the fields [`crate::api::search`]'s `GET
/// /api/v1/search` endpoint can actually populate from an
/// [`oxidarr_core::Release`].
///
/// # Field mapping and honest-subset choices
///
/// | Field | Source | Note |
/// |---|---|---|
/// | `guid` | always `None` | Real Prowlarr's `ReleaseInfo.Guid` comes from the raw feed's own `<guid>` element. [`oxidarr_indexer::rss::parse_feed`] only ever folds a `<guid>` into [`Release::details_url`] when it looks like a URL (see that module's own doc comment) — it is never kept as a value of its own on [`Release`], so there is nothing honest to put here yet |
/// | `title` | `Release::title` | |
/// | `size` | `Release::size` | `long Size` on the real resource is never null (`Size = releaseInfo.Size ?? 0`); mirrored here as a plain `u64` defaulting to `0`, not `Option<u64>` |
/// | `seeders`/`leechers` | `Release::seeders`/`Release::leechers` | real `int?`, nullable both sides |
/// | `infoUrl` | `Release::details_url` | |
/// | `downloadUrl` | `Release::download_url` | |
/// | `magnetUrl` | `Release::magnet_url` | |
/// | `indexerId`/`indexer` | the [`oxidarr_db::IndexerRow`] the search ran against, not `Release` itself — one release carries no notion of which indexer it came from until [`crate::api::search`] attaches it | |
/// | `categories` | `Release::categories` | real `Categories` is `ICollection<IndexerCategory>` (`{id, name}` objects, the same shape [`IndexerCategory`] models); this endpoint reports bare Newznab category ids instead — the same flattening judgment call [`IndexerCapabilities`]'s own doc comment already makes for `t=caps`, extended here rather than re-litigated |
/// | `publishDate` | `Release::publish_date` | RFC 3339, `Z`-suffixed via [`chrono::DateTime::to_rfc3339_opts`]; `None` renders as an absent field rather than real Prowlarr's non-nullable `DateTime` (which defaults to `0001-01-01T00:00:00` when unset) — fabricating a fake epoch is worse than omitting a date nobody parsed out of the feed |
/// | `grabs` | `Release::grabs` | |
/// | `downloadVolumeFactor`/`uploadVolumeFactor` | `Release::download_volume_factor`/`Release::upload_volume_factor` | **not** part of real Prowlarr's wire `ReleaseResource` at all — verified against both the pinned commit and current `develop`: the two properties live only on the internal `TorrentInfo` domain model (`src/NzbDrone.Core/Parser/Model/TorrentInfo.cs`) and `ReleaseResourceMapper.ToResource` never copies them onto the resource it serializes. Included here anyway as a deliberate *addition* beyond the Prowlarr-shaped subset — `oxidarr_core::Release` already carries this per-release ratio data, and a client deciding whether a private-tracker grab is worth its hit-and-run risk benefits from it being on the wire even though real Prowlarr never puts it there |
///
/// `Age`/`AgeHours`/`AgeMinutes` are skipped entirely (not even as `None`):
/// they are computed properties on real Prowlarr's `ReleaseInfo`
/// (`DateTime.UtcNow.Subtract(PublishDate)`, evaluated at serialization
/// time), not stored data — reproducing them would mean either running that
/// same clock-dependent computation here (making every response's JSON
/// depend on wall-clock time at render, not just at fetch, with no test
/// value) or reporting a value frozen at fetch time under a name that
/// promises otherwise. Neither is an honest subset, so both are left out.
/// `id`, `Files`, `SubGroup`, `ReleaseHash`, `SortTitle`, `ImdbId`/`TmdbId`/
/// `TvdbId`/`TvMazeId`, `CommentUrl`, `PosterUrl`, `IndexerFlags`,
/// `InfoHash`, `Protocol`, `FileName`, and `DownloadClientId` are omitted for
/// the same reason [`ApplicationResource`]'s own doc comment gives for its
/// own omissions: no consumer here needs Prowlarr's Angular UI shape, only a
/// human or this plan's own client reading the JSON directly.
///
/// Serializes only (no `Deserialize`) — unlike [`IndexerResource`]/
/// [`ApplicationResource`], nothing in this crate ever reads a
/// `ReleaseResource` back in as a request body.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseResource {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guid: Option<String>,
    pub title: String,
    pub size: u64,
    pub seeders: Option<u32>,
    pub leechers: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub magnet_url: Option<String>,
    pub indexer_id: i32,
    pub indexer: String,
    pub categories: Vec<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publish_date: Option<String>,
    pub grabs: Option<u32>,
    pub download_volume_factor: f32,
    pub upload_volume_factor: f32,
}

impl ReleaseResource {
    /// Builds a `ReleaseResource` from one normalised [`Release`], attaching
    /// the `indexer_id`/`indexer_name` of the indexer it was fetched from —
    /// see this type's own doc comment for the exact field mapping.
    #[must_use]
    pub fn from_release(release: Release, indexer_id: i32, indexer_name: &str) -> Self {
        Self {
            guid: None,
            title: release.title,
            size: release.size.unwrap_or(0),
            seeders: release.seeders,
            leechers: release.leechers,
            info_url: release.details_url,
            download_url: release.download_url,
            magnet_url: release.magnet_url,
            indexer_id,
            indexer: indexer_name.to_string(),
            categories: release.categories,
            publish_date: release
                .publish_date
                .map(|date| date.to_rfc3339_opts(SecondsFormat::Secs, true)),
            grabs: release.grabs,
            download_volume_factor: release.download_volume_factor,
            upload_volume_factor: release.upload_volume_factor,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use chrono::{DateTime, Utc};

    use super::*;

    fn setting(name: &str, kind: &str) -> Setting {
        Setting {
            name: name.to_string(),
            kind: kind.to_string(),
            label: String::new(),
            default: None,
            options: BTreeMap::new(),
        }
    }

    #[test]
    fn text_maps_to_textbox() {
        let field = Field::from_setting(&setting("cookie", "text"));
        assert_eq!(field.kind, "textbox");
    }

    #[test]
    fn password_and_checkbox_pass_through_unchanged() {
        assert_eq!(
            Field::from_setting(&setting("apikey", "password")).kind,
            "password"
        );
        assert_eq!(
            Field::from_setting(&setting("freeleech", "checkbox")).kind,
            "checkbox"
        );
    }

    #[test]
    fn a_setting_with_no_default_has_no_value() {
        let field = Field::from_setting(&setting("cookie", "text"));
        assert_eq!(field.value, None);
    }

    #[test]
    fn checkbox_default_becomes_a_json_bool() {
        let mut setting = setting("freeleech", "checkbox");
        setting.default = Some("true".to_string());
        let field = Field::from_setting(&setting);
        assert_eq!(field.value, Some(Value::Bool(true)));
    }

    #[test]
    fn select_default_stays_the_raw_string_and_options_are_key_sorted() {
        let mut options = BTreeMap::new();
        options.insert("size".to_string(), "Size".to_string());
        options.insert("seeders".to_string(), "Seeders".to_string());
        let setting = Setting {
            name: "sort".to_string(),
            kind: "select".to_string(),
            label: String::new(),
            default: Some("seeders".to_string()),
            options,
        };
        let field = Field::from_setting(&setting);
        assert_eq!(field.value, Some(Value::String("seeders".to_string())));
        assert_eq!(
            field.select_options,
            Some(vec![
                SelectOption {
                    value: 0,
                    name: "Seeders".to_string()
                },
                SelectOption {
                    value: 1,
                    name: "Size".to_string()
                },
            ])
        );
    }

    #[test]
    fn release_resource_from_release_carries_the_attached_indexer_identity() {
        let mut release = Release::default();
        release.title = "Some.Show.S01E01".to_string();
        release.seeders = Some(12);

        let resource = ReleaseResource::from_release(release, 7, "My Indexer");

        assert_eq!(resource.indexer_id, 7);
        assert_eq!(resource.indexer, "My Indexer");
        assert_eq!(resource.title, "Some.Show.S01E01");
        assert_eq!(resource.seeders, Some(12));
    }

    #[test]
    fn release_resource_guid_is_always_none() {
        // See this type's own doc comment: `Release` carries no raw `<guid>`
        // of its own to report here.
        let resource = ReleaseResource::from_release(Release::default(), 1, "Indexer");
        assert_eq!(resource.guid, None);
    }

    #[test]
    fn release_resource_defaults_a_missing_size_to_zero_not_null() {
        let resource = ReleaseResource::from_release(Release::default(), 1, "Indexer");
        assert_eq!(resource.size, 0);
    }

    #[test]
    fn release_resource_formats_publish_date_as_rfc3339_with_a_z_suffix() {
        let published = DateTime::parse_from_rfc3339("2026-09-06T12:34:56+00:00")
            .unwrap()
            .with_timezone(&Utc);
        let mut release = Release::default();
        release.publish_date = Some(published);

        let resource = ReleaseResource::from_release(release, 1, "Indexer");

        assert_eq!(
            resource.publish_date.as_deref(),
            Some("2026-09-06T12:34:56Z")
        );
    }

    #[test]
    fn release_resource_publish_date_is_omitted_from_json_when_absent() {
        let resource = ReleaseResource::from_release(Release::default(), 1, "Indexer");
        let json = serde_json::to_string(&resource).unwrap();
        assert!(!json.contains("publishDate"), "json was: {json}");
    }

    #[test]
    fn a_zero_id_is_not_serialized() {
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
        };
        let json = serde_json::to_string(&resource).unwrap();
        assert!(!json.contains("\"id\""), "json was: {json}");
    }
}
