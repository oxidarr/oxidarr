//! The applications screen: list (name/implementation/sync level,
//! `syncError` badge, delete-with-confirm, test-with-inline-result), an add
//! flow, and edit — the sibling of `crate::screens::indexers`, following
//! that module's own split: every `*View` component below is plain props
//! in, no context, no fetching — trivially SSR-snapshotted
//! (`tests/snapshots.rs`); [`Applications`] is the routed container that
//! fetches from the shared `ApiClient`, holds this screen's own interactive
//! state, and feeds whichever view the current [`ScreenState`] calls for.
//!
//! # Hand-shaped form, not schema-driven
//!
//! Unlike indexers (whose settings form is rendered from a chosen
//! definition's own `fields[]`), an application's form is fixed: `name`, an
//! `implementation` select (`"Sonarr"`/`"Radarr"`), `baseUrl`, `apiKey` (a
//! password input), and a `syncLevel` select. There is no `GET
//! /applications/schema` to pick a definition from, so [`ScreenState::Add`]
//! carries no payload at all — unlike
//! `crate::screens::indexers::ScreenState::Add`'s own `schema: Option<..>`,
//! this screen's add flow goes straight to the form.
//!
//! `baseUrl`/`apiKey` still round-trip through [`ApplicationResource::fields`]
//! on the wire (matching the server's own `oxidarr_prowl::api::applications`
//! `row_to_fields`/`required_field`), so [`build_application_resource`] packs
//! the form's own two plain strings into that `Vec<Field>` shape rather than
//! this screen keeping a `HashMap<String, String>` of current values the way
//! `crate::screens::indexers` does for its own schema-driven fields.
//!
//! # Outbound `sync_error`
//!
//! [`build_application_resource`] never sets [`ApplicationResource::sync_error`]
//! — it is always `None` on every outbound create/update payload, matching
//! that field's own doc comment (mirroring
//! `crate::dto::IndexerResource::sync_error`'s own "never sent by this
//! client" contract). Every payload this screen sends is built by that one
//! function — there is no second, ad hoc construction path a later change
//! could let a carried `sync_error` slip back out through.
//!
//! # Client-side validation
//!
//! [`validate_application_form`] mirrors the server's own three checks in
//! `oxidarr_prowl::api::applications::build_new_application`/`run_test`
//! (`required_field` for `baseUrl`/`apiKey`, `validate_url` for `baseUrl`),
//! plus a `name` check that handler has no need for (server-side `name` is
//! passed straight through with no validation — the UI still requires it,
//! since a blank name is never a sensible resource to save). Running it
//! before every save/test avoids the round trip the server's own `400`
//! would otherwise cost: [`ScreenState`]'s form states keep the last
//! computed errors in the container's own `form_errors` signal, cleared
//! and recomputed on every submit attempt.
//!
//! # Avoiding a `Signal` read guard held across `.await`
//!
//! Every mutating handler below (`create`/`update`/`delete`/`test`) clones
//! the shared `ApiClient` out of its `Signal` in a synchronous statement
//! before the surrounding `async move` block's first `.await` — see
//! `crate::screens::indexers`'s own doc comment ("Avoiding a `Signal` read
//! guard held across `.await`") for exactly why `api.read().clone().x().await`
//! in one statement is unsound here.

use std::collections::HashMap;

use dioxus::prelude::*;

use crate::app::{Session, SharedApi, handle_error};
use crate::components::fields::field_value_as_string;
use crate::dto::{ApplicationResource, Field, SyncLevel};

/// Outcome of a `POST /applications/test` probe kept for one row (list) or
/// the open form (add/edit). See `crate::screens::indexers::TestOutcome`'s
/// own doc comment — this is that same shape, duplicated here rather than
/// shared, so this screen has no compile-time dependency on a sibling
/// screen module for a two-variant enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestOutcome {
    /// The probe reached the application successfully.
    Ok,
    /// The probe failed; carries the rendered failure message (see
    /// [`test_failure_message`]).
    Failed(String),
}

/// This screen's own state machine — see this module's own doc comment for
/// why [`ScreenState::Add`] carries no payload, unlike
/// `crate::screens::indexers::ScreenState::Add`.
#[derive(Debug, Clone, PartialEq)]
pub enum ScreenState {
    /// The application list — this screen's default and its "back" target
    /// from every other state.
    List,
    /// The add flow: the form starts blank (see [`Applications`]'s own
    /// `on_add` handler for the exact defaults).
    Add,
    /// The edit flow: `resource` is the application being edited, as last
    /// fetched — its own `fields` supplies the form's initial `baseUrl`/
    /// `apiKey` values via [`field_value`].
    Edit { resource: ApplicationResource },
}

/// Extracts the named field's current value out of `fields` as a plain
/// `String` — `""` when no field with that `name` is present. Used to seed
/// the edit form's `baseUrl`/`apiKey` drafts from an existing
/// [`ApplicationResource`]'s own `fields`, and (indirectly, via
/// [`crate::components::fields::field_value_as_string`]) to normalize
/// whatever value shape the wire sent (always a JSON string for these two
/// fields in practice, but this reuses the same non-string fallback that
/// module's own doc comment describes rather than assuming so blindly).
#[must_use]
pub fn field_value(fields: &[Field], name: &str) -> String {
    fields
        .iter()
        .find(|field| field.name == name)
        .map(field_value_as_string)
        .unwrap_or_default()
}

/// Builds the outbound [`ApplicationResource`] a create/update/test call
/// sends: `id` carried straight through (`0` for a create — skipped from
/// the JSON entirely by that field's own `skip_serializing_if`), `fields`
/// always exactly the two-entry `[baseUrl, apiKey]` shape the server's own
/// `row_to_fields` produces (see this module's own doc comment), each
/// field's own `kind` sent as `"textbox"` — matching the fixture
/// `crates/oxidarr-prowl/tests/fixtures/wire/application_resource_sync_error.json`,
/// which carries that literal string for both `baseUrl` and `apiKey`
/// regardless of `apiKey`'s own password *input* in this screen's form (a
/// UI presentation choice with no wire-shape consequence: the server's own
/// `required_field` reads `fields[].value` by name, never `fields[].type`).
/// `sync_error` is always `None` — see this module's own doc comment.
#[must_use]
pub fn build_application_resource(
    id: i32,
    name: &str,
    implementation: &str,
    sync_level: SyncLevel,
    base_url: &str,
    api_key: &str,
) -> ApplicationResource {
    ApplicationResource {
        id,
        name: name.to_string(),
        implementation: implementation.to_string(),
        sync_level,
        fields: vec![
            Field {
                name: "baseUrl".to_string(),
                label: "baseUrl".to_string(),
                kind: "textbox".to_string(),
                value: Some(serde_json::Value::String(base_url.to_string())),
                select_options: None,
            },
            Field {
                name: "apiKey".to_string(),
                label: "apiKey".to_string(),
                kind: "textbox".to_string(),
                value: Some(serde_json::Value::String(api_key.to_string())),
                select_options: None,
            },
        ],
        sync_error: None,
    }
}

/// Human-readable label for a [`SyncLevel`] — the list column and the
/// form's own select option text.
#[must_use]
pub const fn sync_level_label(level: SyncLevel) -> &'static str {
    match level {
        SyncLevel::Disabled => "Disabled",
        SyncLevel::AddOnly => "Add only",
        SyncLevel::FullSync => "Full sync",
    }
}

/// The wire string for a [`SyncLevel`] — matches
/// [`crate::dto::SyncLevel`]'s own `serde` `camelCase` rename exactly (see
/// that type's own doc comment and `crate::dto`'s
/// `sync_level_uses_the_servers_own_camel_case_strings` test), reused here
/// as the form select's own `option value` so [`sync_level_from_wire`] can
/// parse a change event's raw string back into a [`SyncLevel`] with no
/// separate lookup table to keep in sync.
#[must_use]
pub const fn sync_level_wire(level: SyncLevel) -> &'static str {
    match level {
        SyncLevel::Disabled => "disabled",
        SyncLevel::AddOnly => "addOnly",
        SyncLevel::FullSync => "fullSync",
    }
}

/// The inverse of [`sync_level_wire`] — `None` for anything not produced by
/// that function (should not occur from this screen's own select, which
/// only ever offers those three option values, but a change handler still
/// totals rather than assuming).
#[must_use]
pub fn sync_level_from_wire(value: &str) -> Option<SyncLevel> {
    match value {
        "disabled" => Some(SyncLevel::Disabled),
        "addOnly" => Some(SyncLevel::AddOnly),
        "fullSync" => Some(SyncLevel::FullSync),
        _ => None,
    }
}

/// Client-side validation mirroring the server's own three checks — see
/// this module's own doc comment. Returns every failing rule's own message,
/// in `name`/`baseUrl`/`apiKey` order; an empty `Vec` means the form is
/// ready to submit. Trims neither input before checking emptiness: a
/// whitespace-only value is not usefully "non-empty" for any of these three
/// fields, but this function only guards against the literal empty string
/// the server's own `required_field` rejects (`value.is_empty()`) — the
/// same standard, not a stricter one this client invents on its own.
#[must_use]
pub fn validate_application_form(name: &str, base_url: &str, api_key: &str) -> Vec<String> {
    let mut errors = Vec::new();
    if name.is_empty() {
        errors.push("Name is required".to_string());
    }
    if base_url.is_empty() {
        errors.push("Base URL is required".to_string());
    } else if base_url.parse::<url::Url>().is_err() {
        errors.push(format!("Base URL {base_url:?} is not a valid URL"));
    }
    if api_key.is_empty() {
        errors.push("API key is required".to_string());
    }
    errors
}

/// Renders `outcome` inline — see
/// `crate::screens::indexers::TestOutcomeView`'s own doc comment; this is
/// that same rendering, duplicated for this screen's own [`TestOutcome`].
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
pub fn ApplicationListView(
    applications: Vec<ApplicationResource>,
    confirm_delete_id: Option<i32>,
    test_results: HashMap<i32, TestOutcome>,
    on_add: EventHandler<()>,
    on_edit: EventHandler<i32>,
    on_delete_request: EventHandler<i32>,
    on_delete_confirm: EventHandler<i32>,
    on_delete_cancel: EventHandler<()>,
    on_test: EventHandler<i32>,
) -> Element {
    rsx! {
        div { id: "applications-screen",
            h1 { "Applications" }
            button { r#type: "button", onclick: move |_| on_add.call(()), "Add application" }
            table {
                thead {
                    tr {
                        th { "Name" }
                        th { "Implementation" }
                        th { "Sync level" }
                        th { "Test" }
                        th { "Actions" }
                    }
                }
                tbody {
                    for application in applications {
                        {
                            let id = application.id;
                            let outcome = test_results.get(&id).cloned();
                            let confirming = confirm_delete_id == Some(id);
                            rsx! {
                                tr { key: "{id}",
                                    td {
                                        "{application.name}"
                                        if let Some(sync_error) = &application.sync_error {
                                            " "
                                            span { class: "sync-error", "{sync_error}" }
                                        }
                                    }
                                    td { "{application.implementation}" }
                                    td { "{sync_level_label(application.sync_level)}" }
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

/// The add/edit form — hand-shaped, not schema-driven (see this module's
/// own doc comment). `errors` is [`validate_application_form`]'s own last
/// computed result for this form's current values, shown inline above the
/// buttons rather than round-tripped through a server `400`.
#[component]
pub fn ApplicationFormView(
    mode_label: String,
    name: String,
    implementation: String,
    base_url: String,
    api_key: String,
    sync_level: SyncLevel,
    errors: Vec<String>,
    test_result: Option<TestOutcome>,
    on_name_change: EventHandler<String>,
    on_implementation_change: EventHandler<String>,
    on_base_url_change: EventHandler<String>,
    on_api_key_change: EventHandler<String>,
    on_sync_level_change: EventHandler<String>,
    on_test: EventHandler<()>,
    on_save: EventHandler<()>,
    on_cancel: EventHandler<()>,
) -> Element {
    rsx! {
        div { id: "application-form",
            h1 { "{mode_label} application" }
            if !errors.is_empty() {
                ul { class: "form-errors",
                    for error in &errors {
                        li { key: "{error}", "{error}" }
                    }
                }
            }
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
                    "Implementation"
                    select {
                        onchange: move |event| on_implementation_change.call(event.value()),
                        for option in ["Sonarr", "Radarr"] {
                            option {
                                key: "{option}",
                                value: "{option}",
                                selected: option == implementation,
                                "{option}"
                            }
                        }
                    }
                }
                label {
                    "Base URL"
                    input {
                        r#type: "text",
                        value: "{base_url}",
                        oninput: move |event| on_base_url_change.call(event.value()),
                    }
                }
                label {
                    "API key"
                    input {
                        r#type: "password",
                        value: "{api_key}",
                        oninput: move |event| on_api_key_change.call(event.value()),
                    }
                }
                label {
                    "Sync level"
                    select {
                        onchange: move |event| on_sync_level_change.call(event.value()),
                        for level in [SyncLevel::Disabled, SyncLevel::AddOnly, SyncLevel::FullSync] {
                            option {
                                key: "{sync_level_wire(level)}",
                                value: "{sync_level_wire(level)}",
                                selected: level == sync_level,
                                "{sync_level_label(level)}"
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
/// `crate::screens::indexers::Indexers`'s own non-test half): every
/// `*View` component above carries this screen's real test coverage.
#[component]
pub fn Applications() -> Element {
    let api = use_context::<SharedApi>();
    let mut session = use_context::<Session>();

    let mut state = use_signal(|| ScreenState::List);
    let mut confirm_delete_id = use_signal(|| None::<i32>);
    let mut list_test_results = use_signal(HashMap::<i32, TestOutcome>::new);
    let mut form_test_result = use_signal(|| None::<TestOutcome>);
    let mut form_errors = use_signal(Vec::<String>::new);
    let mut draft_name = use_signal(String::new);
    let mut draft_implementation = use_signal(|| "Sonarr".to_string());
    let mut draft_base_url = use_signal(String::new);
    let mut draft_api_key = use_signal(String::new);
    let mut draft_sync_level = use_signal(|| SyncLevel::AddOnly);

    // See this module's own doc comment ("Avoiding a `Signal` read guard
    // held across `.await`") for why the client is cloned out of the
    // `Signal` in this synchronous closure, before the `async move` block
    // even exists.
    let mut applications = use_resource(move || {
        let client = api.read().clone();
        async move { client.applications().await }
    });

    let list = match &*applications.read() {
        None => return rsx! { p { "Loading…" } },
        Some(Ok(list)) => list.clone(),
        Some(Err(err)) => {
            handle_error(err.clone(), &mut session);
            return rsx! {};
        }
    };

    let find = move |id: i32| -> Option<ApplicationResource> {
        applications
            .read()
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .and_then(|list| list.iter().find(|item| item.id == id).cloned())
    };

    match &*state.read() {
        ScreenState::List => rsx! {
            ApplicationListView {
                applications: list,
                confirm_delete_id: *confirm_delete_id.read(),
                test_results: list_test_results.read().clone(),
                on_add: move |()| {
                    draft_name.set(String::new());
                    draft_implementation.set("Sonarr".to_string());
                    draft_base_url.set(String::new());
                    draft_api_key.set(String::new());
                    draft_sync_level.set(SyncLevel::AddOnly);
                    form_errors.set(Vec::new());
                    form_test_result.set(None);
                    state.set(ScreenState::Add);
                },
                on_edit: move |id: i32| {
                    if let Some(resource) = find(id) {
                        draft_name.set(resource.name.clone());
                        draft_implementation.set(resource.implementation.clone());
                        draft_base_url.set(field_value(&resource.fields, "baseUrl"));
                        draft_api_key.set(field_value(&resource.fields, "apiKey"));
                        draft_sync_level.set(resource.sync_level);
                        form_errors.set(Vec::new());
                        form_test_result.set(None);
                        state.set(ScreenState::Edit { resource });
                    }
                },
                on_delete_request: move |id: i32| confirm_delete_id.set(Some(id)),
                on_delete_confirm: move |id: i32| {
                    let client = api.read().clone();
                    spawn(async move {
                        let mut session = session;
                        match client.delete_application(id).await {
                            Ok(()) => {
                                confirm_delete_id.set(None);
                                applications.restart();
                            }
                            Err(err) => handle_error(err, &mut session),
                        }
                    });
                },
                on_delete_cancel: move |()| confirm_delete_id.set(None),
                on_test: move |id: i32| {
                    let Some(resource) = find(id) else { return };
                    let client = api.read().clone();
                    spawn(async move {
                        let outcome = match client.test_application(&resource).await {
                            Ok(()) => TestOutcome::Ok,
                            Err(err) => TestOutcome::Failed(test_failure_message(err)),
                        };
                        list_test_results.write().insert(id, outcome);
                    });
                },
            }
        },
        ScreenState::Add => rsx! {
            ApplicationFormView {
                mode_label: "Add",
                name: draft_name.read().clone(),
                implementation: draft_implementation.read().clone(),
                base_url: draft_base_url.read().clone(),
                api_key: draft_api_key.read().clone(),
                sync_level: *draft_sync_level.read(),
                errors: form_errors.read().clone(),
                test_result: form_test_result.read().clone(),
                on_name_change: move |value| draft_name.set(value),
                on_implementation_change: move |value| draft_implementation.set(value),
                on_base_url_change: move |value| draft_base_url.set(value),
                on_api_key_change: move |value| draft_api_key.set(value),
                on_sync_level_change: move |value: String| {
                    if let Some(level) = sync_level_from_wire(&value) {
                        draft_sync_level.set(level);
                    }
                },
                on_test: move |()| {
                    let errors = validate_application_form(
                        &draft_name.read(),
                        &draft_base_url.read(),
                        &draft_api_key.read(),
                    );
                    if !errors.is_empty() {
                        form_errors.set(errors);
                        return;
                    }
                    form_errors.set(Vec::new());
                    let resource = build_application_resource(
                        0,
                        &draft_name.read(),
                        &draft_implementation.read(),
                        *draft_sync_level.read(),
                        &draft_base_url.read(),
                        &draft_api_key.read(),
                    );
                    let client = api.read().clone();
                    spawn(async move {
                        let outcome = match client.test_application(&resource).await {
                            Ok(()) => TestOutcome::Ok,
                            Err(err) => TestOutcome::Failed(test_failure_message(err)),
                        };
                        form_test_result.set(Some(outcome));
                    });
                },
                on_save: move |()| {
                    let errors = validate_application_form(
                        &draft_name.read(),
                        &draft_base_url.read(),
                        &draft_api_key.read(),
                    );
                    if !errors.is_empty() {
                        form_errors.set(errors);
                        return;
                    }
                    form_errors.set(Vec::new());
                    let resource = build_application_resource(
                        0,
                        &draft_name.read(),
                        &draft_implementation.read(),
                        *draft_sync_level.read(),
                        &draft_base_url.read(),
                        &draft_api_key.read(),
                    );
                    let client = api.read().clone();
                    spawn(async move {
                        let mut session = session;
                        match client.create_application(&resource).await {
                            Ok(_) => {
                                applications.restart();
                                state.set(ScreenState::List);
                            }
                            Err(err) => handle_error(err, &mut session),
                        }
                    });
                },
                on_cancel: move |()| state.set(ScreenState::List),
            }
        },
        ScreenState::Edit { resource } => {
            let resource_id = resource.id;
            rsx! {
                ApplicationFormView {
                    mode_label: "Edit",
                    name: draft_name.read().clone(),
                    implementation: draft_implementation.read().clone(),
                    base_url: draft_base_url.read().clone(),
                    api_key: draft_api_key.read().clone(),
                    sync_level: *draft_sync_level.read(),
                    errors: form_errors.read().clone(),
                    test_result: form_test_result.read().clone(),
                    on_name_change: move |value| draft_name.set(value),
                    on_implementation_change: move |value| draft_implementation.set(value),
                    on_base_url_change: move |value| draft_base_url.set(value),
                    on_api_key_change: move |value| draft_api_key.set(value),
                    on_sync_level_change: move |value: String| {
                        if let Some(level) = sync_level_from_wire(&value) {
                            draft_sync_level.set(level);
                        }
                    },
                    on_test: move |()| {
                        let errors = validate_application_form(
                            &draft_name.read(),
                            &draft_base_url.read(),
                            &draft_api_key.read(),
                        );
                        if !errors.is_empty() {
                            form_errors.set(errors);
                            return;
                        }
                        form_errors.set(Vec::new());
                        let built = build_application_resource(
                            resource_id,
                            &draft_name.read(),
                            &draft_implementation.read(),
                            *draft_sync_level.read(),
                            &draft_base_url.read(),
                            &draft_api_key.read(),
                        );
                        let client = api.read().clone();
                        spawn(async move {
                            let outcome = match client.test_application(&built).await {
                                Ok(()) => TestOutcome::Ok,
                                Err(err) => TestOutcome::Failed(test_failure_message(err)),
                            };
                            form_test_result.set(Some(outcome));
                        });
                    },
                    on_save: move |()| {
                        let errors = validate_application_form(
                            &draft_name.read(),
                            &draft_base_url.read(),
                            &draft_api_key.read(),
                        );
                        if !errors.is_empty() {
                            form_errors.set(errors);
                            return;
                        }
                        form_errors.set(Vec::new());
                        let built = build_application_resource(
                            resource_id,
                            &draft_name.read(),
                            &draft_implementation.read(),
                            *draft_sync_level.read(),
                            &draft_base_url.read(),
                            &draft_api_key.read(),
                        );
                        let client = api.read().clone();
                        spawn(async move {
                            let mut session = session;
                            match client.update_application(&built).await {
                                Ok(_) => {
                                    applications.restart();
                                    state.set(ScreenState::List);
                                }
                                Err(err) => handle_error(err, &mut session),
                            }
                        });
                    },
                    on_cancel: move |()| state.set(ScreenState::List),
                }
            }
        }
    }
}

/// Renders a [`crate::api::UiError`] as one failed test's own inline
/// message — see `crate::screens::indexers::test_failure_message`'s own
/// doc comment; this is that same rendering, duplicated for this screen.
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

    fn field(name: &str, value: &str) -> Field {
        Field {
            name: name.to_string(),
            label: name.to_string(),
            kind: "textbox".to_string(),
            value: Some(serde_json::Value::String(value.to_string())),
            select_options: None,
        }
    }

    fn sample_application() -> ApplicationResource {
        ApplicationResource {
            id: 3,
            name: "My Sonarr".to_string(),
            implementation: "Sonarr".to_string(),
            sync_level: SyncLevel::FullSync,
            fields: vec![
                field("baseUrl", "http://sonarr.example"),
                field("apiKey", "secretkey"),
            ],
            sync_error: None,
        }
    }

    #[test]
    fn build_application_resource_maps_args_onto_named_fields() {
        let resource = build_application_resource(
            0,
            "My Sonarr",
            "Sonarr",
            SyncLevel::FullSync,
            "http://sonarr.example",
            "secretkey",
        );
        assert_eq!(resource.id, 0);
        assert_eq!(resource.name, "My Sonarr");
        assert_eq!(resource.implementation, "Sonarr");
        assert_eq!(resource.sync_level, SyncLevel::FullSync);
        assert_eq!(resource.fields.len(), 2);
        assert_eq!(resource.fields[0].name, "baseUrl");
        assert_eq!(
            resource.fields[0].value,
            Some(serde_json::Value::String(
                "http://sonarr.example".to_string()
            ))
        );
        assert_eq!(resource.fields[1].name, "apiKey");
        assert_eq!(
            resource.fields[1].value,
            Some(serde_json::Value::String("secretkey".to_string()))
        );
        assert_eq!(resource.sync_error, None);
    }

    #[test]
    fn build_application_resource_never_carries_a_sync_error_through() {
        // A resource whose last write reported a sync failure — that
        // failure must never round-trip back out on the very next write
        // (`crate::dto::ApplicationResource::sync_error`'s own doc comment
        // mirrors `IndexerResource::sync_error`'s: never sent by this
        // client, only ever read back).
        let existing = ApplicationResource {
            sync_error: Some("unexpected status 500 from http://sonarr.example".to_string()),
            ..sample_application()
        };
        let rebuilt = build_application_resource(
            existing.id,
            &existing.name,
            &existing.implementation,
            existing.sync_level,
            "http://sonarr.example",
            "secretkey",
        );
        assert_eq!(rebuilt.sync_error, None);
    }

    #[test]
    fn field_value_extracts_a_named_field_by_name() {
        let fields = vec![
            field("baseUrl", "http://sonarr.example"),
            field("apiKey", "secretkey"),
        ];
        assert_eq!(field_value(&fields, "baseUrl"), "http://sonarr.example");
        assert_eq!(field_value(&fields, "apiKey"), "secretkey");
    }

    #[test]
    fn field_value_with_no_matching_field_returns_blank() {
        let fields = vec![field("baseUrl", "http://sonarr.example")];
        assert_eq!(field_value(&fields, "apiKey"), "");
    }

    #[test]
    fn sync_level_label_renders_human_readable_text() {
        assert_eq!(sync_level_label(SyncLevel::Disabled), "Disabled");
        assert_eq!(sync_level_label(SyncLevel::AddOnly), "Add only");
        assert_eq!(sync_level_label(SyncLevel::FullSync), "Full sync");
    }

    #[test]
    fn sync_level_wire_renders_the_servers_own_camel_case_strings() {
        assert_eq!(sync_level_wire(SyncLevel::Disabled), "disabled");
        assert_eq!(sync_level_wire(SyncLevel::AddOnly), "addOnly");
        assert_eq!(sync_level_wire(SyncLevel::FullSync), "fullSync");
    }

    #[test]
    fn sync_level_from_wire_parses_every_known_value() {
        assert_eq!(sync_level_from_wire("disabled"), Some(SyncLevel::Disabled));
        assert_eq!(sync_level_from_wire("addOnly"), Some(SyncLevel::AddOnly));
        assert_eq!(sync_level_from_wire("fullSync"), Some(SyncLevel::FullSync));
    }

    #[test]
    fn sync_level_from_wire_rejects_an_unknown_value() {
        assert_eq!(sync_level_from_wire("nonsense"), None);
    }

    #[test]
    fn sync_level_wire_and_from_wire_round_trip() {
        for level in [SyncLevel::Disabled, SyncLevel::AddOnly, SyncLevel::FullSync] {
            assert_eq!(sync_level_from_wire(sync_level_wire(level)), Some(level));
        }
    }

    #[test]
    fn validate_application_form_with_everything_valid_returns_no_errors() {
        let errors = validate_application_form("My Sonarr", "http://sonarr.example", "secretkey");
        assert!(errors.is_empty(), "errors were: {errors:?}");
    }

    #[test]
    fn validate_application_form_with_a_blank_name_reports_it() {
        let errors = validate_application_form("", "http://sonarr.example", "secretkey");
        assert!(
            errors.iter().any(|e| e.contains("Name")),
            "errors were: {errors:?}"
        );
    }

    #[test]
    fn validate_application_form_with_a_blank_base_url_reports_it() {
        let errors = validate_application_form("My Sonarr", "", "secretkey");
        assert!(
            errors
                .iter()
                .any(|e| e.contains("Base URL") || e.contains("base URL")),
            "errors were: {errors:?}"
        );
    }

    #[test]
    fn validate_application_form_with_a_malformed_base_url_reports_it_as_invalid() {
        let errors = validate_application_form("My Sonarr", "not a url", "secretkey");
        assert!(
            errors.iter().any(|e| e.to_lowercase().contains("valid")),
            "errors were: {errors:?}"
        );
    }

    #[test]
    fn validate_application_form_with_a_blank_api_key_reports_it() {
        let errors = validate_application_form("My Sonarr", "http://sonarr.example", "");
        assert!(
            errors
                .iter()
                .any(|e| e.contains("API key") || e.contains("api key")),
            "errors were: {errors:?}"
        );
    }

    #[test]
    fn validate_application_form_with_everything_blank_reports_three_errors() {
        let errors = validate_application_form("", "", "");
        assert_eq!(errors.len(), 3, "errors were: {errors:?}");
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
