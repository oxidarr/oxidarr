//! The indexers screen: list (name/implementation/priority/enable toggle,
//! delete-with-confirm, test-with-inline-result, `syncError` badge), an add
//! flow (a definition picker sourced from `GET /indexer/schema`, searchable
//! by name, followed by a settings form rendered from the chosen entry's
//! own `fields[]`), and edit (the same settings form, prefilled from an
//! existing indexer's own current values). Follows `crate::screens::status`'s
//! own split: every `*View` component below is plain props in, no
//! context, no fetching — trivially SSR-snapshotted (`tests/snapshots.rs`);
//! [`Indexers`] is the routed container that fetches from the shared
//! `ApiClient`, holds this screen's own interactive state, and feeds
//! whichever view the current [`ScreenState`] calls for.
//!
//! # State machine
//!
//! [`ScreenState`] is kept to three variants: `List`, `Add { schema }`,
//! `Edit { resource }`. The add flow's own
//! `schema` starts `None` (the definition picker shows) and becomes
//! `Some(entry)` in place, without ever leaving the `Add` variant, once a
//! definition is picked — no separate "picking" state is needed.
//!
//! # Field metadata vs. current value
//!
//! A schema entry's own `fields[]` carries each field's *metadata*
//! (`label`/`kind`/`selectOptions`) — this screen keeps that `Vec<Field>`
//! untouched in [`ScreenState`] and tracks the form's own *current* values
//! separately, in a `HashMap<String, String>` keyed by field name
//! (`crate::components::fields::field_value_as_string`/`value_for_field`
//! convert between that string state and the wire [`serde_json::Value`] —
//! see that module's own doc comment). Splitting metadata from current
//! value this way is what lets [`build_indexer_resource`] stay a pure,
//! natively-testable function with no `dioxus` involvement at all.
//!
//! An *existing* indexer's own `fields[]`, as `GET /api/v1/indexer` returns
//! it, is a different story: the server reconstructs it straight from the
//! persisted settings map (`fields_from_settings`,
//! `crates/oxidarr-prowl/src/api/indexers.rs`), which carries no metadata at
//! all — no `selectOptions`, `label` equal to the raw setting name, `kind`
//! only ever `"checkbox"`/`"textbox"`. Rendering the Edit form straight from
//! that would degrade a `select` field to free text and render a
//! `password` setting as plain cleartext. [`merge_field_metadata`] is the
//! fix: it takes the matching schema entry's own real metadata and pairs it
//! with the existing indexer's own current values.
//!
//! # Edit form must never open unmerged
//!
//! `merge_field_metadata` needs a *resolved* matching schema entry to run at
//! all — and this screen's `indexers` and `schema` are two independent
//! `use_resource`s, so the indexer list can (and, on a slow schema fetch,
//! routinely will) render before `schema` has resolved. A click on Edit in
//! that window used to open the form anyway, with `resource.fields` left in
//! the server's own reconstructed shape: a "graceful no-op" for a `select`
//! field degrading to free text or a raw name showing where a label
//! should, but not for `password` — a reconstructed field's `kind` reads as
//! plain `"textbox"`, which [`initial_form_values`] and
//! [`preserve_blank_passwords`] (see below) would then both trust
//! verbatim, displaying the real stored secret in the draft and letting a
//! blank save wipe it. [`resolve_schema_entry_for_edit`] is the gate
//! [`Indexers`]'s own `on_edit` handler now checks *first*: `None` — schema
//! still loading, or resolved with no matching entry — refuses to open
//! Edit at all (a banner explains why) rather than opening it with
//! unmerged fields either way. [`merge_field_metadata`] only ever runs
//! once that gate has already passed.
//!
//! # Password fields never prefill the secret
//!
//! See `crate::components::fields`'s own doc comment for the render-side
//! half of this (the password input never displays a field's actual
//! value). [`initial_form_values`] is this screen's own save-side half:
//! unlike [`initial_values`] (a raw, lossless extraction — what
//! [`toggle_enable`]/[`build_resource_for_test`] both need, since neither
//! goes through a form), it blanks out every `password`-kind field ([`is_password_field`]
//! decides which — see its own doc comment for why that's a
//! belt-and-braces check, not a bare `kind` comparison) so the form's own
//! draft state never holds the real secret either.
//! [`preserve_blank_passwords`] is the inverse, run just before building the
//! outbound payload: a `password` field left blank in the form's own
//! current values is restored from the schema-shaped fields it's given
//! (either a fresh schema entry, on Add — a no-op, nothing stored yet — or
//! [`ScreenState::Edit`]'s own merged fields), rather than sent as a
//! literal blank that would overwrite the stored secret.
//!
//! # Outbound `sync_error`
//!
//! [`build_indexer_resource`] never sets [`IndexerResource::sync_error`] —
//! it is always `None` on every outbound create/update payload, matching
//! that field's own doc comment ("Never sent by this client; only ever read
//! back").
//!
//! # Avoiding a `Signal` read guard held across `.await`
//!
//! Every mutating handler below (`update`/`create`/`delete`/`test`) clones
//! the shared `ApiClient` out of its `Signal` (`api.read().clone()` —
//! `crate::api::ApiClient`'s own `Clone` derive exists for exactly this)
//! into an owned local *before* the surrounding `async move` block's first
//! `.await`, rather than writing `api.read().create_indexer(...).await`
//! directly: the latter's temporary `Ref` guard would live across the
//! `.await` point (Rust's temporary-lifetime-extension keeps a chained
//! temporary alive for the whole statement), which risks a "already
//! borrowed" panic if anything else touches the same `Signal` while the
//! request is in flight.

use std::collections::HashMap;

use dioxus::prelude::*;

use crate::app::{Session, SharedApi, handle_error};
use crate::components::fields::{field_value_as_string, render_field, value_for_field};
use crate::dto::{Field, IndexerCapabilities, IndexerResource};

/// Outcome of a `POST /indexer/test` probe kept for one row (list) or the
/// open form (add/edit). Plain data, no `dioxus` involvement — this and
/// every function below that only touches this type stay natively
/// testable, unlike the `#[component]` views further down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestOutcome {
    /// The probe reached the indexer successfully.
    Ok,
    /// The probe failed; carries [`crate::api::UiError`]'s own rendered
    /// message (a `Problem`'s message, or a `Network`/`Decode` error's own
    /// prefixed text — whatever `crate::app::classify_error` would have
    /// shown in the banner for the same error, reused here so a failed test
    /// reads the same failure text a banner would).
    Failed(String),
}

/// This screen's own state machine — see this module's own doc comment.
#[derive(Debug, Clone, PartialEq)]
pub enum ScreenState {
    /// The indexer list — this screen's default and its "back" target from
    /// every other state.
    List,
    /// The add flow: `schema` is `None` while the definition picker shows,
    /// `Some(entry)` once a definition has been picked and the settings
    /// form is showing instead.
    Add { schema: Option<IndexerResource> },
    /// The edit flow: `resource` is the indexer being edited, as last
    /// fetched, with its own `fields[]` already replaced by
    /// [`merge_field_metadata`]'s own output — the matching schema entry's
    /// real metadata (`label`/`kind`/`selectOptions`) paired with this
    /// indexer's own current values (see this module's own doc comment) —
    /// by the time [`Indexers`]'s own `on_edit` handler sets this variant.
    /// [`initial_form_values`] seeds the form's initial per-field values
    /// from those same (already-merged) `fields[]`.
    Edit { resource: IndexerResource },
}

/// One `values` entry per `fields`, keyed by name, via
/// `crate::components::fields::field_value_as_string` — a raw, lossless
/// extraction with no per-`kind` special-casing. [`toggle_enable`] and
/// [`build_resource_for_test`] both use this directly: neither goes through
/// a form, so there is no "blank means unchanged" convention to apply —
/// both need every field's own real current value, `password` fields
/// included, to resubmit the resource unchanged apart from the one field
/// they're actually touching. [`initial_form_values`] is the form-seeding
/// counterpart that *does* blank out `password` fields.
#[must_use]
pub fn initial_values(fields: &[Field]) -> HashMap<String, String> {
    fields
        .iter()
        .map(|field| (field.name.clone(), field_value_as_string(field)))
        .collect()
}

/// True for a `password`-kind field — checked two ways: `field`'s own
/// `kind`, or, belt-and-braces, a name match in `schema_fields` whose own
/// `kind` is `"password"`. In the well-behaved path `field` already comes
/// from [`merge_field_metadata`]'s own output, so `field.kind` alone is
/// already authoritative and the `schema_fields` cross-check only confirms
/// what it already says. It exists for exactly the failure mode a real
/// review caught: `Indexers`'s own `on_edit` handler used to open the Edit
/// form even when no schema entry had resolved yet to merge onto a
/// resource's own fields, leaving them in the server's own reconstructed
/// shape — `kind: "textbox"` even for a setting the definition itself
/// calls `password` — which both [`initial_form_values`] and
/// [`preserve_blank_passwords`] would then trust verbatim. `on_edit` now
/// refuses to open Edit at all unless [`resolve_schema_entry_for_edit`]
/// found a match (see this module's own doc comment, "Edit form must never
/// open unmerged"), so this cross-check is redundant in the current,
/// correctly-gated call graph; it stays as the second, independent line of
/// defense a future change to that gate would have to break through too
/// before a password field could leak or be wiped again.
fn is_password_field(field: &Field, schema_fields: &[Field]) -> bool {
    field.kind == "password"
        || schema_fields
            .iter()
            .any(|schema_field| schema_field.name == field.name && schema_field.kind == "password")
}

/// The form's initial per-field state for a freshly chosen schema entry
/// (add) or an indexer being edited: [`initial_values`], with every
/// `password`-kind field's own value blanked out — [`is_password_field`]
/// decides which, checked against `schema_fields` (the schema entry this
/// screen actually fetched for this definition; the same slice as `fields`
/// itself once merged, but kept as a separate parameter as this function's
/// own half of the belt-and-braces [`is_password_field`]'s own doc comment
/// describes). A blank `password` field means "leave the stored value
/// unchanged" throughout this form (see `crate::components::fields`'s own
/// doc comment on why the rendered input never shows a field's actual
/// current value, and [`preserve_blank_passwords`], this module's own
/// inverse restoring it just before the outbound payload is built) —
/// seeding the form itself with the real secret would defeat that the
/// moment anyone inspected the running app's own signal state, even though
/// the rendered input never displays it.
#[must_use]
pub fn initial_form_values(fields: &[Field], schema_fields: &[Field]) -> HashMap<String, String> {
    let mut values = initial_values(fields);
    for field in fields {
        if is_password_field(field, schema_fields) {
            values.insert(field.name.clone(), String::new());
        }
    }
    values
}

/// Builds the outbound [`IndexerResource`] a create/update call sends: `id`
/// carried straight through (`0` for a create — skipped from the JSON
/// entirely by that field's own `skip_serializing_if`), `fields` rebuilt
/// from `schema_fields`'s own metadata paired with each field's current
/// string value in `values` (missing from `values` becomes `""`, the same
/// as a fresh, untouched field), converted back to a wire
/// [`serde_json::Value`] via `crate::components::fields::value_for_field`.
/// `description`/`language`/`capabilities` are always sent as their zero
/// values — the server never persists them per indexer regardless (see
/// `oxidarr_prowl::api::indexers`'s own "Resource ↔ row mapping" section),
/// so there is nothing honest to fill them with here. `sync_error` is
/// always `None` — see this module's own doc comment.
///
/// Eight parameters, one over clippy's default `too_many_arguments`
/// threshold: every one of them is a genuinely independent piece of the
/// outbound resource (not a group that wants its own struct), and `values`
/// is always this screen's own plain `HashMap<String, String>` form state —
/// never called with any other hasher — so `implicit_hasher`'s suggested
/// generalization would add a type parameter no caller here would ever use.
#[must_use]
#[allow(clippy::too_many_arguments, clippy::implicit_hasher)]
pub fn build_indexer_resource(
    id: i32,
    name: &str,
    definition_name: &str,
    implementation: &str,
    enable: bool,
    priority: i32,
    schema_fields: &[Field],
    values: &HashMap<String, String>,
) -> IndexerResource {
    let fields = schema_fields
        .iter()
        .map(|field| {
            let value = values.get(&field.name).cloned().unwrap_or_default();
            Field {
                name: field.name.clone(),
                label: field.label.clone(),
                kind: field.kind.clone(),
                value: Some(value_for_field(&field.kind, &value)),
                select_options: field.select_options.clone(),
            }
        })
        .collect();
    IndexerResource {
        id,
        name: name.to_string(),
        implementation: implementation.to_string(),
        definition_name: definition_name.to_string(),
        description: String::new(),
        language: String::new(),
        enable,
        priority,
        capabilities: IndexerCapabilities::default(),
        fields,
        sync_error: None,
    }
}

/// Builds the outbound `IndexerResource` for the list screen's own
/// enable-toggle: `resource` with `enable` flipped, routed through
/// [`build_indexer_resource`] rather than a bare `IndexerResource { enable:
/// ..., ..resource.clone() }` struct-update — the latter would carry
/// `resource`'s own `sync_error` straight through to the outbound PUT body,
/// violating that field's own "never sent by this client" contract (see
/// [`build_indexer_resource`]'s own doc comment) the moment a previous
/// write had reported one.
#[must_use]
pub fn toggle_enable(resource: &IndexerResource) -> IndexerResource {
    let values = initial_values(&resource.fields);
    build_indexer_resource(
        resource.id,
        &resource.name,
        &resource.definition_name,
        &resource.implementation,
        !resource.enable,
        resource.priority,
        &resource.fields,
        &values,
    )
}

/// Merges a chosen schema entry's own field *metadata*
/// (`label`/`kind`/`selectOptions`) with an existing indexer's own current
/// field *values* — the fix for the Edit form's own field-degradation bug:
/// `GET /api/v1/indexer` reconstructs `fields[]` straight from the
/// persisted settings map (`fields_from_settings`,
/// `crates/oxidarr-prowl/src/api/indexers.rs`), which has no access to the
/// original definition's own metadata at all — every field comes back
/// `label` equal to its raw setting name, `kind` only ever `"checkbox"` or
/// `"textbox"`, and no `selectOptions`. Rendering the Edit form straight
/// from that response degrades a `select` field to free text, shows a raw
/// name instead of a real label, and — the sharpest edge — renders a
/// `password` setting as a plain cleartext `type="text"` input, since
/// [`crate::components::fields::render_field`]'s `password` branch only
/// ever triggers on `kind == "password"`, which a reconstructed field never
/// carries.
///
/// Iterates `schema_fields` (this screen already has them, from `GET
/// /indexer/schema` — see this module's own doc comment) rather than
/// `stored_fields`, so the result always carries full, correct metadata for
/// every field the definition currently defines; each field's *value* comes
/// from the matching `stored_fields` entry (by name) when present, falling
/// back to the schema field's own default otherwise — the definition may
/// have gained a field since this indexer was last saved, in which case
/// `stored_fields` simply has no entry for it yet.
///
/// The reverse case — `stored_fields` carries a value for a setting
/// `schema_fields` no longer defines at all (a renamed or removed setting)
/// — is a decided, intentional drop, not an oversight: iterating
/// `schema_fields` alone means a name only present in `stored_fields` never
/// makes it into the merged result, and a resource rebuilt from that
/// result (`build_indexer_resource`, which itself only ever iterates its
/// own `schema_fields` parameter too) carries that drop through to the next
/// save. There is nothing honest to keep it *for* — a setting with no
/// current definition has no label, no kind, no select options to render,
/// and the server has no schema of its own to validate or interpret it
/// against either.
#[must_use]
pub fn merge_field_metadata(schema_fields: &[Field], stored_fields: &[Field]) -> Vec<Field> {
    schema_fields
        .iter()
        .map(|schema_field| {
            let value = stored_fields
                .iter()
                .find(|stored| stored.name == schema_field.name)
                .and_then(|stored| stored.value.clone())
                .or_else(|| schema_field.value.clone());
            Field {
                value,
                ..schema_field.clone()
            }
        })
        .collect()
}

/// Finds `definition_name`'s own entry in `schema_entries` — the gate
/// `Indexers`'s own `on_edit` handler must pass before it may open the
/// Edit form at all (see this module's own doc comment, "Edit form must
/// never open unmerged"). `None` covers two states this function
/// deliberately does not distinguish, because `on_edit` reacts to both the
/// same way: the schema fetch hasn't resolved yet (`schema_entries` is
/// itself `None` — the exact window a click on Edit can land in, since
/// this screen's `indexers` and `schema` are two independent
/// `use_resource`s and the indexer list can render well before the schema
/// does), or it resolved but carries no entry whose own `definitionName`
/// matches (the fetch failed, or this indexer's own definition is gone).
/// Pure and natively testable, unlike the `on_edit` closure itself.
#[must_use]
pub fn resolve_schema_entry_for_edit<'a>(
    schema_entries: Option<&'a [IndexerResource]>,
    definition_name: &str,
) -> Option<&'a IndexerResource> {
    schema_entries?
        .iter()
        .find(|entry| entry.definition_name == definition_name)
}

/// Restores a `password`-kind field's own current value from `schema_fields`
/// whenever `values` holds it blank — called just before building an
/// outbound create/update/test payload. See
/// `crate::components::fields`'s own doc comment (the `render_field`
/// `password` branch never displays a field's actual current value, so a
/// form left untouched, or typed into and cleared back to blank, both leave
/// that field blank in `values`); without this, saving would send that
/// blank straight through and overwrite the stored secret with an empty
/// string. `schema_fields` here is expected to be the same
/// metadata-plus-current-value shape [`merge_field_metadata`] produces (for
/// Edit) or a fresh schema entry's own fields (for Add, where there is
/// nothing stored yet to preserve — every field there is blank by
/// construction, so this is a no-op).
#[must_use]
#[allow(clippy::implicit_hasher)]
pub fn preserve_blank_passwords(
    schema_fields: &[Field],
    values: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut resolved = values.clone();
    for field in schema_fields {
        // [`is_password_field`] rather than a bare `field.kind ==
        // "password"` check — see that function's own doc comment. Here
        // `schema_fields` doubles as both the field being checked and its
        // own cross-check source (there is no second, independently
        // fetched list this function has to consult), so this is
        // consistency with [`initial_form_values`]'s own policy rather
        // than a second, distinct safeguard on its own.
        if !is_password_field(field, schema_fields) {
            continue;
        }
        let is_blank = resolved.get(&field.name).is_none_or(String::is_empty);
        if is_blank {
            let stored = field_value_as_string(field);
            if !stored.is_empty() {
                resolved.insert(field.name.clone(), stored);
            }
        }
    }
    resolved
}

/// Builds the outbound `IndexerResource` the list screen's own Test button
/// sends: `resource` as fetched, routed through [`build_indexer_resource`]
/// (rather than sent straight through) so a carried
/// [`IndexerResource::sync_error`] never rides back out on the wire — see
/// [`build_indexer_resource`]'s own doc comment and
/// [`toggle_enable`]'s identical reasoning for the enable-toggle's own
/// payload.
#[must_use]
pub fn build_resource_for_test(resource: &IndexerResource) -> IndexerResource {
    let values = initial_values(&resource.fields);
    build_indexer_resource(
        resource.id,
        &resource.name,
        &resource.definition_name,
        &resource.implementation,
        resource.enable,
        resource.priority,
        &resource.fields,
        &values,
    )
}

/// Case-insensitive substring filter over `entries`'s own `name` — the add
/// flow's definition picker searches by name.
#[must_use]
pub fn filter_schema_entries<'a>(
    entries: &'a [IndexerResource],
    query: &str,
) -> Vec<&'a IndexerResource> {
    let query = query.to_lowercase();
    entries
        .iter()
        .filter(|entry| entry.name.to_lowercase().contains(&query))
        .collect()
}

/// Renders `outcome` inline: nothing for `None` (no test has run yet, or
/// its result was cleared), a plain "succeeded" note for [`TestOutcome::Ok`],
/// the failure's own message for [`TestOutcome::Failed`]. Shared by
/// [`IndexerListView`]'s own per-row test button and [`IndexerFormView`]'s
/// own form-level one.
#[component]
pub fn TestOutcomeView(outcome: Option<TestOutcome>) -> Element {
    match outcome {
        None => rsx! {},
        Some(TestOutcome::Ok) => rsx! {
            span { class: "test-result test-result-ok", "Test succeeded" }
        },
        Some(TestOutcome::Failed(message)) => rsx! {
            span { class: "test-result test-result-failed", "{message}" }
        },
    }
}

/// The status class one list row carries, in the order a reader cares
/// about: a row whose last sync failed is `is-failed` whether or not it is
/// enabled (the failure is the thing worth seeing), a disabled row with no
/// failure is `is-off`, and everything else is `is-live`. The stylesheet
/// turns this into the row's left-edge colour; the ordering here is the
/// whole reason that edge is worth scanning.
#[must_use]
pub fn row_state_class(enable: bool, sync_error: Option<&str>) -> &'static str {
    if sync_error.is_some() {
        "is-failed"
    } else if enable {
        "is-live"
    } else {
        "is-off"
    }
}

/// The list screen's presentational half — see this module's own doc
/// comment. Every callback fires the affected row's own `id`, except
/// `on_add` (no row) and `on_delete_cancel` (no id needed to clear the
/// pending confirmation).
#[component]
pub fn IndexerListView(
    indexers: Vec<IndexerResource>,
    confirm_delete_id: Option<i32>,
    test_results: HashMap<i32, TestOutcome>,
    on_add: EventHandler<()>,
    on_edit: EventHandler<i32>,
    on_toggle_enable: EventHandler<i32>,
    on_delete_request: EventHandler<i32>,
    on_delete_confirm: EventHandler<i32>,
    on_delete_cancel: EventHandler<()>,
    on_test: EventHandler<i32>,
) -> Element {
    rsx! {
        div { id: "indexers-screen",
            div { class: "screen-head",
                h1 { "Indexers" }
                button {
                    r#type: "button",
                    class: "primary",
                    onclick: move |_| on_add.call(()),
                    "Add indexer"
                }
            }
            table {
                thead {
                    tr {
                        th { "Name" }
                        th { "Implementation" }
                        th { "Priority" }
                        th { "Enabled" }
                        th { "Test" }
                        th { "Actions" }
                    }
                }
                tbody {
                    for indexer in indexers {
                        {
                            let id = indexer.id;
                            let outcome = test_results.get(&id).cloned();
                            let confirming = confirm_delete_id == Some(id);
                            let state = row_state_class(
                                indexer.enable,
                                indexer.sync_error.as_deref(),
                            );
                            rsx! {
                                tr { key: "{id}", class: "{state}",
                                    td {
                                        "{indexer.name}"
                                        if let Some(sync_error) = &indexer.sync_error {
                                            " "
                                            span { class: "sync-error", "{sync_error}" }
                                        }
                                    }
                                    td { "{indexer.implementation}" }
                                    td { "{indexer.priority}" }
                                    td {
                                        input {
                                            r#type: "checkbox",
                                            checked: indexer.enable,
                                            onchange: move |_| on_toggle_enable.call(id),
                                        }
                                    }
                                    td {
                                        button {
                                            r#type: "button",
                                            onclick: move |_| on_test.call(id),
                                            "Test"
                                        }
                                        TestOutcomeView { outcome }
                                    }
                                    td {
                                        button {
                                            r#type: "button",
                                            onclick: move |_| on_edit.call(id),
                                            "Edit"
                                        }
                                        if confirming {
                                            button {
                                                r#type: "button",
                                                class: "danger",
                                                onclick: move |_| on_delete_confirm.call(id),
                                                "Confirm delete"
                                            }
                                            button {
                                                r#type: "button",
                                                onclick: move |_| on_delete_cancel.call(()),
                                                "Cancel"
                                            }
                                        } else {
                                            button {
                                                r#type: "button",
                                                class: "quiet",
                                                onclick: move |_| on_delete_request.call(id),
                                                "Delete"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The add flow's definition picker — shown while `ScreenState::Add`'s own
/// `schema` is still `None`. `entries` is the full `GET /indexer/schema`
/// list; filtering by `query` (`filter_schema_entries`) happens here rather
/// than in the routed container, so this view stays a pure function of its
/// own props for snapshotting. Schema entries all share `id == 0` (no
/// database row yet — see `crate::dto::IndexerResource::id`'s own doc
/// comment), so `on_pick` fires the chosen entry's own `definitionName`
/// instead, which is unique per definition.
#[component]
pub fn DefinitionPickerView(
    entries: Vec<IndexerResource>,
    query: String,
    on_query_change: EventHandler<String>,
    on_pick: EventHandler<String>,
    on_cancel: EventHandler<()>,
) -> Element {
    let matches: Vec<IndexerResource> = filter_schema_entries(&entries, &query)
        .into_iter()
        .cloned()
        .collect();
    rsx! {
        div { id: "definition-picker",
            h1 { "Add indexer" }
            input {
                r#type: "text",
                placeholder: "Search definitions",
                value: "{query}",
                oninput: move |event| on_query_change.call(event.value()),
            }
            ul {
                for entry in matches {
                    {
                        let definition_name = entry.definition_name.clone();
                        rsx! {
                            li { key: "{entry.definition_name}",
                                span { "{entry.name} ({entry.implementation})" }
                                button {
                                    r#type: "button",
                                    onclick: move |_| on_pick.call(definition_name.clone()),
                                    "Select"
                                }
                            }
                        }
                    }
                }
            }
            button { r#type: "button", onclick: move |_| on_cancel.call(()), "Cancel" }
        }
    }
}

/// The settings form shared by the add flow (once a schema entry is
/// chosen) and the edit flow. `fields` carries each field's own metadata
/// (`label`/`kind`/`selectOptions`); `values` is the form's own current
/// per-field string state (`crate::components::fields::render_field`'s own
/// convention) — see this module's own doc comment for why those two are
/// kept separate.
#[component]
pub fn IndexerFormView(
    mode_label: String,
    definition_name: String,
    name: String,
    priority: i32,
    enable: bool,
    fields: Vec<Field>,
    values: HashMap<String, String>,
    test_result: Option<TestOutcome>,
    on_name_change: EventHandler<String>,
    on_priority_change: EventHandler<String>,
    on_enable_change: EventHandler<bool>,
    on_field_change: EventHandler<(String, String)>,
    on_test: EventHandler<()>,
    on_save: EventHandler<()>,
    on_cancel: EventHandler<()>,
) -> Element {
    rsx! {
        div { id: "indexer-form",
            h1 { "{mode_label} indexer" }
            p { "Definition: {definition_name}" }
            form {
                onsubmit: move |event| {
                    event.prevent_default();
                    on_save.call(());
                },
                label {
                    "Name"
                    input {
                        r#type: "text",
                        value: "{name}",
                        oninput: move |event| on_name_change.call(event.value()),
                    }
                }
                label {
                    "Priority"
                    input {
                        r#type: "number",
                        value: "{priority}",
                        oninput: move |event| on_priority_change.call(event.value()),
                    }
                }
                label {
                    input {
                        r#type: "checkbox",
                        checked: enable,
                        onchange: move |event| on_enable_change.call(event.checked()),
                    }
                    "Enabled"
                }
                for field in &fields {
                    {
                        let value = values.get(&field.name).cloned().unwrap_or_default();
                        rsx! {
                            label { key: "{field.name}",
                                "{field.label}"
                                {render_field(field, &value, on_field_change)}
                            }
                        }
                    }
                }
                TestOutcomeView { outcome: test_result }
                button { r#type: "button", onclick: move |_| on_test.call(()), "Test" }
                button { r#type: "submit", class: "primary", "Save" }
                button { r#type: "button", onclick: move |_| on_cancel.call(()), "Cancel" }
            }
        }
    }
}

/// The routed, data-fetching container — see this module's own doc
/// comment. Not exercised by any native test (same reason as
/// `crate::screens::status::Status`'s own non-test half): every `*View`
/// component above carries this screen's real test coverage.
#[component]
pub fn Indexers() -> Element {
    let api = use_context::<SharedApi>();
    let mut session = use_context::<Session>();

    let mut state = use_signal(|| ScreenState::List);
    let mut confirm_delete_id = use_signal(|| None::<i32>);
    let mut list_test_results = use_signal(HashMap::<i32, TestOutcome>::new);
    let mut form_test_result = use_signal(|| None::<TestOutcome>);
    let mut search_query = use_signal(String::new);
    let mut draft_name = use_signal(String::new);
    let mut draft_priority = use_signal(|| 25_i32);
    let mut draft_enable = use_signal(|| false);
    let mut draft_values = use_signal(HashMap::<String, String>::new);

    // `api.read().clone().indexers().await` in one statement would keep the
    // `Signal` read guard's temporary alive across the `.await` (the
    // chained `.clone()` doesn't end its lifetime — that only happens at
    // the statement's end); cloning in the synchronous outer closure first,
    // before the `async move` block even exists, avoids that entirely — see
    // this module's own doc comment.
    let mut indexers = use_resource(move || {
        let client = api.read().clone();
        async move { client.indexers().await }
    });
    let schema = use_resource(move || {
        let client = api.read().clone();
        async move { client.indexer_schema().await }
    });

    let list = match &*indexers.read() {
        None => return rsx! { p { "Loading…" } },
        Some(Ok(list)) => list.clone(),
        // `handle_error` writes to `session`'s own `Signal`s during this
        // component's render body — see
        // `crate::screens::status::Status`'s own comment on this exact
        // pattern for why that's safe only because `Indexers` takes no
        // props (Dioxus's own no-props memoization). If `Indexers` ever
        // gains a prop, move this into a `use_effect` first — this site and
        // the sibling one below, in `ScreenState::Add`'s own `schema`
        // match, share the identical invariant.
        Some(Err(err)) => {
            handle_error(err.clone(), &mut session);
            return rsx! {};
        }
    };

    match &*state.read() {
        ScreenState::List => rsx! {
            IndexerListView {
                indexers: list,
                confirm_delete_id: *confirm_delete_id.read(),
                test_results: list_test_results.read().clone(),
                on_add: move |()| {
                    state.set(ScreenState::Add { schema: None });
                    search_query.set(String::new());
                },
                on_edit: move |id: i32| {
                    let found = indexers
                        .read()
                        .as_ref()
                        .and_then(|result| result.as_ref().ok())
                        .and_then(|list| list.iter().find(|item| item.id == id).cloned());
                    let Some(mut resource) = found else { return };
                    // `resource.fields` as fetched carries only
                    // reconstructed metadata (see `merge_field_metadata`'s
                    // own doc comment) — the Edit form must never open
                    // without first merging the matching schema entry's
                    // own real metadata onto it (this module's own doc
                    // comment, "Edit form must never open unmerged": a
                    // password setting's reconstructed `kind` reads as
                    // plain `"textbox"`, which would both display the
                    // stored secret in the draft and let a blank save wipe
                    // it — not a "graceful" degradation the way a missing
                    // label or select options would be). `schema` and
                    // `indexers` are two independent `use_resource`s, so a
                    // click on Edit can land in the window where the list
                    // has already resolved but the schema hasn't;
                    // `resolve_schema_entry_for_edit` returns `None` for
                    // that case and for "resolved with no matching entry"
                    // alike, and either way this refuses to open Edit,
                    // explaining why in the banner instead of silently
                    // degrading.
                    let schema_entries = schema
                        .read()
                        .as_ref()
                        .and_then(|result| result.as_ref().ok())
                        .cloned();
                    let schema_entry =
                        resolve_schema_entry_for_edit(schema_entries.as_deref(), &resource.definition_name)
                            .cloned();
                    let Some(schema_entry) = schema_entry else {
                        session.error.set(Some(
                            "Still loading this indexer's field definitions — try Edit again in a moment."
                                .to_string(),
                        ));
                        return;
                    };
                    resource.fields = merge_field_metadata(&schema_entry.fields, &resource.fields);
                    draft_name.set(resource.name.clone());
                    draft_priority.set(resource.priority);
                    draft_enable.set(resource.enable);
                    draft_values.set(initial_form_values(&resource.fields, &schema_entry.fields));
                    form_test_result.set(None);
                    state.set(ScreenState::Edit { resource });
                },
                on_toggle_enable: move |id: i32| {
                    let found = indexers
                        .read()
                        .as_ref()
                        .and_then(|result| result.as_ref().ok())
                        .and_then(|list| list.iter().find(|item| item.id == id).cloned());
                    let Some(resource) = found else { return };
                    let updated = toggle_enable(&resource);
                    let client = api.read().clone();
                    spawn(async move {
                        let mut session = session;
                        match client.update_indexer(&updated).await {
                            Ok(_) => indexers.restart(),
                            Err(err) => handle_error(err, &mut session),
                        }
                    });
                },
                on_delete_request: move |id: i32| confirm_delete_id.set(Some(id)),
                on_delete_confirm: move |id: i32| {
                    let client = api.read().clone();
                    spawn(async move {
                        let mut session = session;
                        match client.delete_indexer(id).await {
                            Ok(()) => {
                                confirm_delete_id.set(None);
                                indexers.restart();
                            }
                            Err(err) => handle_error(err, &mut session),
                        }
                    });
                },
                on_delete_cancel: move |()| confirm_delete_id.set(None),
                on_test: move |id: i32| {
                    let found = indexers
                        .read()
                        .as_ref()
                        .and_then(|result| result.as_ref().ok())
                        .and_then(|list| list.iter().find(|item| item.id == id).cloned());
                    let Some(resource) = found else { return };
                    // `resource` as fetched may carry a `sync_error` from a
                    // previous write — route it through the same builder
                    // `toggle_enable` uses so this never rides back out on
                    // the outbound test payload (see
                    // `build_resource_for_test`'s own doc comment).
                    let built = build_resource_for_test(&resource);
                    let client = api.read().clone();
                    spawn(async move {
                        let outcome = match client.test_indexer(&built).await {
                            Ok(()) => TestOutcome::Ok,
                            Err(err) => TestOutcome::Failed(test_failure_message(err)),
                        };
                        list_test_results.write().insert(id, outcome);
                    });
                },
            }
        },
        ScreenState::Add { schema: None } => {
            let entries = match &*schema.read() {
                None => return rsx! { p { "Loading…" } },
                Some(Ok(entries)) => entries.clone(),
                // Same invariant as this component's own `indexers`
                // match above — see that site's own comment.
                Some(Err(err)) => {
                    handle_error(err.clone(), &mut session);
                    return rsx! {};
                }
            };
            rsx! {
                DefinitionPickerView {
                    entries,
                    query: search_query.read().clone(),
                    on_query_change: move |value| search_query.set(value),
                    on_pick: move |definition_name: String| {
                        let found = schema
                            .read()
                            .as_ref()
                            .and_then(|result| result.as_ref().ok())
                            .and_then(|entries| {
                                entries.iter().find(|entry| entry.definition_name == definition_name).cloned()
                            });
                        if let Some(entry) = found {
                            draft_name.set(entry.name.clone());
                            draft_priority.set(entry.priority);
                            draft_enable.set(entry.enable);
                            draft_values.set(initial_form_values(&entry.fields, &entry.fields));
                            form_test_result.set(None);
                            state.set(ScreenState::Add { schema: Some(entry) });
                        }
                    },
                    on_cancel: move |()| state.set(ScreenState::List),
                }
            }
        }
        ScreenState::Add {
            schema: Some(entry),
        } => {
            let entry = entry.clone();
            rsx! {
                IndexerFormView {
                    mode_label: "Add",
                    definition_name: entry.definition_name.clone(),
                    name: draft_name.read().clone(),
                    priority: *draft_priority.read(),
                    enable: *draft_enable.read(),
                    fields: entry.fields.clone(),
                    values: draft_values.read().clone(),
                    test_result: form_test_result.read().clone(),
                    on_name_change: move |value| draft_name.set(value),
                    on_priority_change: move |value: String| {
                        if let Ok(priority) = value.parse() {
                            draft_priority.set(priority);
                        }
                    },
                    on_enable_change: move |value| draft_enable.set(value),
                    on_field_change: move |(name, value): (String, String)| {
                        draft_values.write().insert(name, value);
                    },
                    on_test: {
                        let entry = entry.clone();
                        move |()| {
                            let values = preserve_blank_passwords(&entry.fields, &draft_values.read());
                            let resource = build_indexer_resource(
                                0,
                                &draft_name.read(),
                                &entry.definition_name,
                                &entry.implementation,
                                *draft_enable.read(),
                                *draft_priority.read(),
                                &entry.fields,
                                &values,
                            );
                            let client = api.read().clone();
                            spawn(async move {
                                let outcome = match client.test_indexer(&resource).await {
                                    Ok(()) => TestOutcome::Ok,
                                    Err(err) => TestOutcome::Failed(test_failure_message(err)),
                                };
                                form_test_result.set(Some(outcome));
                            });
                        }
                    },
                    on_save: {
                        let entry = entry.clone();
                        move |()| {
                            let values = preserve_blank_passwords(&entry.fields, &draft_values.read());
                            let resource = build_indexer_resource(
                                0,
                                &draft_name.read(),
                                &entry.definition_name,
                                &entry.implementation,
                                *draft_enable.read(),
                                *draft_priority.read(),
                                &entry.fields,
                                &values,
                            );
                            let client = api.read().clone();
                            spawn(async move {
                                let mut session = session;
                                match client.create_indexer(&resource).await {
                                    Ok(_) => {
                                        indexers.restart();
                                        state.set(ScreenState::List);
                                    }
                                    Err(err) => handle_error(err, &mut session),
                                }
                            });
                        }
                    },
                    on_cancel: move |()| state.set(ScreenState::List),
                }
            }
        }
        ScreenState::Edit { resource } => {
            let resource = resource.clone();
            rsx! {
                IndexerFormView {
                    mode_label: "Edit",
                    definition_name: resource.definition_name.clone(),
                    name: draft_name.read().clone(),
                    priority: *draft_priority.read(),
                    enable: *draft_enable.read(),
                    fields: resource.fields.clone(),
                    values: draft_values.read().clone(),
                    test_result: form_test_result.read().clone(),
                    on_name_change: move |value| draft_name.set(value),
                    on_priority_change: move |value: String| {
                        if let Ok(priority) = value.parse() {
                            draft_priority.set(priority);
                        }
                    },
                    on_enable_change: move |value| draft_enable.set(value),
                    on_field_change: move |(name, value): (String, String)| {
                        draft_values.write().insert(name, value);
                    },
                    on_test: {
                        let resource = resource.clone();
                        move |()| {
                            let values = preserve_blank_passwords(&resource.fields, &draft_values.read());
                            let built = build_indexer_resource(
                                resource.id,
                                &draft_name.read(),
                                &resource.definition_name,
                                &resource.implementation,
                                *draft_enable.read(),
                                *draft_priority.read(),
                                &resource.fields,
                                &values,
                            );
                            let client = api.read().clone();
                            spawn(async move {
                                let outcome = match client.test_indexer(&built).await {
                                    Ok(()) => TestOutcome::Ok,
                                    Err(err) => TestOutcome::Failed(test_failure_message(err)),
                                };
                                form_test_result.set(Some(outcome));
                            });
                        }
                    },
                    on_save: {
                        let resource = resource.clone();
                        move |()| {
                            let values = preserve_blank_passwords(&resource.fields, &draft_values.read());
                            let built = build_indexer_resource(
                                resource.id,
                                &draft_name.read(),
                                &resource.definition_name,
                                &resource.implementation,
                                *draft_enable.read(),
                                *draft_priority.read(),
                                &resource.fields,
                                &values,
                            );
                            let client = api.read().clone();
                            spawn(async move {
                                let mut session = session;
                                match client.update_indexer(&built).await {
                                    Ok(_) => {
                                        indexers.restart();
                                        state.set(ScreenState::List);
                                    }
                                    Err(err) => handle_error(err, &mut session),
                                }
                            });
                        }
                    },
                    on_cancel: move |()| state.set(ScreenState::List),
                }
            }
        }
    }
}

/// Renders a [`crate::api::UiError`] as one failed test's own inline
/// message — reuses `crate::app::classify_error`'s exact wording for the
/// non-auth cases so a failed test reads the same text a banner would; a
/// `401` (which `classify_error` maps to reopening the key prompt, not a
/// banner message) falls back to a plain, honest description here instead,
/// since a test result has nowhere to reopen a key prompt from.
fn test_failure_message(err: crate::api::UiError) -> String {
    use crate::api::UiError;
    match err {
        UiError::Unauthorized => "Unauthorized — check the configured API key".to_string(),
        UiError::Problem(message) => message,
        UiError::Network(message) => format!("Network error: {message}"),
        UiError::Decode(message) => format!("Unexpected response: {message}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::SelectOption;

    fn field(name: &str, kind: &str) -> Field {
        Field {
            name: name.to_string(),
            label: name.to_string(),
            kind: kind.to_string(),
            value: None,
            select_options: None,
        }
    }

    fn schema_entry(name: &str) -> IndexerResource {
        IndexerResource {
            id: 0,
            name: name.to_string(),
            implementation: "Cardigann".to_string(),
            definition_name: name.to_lowercase(),
            description: String::new(),
            language: String::new(),
            enable: false,
            priority: 25,
            capabilities: IndexerCapabilities::default(),
            fields: Vec::new(),
            sync_error: None,
        }
    }

    #[test]
    fn initial_values_extracts_one_entry_per_field_by_name() {
        let fields = vec![
            Field {
                value: Some(serde_json::Value::String("abc".to_string())),
                ..field("cookie", "textbox")
            },
            Field {
                value: Some(serde_json::Value::Bool(true)),
                ..field("freeleech", "checkbox")
            },
        ];
        let values = initial_values(&fields);
        assert_eq!(values.get("cookie"), Some(&"abc".to_string()));
        assert_eq!(values.get("freeleech"), Some(&"true".to_string()));
    }

    #[test]
    fn build_indexer_resource_maps_form_values_onto_fields_by_name() {
        let schema_fields = vec![field("cookie", "textbox"), field("freeleech", "checkbox")];
        let mut values = HashMap::new();
        values.insert("cookie".to_string(), "abc".to_string());
        values.insert("freeleech".to_string(), "true".to_string());

        let resource = build_indexer_resource(
            0,
            "My Indexer",
            "example",
            "Cardigann",
            true,
            25,
            &schema_fields,
            &values,
        );

        assert_eq!(resource.id, 0);
        assert_eq!(resource.name, "My Indexer");
        assert_eq!(resource.definition_name, "example");
        assert_eq!(resource.implementation, "Cardigann");
        assert!(resource.enable);
        assert_eq!(resource.priority, 25);
        assert_eq!(resource.fields.len(), 2);
        assert_eq!(
            resource.fields[0].value,
            Some(serde_json::Value::String("abc".to_string()))
        );
        assert_eq!(
            resource.fields[1].value,
            Some(serde_json::Value::Bool(true))
        );
    }

    #[test]
    fn toggle_enable_flips_the_enable_flag() {
        let resource = IndexerResource {
            enable: true,
            ..schema_entry("Example")
        };
        assert!(!toggle_enable(&resource).enable);
    }

    #[test]
    fn toggling_enable_strips_a_carried_sync_error() {
        // A resource whose last write reported a sync failure — that
        // failure must never round-trip back out on the very next write
        // (`crate::dto::IndexerResource::sync_error`'s own doc comment:
        // "Never sent by this client; only ever read back").
        let resource = IndexerResource {
            sync_error: Some("sync failed: connection refused".to_string()),
            ..schema_entry("Example")
        };
        assert_eq!(toggle_enable(&resource).sync_error, None);
    }

    #[test]
    fn build_indexer_resource_defaults_a_missing_field_value_to_blank() {
        let schema_fields = vec![field("cookie", "textbox")];
        let resource = build_indexer_resource(
            0,
            "Name",
            "example",
            "Cardigann",
            true,
            25,
            &schema_fields,
            &HashMap::new(),
        );
        assert_eq!(
            resource.fields[0].value,
            Some(serde_json::Value::String(String::new()))
        );
    }

    #[test]
    fn build_indexer_resource_carries_select_options_through_unchanged() {
        let options = vec![SelectOption {
            value: 0,
            name: "seeders".to_string(),
        }];
        let schema_fields = vec![Field {
            select_options: Some(options.clone()),
            ..field("sort", "select")
        }];
        let resource = build_indexer_resource(
            0,
            "Name",
            "example",
            "Cardigann",
            true,
            25,
            &schema_fields,
            &HashMap::new(),
        );
        assert_eq!(resource.fields[0].select_options, Some(options));
    }

    #[test]
    fn filter_schema_entries_matches_case_insensitively_by_name() {
        let entries = vec![schema_entry("1337x"), schema_entry("The Pirate Bay")];
        let matches = filter_schema_entries(&entries, "pirate");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "The Pirate Bay");
    }

    #[test]
    fn filter_schema_entries_with_a_blank_query_matches_everything() {
        let entries = vec![schema_entry("1337x"), schema_entry("The Pirate Bay")];
        assert_eq!(filter_schema_entries(&entries, "").len(), 2);
    }

    #[test]
    fn filter_schema_entries_with_no_match_returns_empty() {
        let entries = vec![schema_entry("1337x")];
        assert!(filter_schema_entries(&entries, "nonexistent").is_empty());
    }

    #[test]
    fn test_failure_message_renders_a_problem_verbatim() {
        assert_eq!(
            test_failure_message(crate::api::UiError::Problem("bad request".to_string())),
            "bad request"
        );
    }

    #[test]
    fn test_failure_message_prefixes_a_network_error() {
        assert_eq!(
            test_failure_message(crate::api::UiError::Network("refused".to_string())),
            "Network error: refused"
        );
    }

    // --- merge_field_metadata: recovering schema metadata for Edit ---
    //
    // `GET /api/v1/indexer` reconstructs `fields[]` from the persisted
    // settings map alone (`fields_from_settings`,
    // crates/oxidarr-prowl/src/api/indexers.rs): every field comes back
    // `label` equal to its raw name, `kind` only ever `"checkbox"` or
    // `"textbox"`, and no `selectOptions` at all — a `select` setting
    // degrades to free text and a `password` setting renders as plain
    // cleartext. These prove `merge_field_metadata` recovers the real
    // metadata from the matching schema entry while keeping the *current*
    // value from what's actually stored.

    #[test]
    fn merge_field_metadata_takes_label_kind_and_select_options_from_schema() {
        let schema_fields = vec![Field {
            label: "Sort by".to_string(),
            select_options: Some(vec![
                SelectOption {
                    value: 0,
                    name: "seeders".to_string(),
                },
                SelectOption {
                    value: 1,
                    name: "created".to_string(),
                },
            ]),
            value: Some(serde_json::Value::String("seeders".to_string())),
            ..field("sort", "select")
        }];
        // What `GET /api/v1/indexer` actually sends back for this same
        // field: reconstructed, generic metadata, but the real stored
        // value.
        let stored_fields = vec![Field {
            value: Some(serde_json::Value::String("created".to_string())),
            ..field("sort", "textbox")
        }];

        let merged = merge_field_metadata(&schema_fields, &stored_fields);

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].label, "Sort by");
        assert_eq!(merged[0].kind, "select");
        assert!(merged[0].select_options.is_some());
        assert_eq!(
            merged[0].value,
            Some(serde_json::Value::String("created".to_string())),
            "the current stored value must survive the merge, not the schema's own default"
        );
    }

    #[test]
    fn merge_field_metadata_recovers_a_passwords_real_kind() {
        let schema_fields = vec![Field {
            label: "Passkey".to_string(),
            ..field("passkey", "password")
        }];
        let stored_fields = vec![Field {
            value: Some(serde_json::Value::String("secretkey".to_string())),
            ..field("passkey", "textbox")
        }];

        let merged = merge_field_metadata(&schema_fields, &stored_fields);

        assert_eq!(merged[0].kind, "password");
        assert_eq!(merged[0].label, "Passkey");
        assert_eq!(
            merged[0].value,
            Some(serde_json::Value::String("secretkey".to_string()))
        );
    }

    #[test]
    fn merge_field_metadata_falls_back_to_the_schemas_own_default_when_nothing_is_stored_yet() {
        // A field the definition gained after this indexer was last saved:
        // present in `schema_fields`, absent from `stored_fields` entirely.
        let schema_fields = vec![Field {
            value: Some(serde_json::Value::Bool(true)),
            ..field("freeleech", "checkbox")
        }];
        let merged = merge_field_metadata(&schema_fields, &[]);
        assert_eq!(merged[0].value, Some(serde_json::Value::Bool(true)));
    }

    #[test]
    fn merge_field_metadata_drops_a_stored_field_the_schema_no_longer_defines() {
        // Decided and pinned: merge_field_metadata is schema-driven by
        // design (see its own doc comment — it iterates `schema_fields`,
        // never `stored_fields`), so a setting the current definition no
        // longer defines at all (renamed, or the definition dropped a
        // field) is intentionally dropped from the merged result, not
        // carried through as an orphaned extra. A save built from that
        // merged result therefore also drops it server-side — accepted as
        // correct, not a bug: keeping a value around for a setting the
        // current schema doesn't even ask for has nothing honest to
        // display (no label, no kind, no select options), and the server
        // itself would just store it back verbatim with no definition to
        // validate or interpret it against.
        let schema_fields = vec![field("cookie", "textbox")];
        let stored_fields = vec![
            Field {
                value: Some(serde_json::Value::String("abc".to_string())),
                ..field("cookie", "textbox")
            },
            Field {
                value: Some(serde_json::Value::String("orphaned-value".to_string())),
                ..field("legacy_cookie", "textbox")
            },
        ];

        let merged = merge_field_metadata(&schema_fields, &stored_fields);

        assert_eq!(merged.len(), 1);
        assert!(merged.iter().all(|f| f.name != "legacy_cookie"));
    }

    // --- resolve_schema_entry_for_edit: the Edit form must never open
    // unmerged (critical fix) ---
    //
    // `Indexers`'s own `on_edit` handler used to merge schema metadata onto
    // a resource's own fields only *if* a matching schema entry happened
    // to already be available, opening the Edit form regardless either
    // way — a "graceful no-op" for label/select degradation, but not for a
    // password setting: reconstructed fields (`GET /api/v1/indexer`'s own
    // shape) carry `kind: "textbox"` even for what the definition itself
    // defines as `password`, so `initial_form_values`'s own kind check
    // would seed the real stored secret into the draft, and
    // `preserve_blank_passwords` would no longer protect it — a user who
    // then cleared and saved that visible secret would wipe it server-side.
    // `resolve_schema_entry_for_edit` is the gate `on_edit` now checks
    // first: `None` (schema still loading, in the window between this
    // screen's own two independent `use_resource` fetches — `indexers` and
    // `schema` — or resolved with no matching entry) means "refuse to open
    // Edit, surface a banner instead", never "open it anyway".

    fn indexer_schema_entry(definition_name: &str, field_kind: &str) -> IndexerResource {
        IndexerResource {
            definition_name: definition_name.to_string(),
            fields: vec![field("passkey", field_kind)],
            ..schema_entry("Example")
        }
    }

    #[test]
    fn resolve_schema_entry_for_edit_is_none_while_the_schema_fetch_is_still_loading() {
        // The exact defect window: `schema.read()` hasn't resolved at all
        // yet (`None`), even though the indexer list (fetched
        // independently) already has.
        assert!(resolve_schema_entry_for_edit(None, "example").is_none());
    }

    #[test]
    fn resolve_schema_entry_for_edit_is_none_with_no_matching_definition() {
        let entries = vec![indexer_schema_entry("other", "password")];
        assert!(resolve_schema_entry_for_edit(Some(&entries), "example").is_none());
    }

    #[test]
    fn resolve_schema_entry_for_edit_finds_the_matching_entry() {
        let entries = vec![
            indexer_schema_entry("other", "password"),
            indexer_schema_entry("example", "password"),
        ];
        let found = resolve_schema_entry_for_edit(Some(&entries), "example");
        assert_eq!(
            found.map(|entry| &entry.definition_name),
            Some(&"example".to_string())
        );
    }

    // --- password fields: no prefill, blank-on-save preserves the stored value ---

    #[test]
    fn initial_form_values_blanks_out_a_stored_passwords_value() {
        let fields = vec![Field {
            value: Some(serde_json::Value::String("secretkey".to_string())),
            ..field("passkey", "password")
        }];
        let values = initial_form_values(&fields, &fields);
        assert_eq!(values.get("passkey"), Some(&String::new()));
    }

    #[test]
    fn initial_form_values_blanks_a_field_the_schema_says_is_password_even_if_unmerged() {
        // Reproduces the critical defect directly: `unmerged_fields` is
        // exactly the shape a reconstructed, never-merged field would have
        // — `kind: "textbox"` — for a setting the schema itself defines as
        // `password`. Before this cross-check existed, `initial_form_values`
        // trusted `field.kind` alone and would have seeded the real
        // "secretkey" into the draft here; cross-referencing `schema_fields`
        // by name (see `is_password_field`'s own doc comment) still catches
        // it even though `unmerged_fields[0].kind` itself is wrong.
        let unmerged_fields = vec![Field {
            value: Some(serde_json::Value::String("secretkey".to_string())),
            ..field("passkey", "textbox")
        }];
        let schema_fields = vec![field("passkey", "password")];

        let values = initial_form_values(&unmerged_fields, &schema_fields);

        assert_eq!(values.get("passkey"), Some(&String::new()));
    }

    #[test]
    fn initial_values_keeps_a_stored_passwords_real_value_for_direct_resubmission() {
        // Unlike `initial_form_values`, this is the raw extraction
        // `toggle_enable`/`build_resource_for_test` rely on — neither goes
        // through a form, so there is nothing to blank.
        let fields = vec![Field {
            value: Some(serde_json::Value::String("secretkey".to_string())),
            ..field("passkey", "password")
        }];
        let values = initial_values(&fields);
        assert_eq!(values.get("passkey"), Some(&"secretkey".to_string()));
    }

    #[test]
    fn preserve_blank_passwords_restores_the_stored_value_when_left_blank() {
        let schema_fields = vec![Field {
            value: Some(serde_json::Value::String("secretkey".to_string())),
            ..field("passkey", "password")
        }];
        let mut values = HashMap::new();
        values.insert("passkey".to_string(), String::new());

        let resolved = preserve_blank_passwords(&schema_fields, &values);

        assert_eq!(resolved.get("passkey"), Some(&"secretkey".to_string()));
    }

    #[test]
    fn preserve_blank_passwords_keeps_a_freshly_typed_replacement() {
        let schema_fields = vec![Field {
            value: Some(serde_json::Value::String("secretkey".to_string())),
            ..field("passkey", "password")
        }];
        let mut values = HashMap::new();
        values.insert("passkey".to_string(), "newkey".to_string());

        let resolved = preserve_blank_passwords(&schema_fields, &values);

        assert_eq!(resolved.get("passkey"), Some(&"newkey".to_string()));
    }

    #[test]
    fn preserve_blank_passwords_leaves_non_password_fields_untouched() {
        let schema_fields = vec![field("cookie", "textbox")];
        let values = HashMap::new();
        let resolved = preserve_blank_passwords(&schema_fields, &values);
        assert_eq!(resolved.get("cookie"), None);
    }

    // --- build_resource_for_test: the list Test button must never leak a
    // carried sync_error onto the wire (see this module's own doc comment,
    // "Outbound sync_error") ---

    #[test]
    fn build_resource_for_test_strips_a_carried_sync_error() {
        let resource = IndexerResource {
            sync_error: Some("sync failed: connection refused".to_string()),
            ..schema_entry("Example")
        };
        assert_eq!(build_resource_for_test(&resource).sync_error, None);
    }

    #[test]
    fn build_resource_for_test_keeps_the_resources_own_enable_and_priority() {
        let resource = IndexerResource {
            enable: true,
            priority: 42,
            ..schema_entry("Example")
        };
        let built = build_resource_for_test(&resource);
        assert!(built.enable);
        assert_eq!(built.priority, 42);
    }

    // --- row status class (the stylesheet's left-edge "status spine") ---

    #[test]
    fn an_enabled_indexer_with_no_sync_error_is_live() {
        assert_eq!(row_state_class(true, None), "is-live");
    }

    #[test]
    fn a_disabled_indexer_with_no_sync_error_is_off() {
        assert_eq!(row_state_class(false, None), "is-off");
    }

    #[test]
    fn a_sync_error_reads_as_failed_even_while_enabled() {
        assert_eq!(row_state_class(true, Some("boom")), "is-failed");
    }

    #[test]
    fn a_sync_error_outranks_being_disabled() {
        // The failure is the thing worth seeing: a disabled row that also
        // failed still shows oxide, not dormant steel.
        assert_eq!(row_state_class(false, Some("boom")), "is-failed");
    }
}
