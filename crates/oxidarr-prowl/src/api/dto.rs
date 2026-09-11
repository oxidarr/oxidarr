//! Shared Prowlarr-shaped response DTOs, reused by more than one `/api/v1`
//! endpoint. Task 4 ([`crate::api::indexer_schema`]) starts this module with
//! the indexer schema shapes; later tasks in this plan extend it with
//! indexer/application CRUD payloads.
//!
//! # Prowlarr shapes
//!
//! Read `src/Prowlarr.Api.V1/Indexers/IndexerResource.cs`,
//! `src/Prowlarr.Api.V1/ProviderResource.cs`,
//! `src/Prowlarr.Http/REST/RestResource.cs`,
//! `src/Prowlarr.Http/ClientSchema/Field.cs`,
//! `src/Prowlarr.Api.V1/Indexers/IndexerCapabilityResource.cs`,
//! `src/NzbDrone.Core/Indexers/IndexerCategory.cs`,
//! `src/NzbDrone.Core/ThingiProvider/ProviderDefinition.cs`, and
//! `src/NzbDrone.Core/Indexers/IndexerDefinition.cs` from
//! `Prowlarr/Prowlarr` at commit `693c7c3b0e8ec6e9dd792c01e5fa1091260b3be1`
//! (the same commit [`crate::api::system`] cites).
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

use oxidarr_cardigann::model::Setting;
use serde::Serialize;
use serde_json::Value;

/// Prowlarr's `IndexerResource`, cut to the fields in this module's doc
/// table.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexerResource {
    #[serde(skip_serializing_if = "is_zero")]
    pub id: i32,
    pub name: String,
    pub implementation: &'static str,
    pub definition_name: String,
    pub description: String,
    pub language: String,
    pub enable: bool,
    pub priority: i32,
    pub capabilities: IndexerCapabilities,
    pub fields: Vec<Field>,
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
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexerCapabilities {
    pub categories: Vec<IndexerCategory>,
}

/// Prowlarr's `IndexerCategory`, cut to `id`/`name`. Real Prowlarr nests
/// subcategories under their parent block (`IndexerCategory.SubCategories`,
/// via `GetTorznabCategoryTree` — the same nesting
/// [`crate::torznab::render_caps`]'s Torznab XML renders); see
/// [`crate::api::indexer_schema`]'s module docs for why this endpoint
/// deliberately flattens that away instead.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IndexerCategory {
    pub id: u32,
    pub name: String,
}

/// Prowlarr's `Field` (`Prowlarr.Http.ClientSchema.Field`), cut to the
/// subset mapped from a Cardigann [`Setting`]. See the module docs' "Field
/// type mapping" section for `kind`/`value`/`select_options` semantics.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Field {
    pub name: String,
    pub label: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
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
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SelectOption {
    pub value: usize,
    pub name: String,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

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
    fn a_zero_id_is_not_serialized() {
        let resource = IndexerResource {
            id: 0,
            name: "Example".to_string(),
            implementation: "Cardigann",
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
