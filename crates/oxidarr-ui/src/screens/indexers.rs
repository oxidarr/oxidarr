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
//! [`ScreenState`] is kept to three variants, per this task's own brief:
//! `List`, `Add { schema }`, `Edit { resource }`. The add flow's own
//! `schema` starts `None` (the definition picker shows) and becomes
//! `Some(entry)` in place, without ever leaving the `Add` variant, once a
//! definition is picked — no separate "picking" state is needed.
//!
//! # Field metadata vs. current value
//!
//! A schema entry's or an existing indexer's own `fields[]` carries each
//! field's *metadata* (`label`/`kind`/`selectOptions`) — this screen keeps
//! that Vec<Field> untouched in [`ScreenState`] and tracks the form's own
//! *current* values separately, in a `HashMap<String, String>` keyed by
//! field name (`crate::components::fields::field_value_as_string`/
//! `value_for_field` convert between that string state and the wire
//! [`serde_json::Value`] — see that module's own doc comment). Splitting
//! metadata from current value this way is what lets [`build_indexer_resource`]
//! stay a pure, natively-testable function with no `dioxus` involvement at
//! all.
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
    /// fetched — its own `fields[]` supplies the form's field metadata, and
    /// `crate::components::fields::field_value_as_string` seeds the form's
    /// initial per-field values from its own `value`s.
    Edit { resource: IndexerResource },
}

/// One `values` entry per `fields`, keyed by name, via
/// `crate::components::fields::field_value_as_string` — the form's initial
/// current-value state for a freshly chosen schema entry or an indexer
/// being edited.
#[must_use]
pub fn initial_values(fields: &[Field]) -> HashMap<String, String> {
    fields
        .iter()
        .map(|field| (field.name.clone(), field_value_as_string(field)))
        .collect()
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

/// Case-insensitive substring filter over `entries`'s own `name` — the add
/// flow's definition picker searches by name, per this task's own brief.
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
            h1 { "Indexers" }
            button { r#type: "button", onclick: move |_| on_add.call(()), "Add indexer" }
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
                            rsx! {
                                tr { key: "{id}",
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
                button { r#type: "submit", "Save" }
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
                    if let Some(resource) = found {
                        draft_name.set(resource.name.clone());
                        draft_priority.set(resource.priority);
                        draft_enable.set(resource.enable);
                        draft_values.set(initial_values(&resource.fields));
                        form_test_result.set(None);
                        state.set(ScreenState::Edit { resource });
                    }
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
                    let client = api.read().clone();
                    spawn(async move {
                        let outcome = match client.test_indexer(&resource).await {
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
                            draft_values.set(initial_values(&entry.fields));
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
                            let resource = build_indexer_resource(
                                0,
                                &draft_name.read(),
                                &entry.definition_name,
                                &entry.implementation,
                                *draft_enable.read(),
                                *draft_priority.read(),
                                &entry.fields,
                                &draft_values.read(),
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
                            let resource = build_indexer_resource(
                                0,
                                &draft_name.read(),
                                &entry.definition_name,
                                &entry.implementation,
                                *draft_enable.read(),
                                *draft_priority.read(),
                                &entry.fields,
                                &draft_values.read(),
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
                            let built = build_indexer_resource(
                                resource.id,
                                &draft_name.read(),
                                &resource.definition_name,
                                &resource.implementation,
                                *draft_enable.read(),
                                *draft_priority.read(),
                                &resource.fields,
                                &draft_values.read(),
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
                            let built = build_indexer_resource(
                                resource.id,
                                &draft_name.read(),
                                &resource.definition_name,
                                &resource.implementation,
                                *draft_enable.read(),
                                *draft_priority.read(),
                                &resource.fields,
                                &draft_values.read(),
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
}
