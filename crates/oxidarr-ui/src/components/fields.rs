//! Schema-field rendering: [`render_field`] maps a Cardigann/Prowlarr
//! [`Field`]'s own `kind` (`textbox`/`password`/`checkbox`/`select`, per
//! `oxidarr_prowl::api::dto`'s own "Field type mapping" section) onto one
//! form control; any other `kind` string falls back to a plain textbox
//! rather than rendering nothing — documented, not silently dropped.
//!
//! Every field's *current* value is threaded through this module (and
//! `crate::screens::indexers`, which owns the surrounding form) as a plain
//! `String` — including `checkbox`, whose wire value
//! (`crate::dto::Field::value`) is a JSON boolean. This module's own
//! literal spelling for that boolean, `"true"`/`"false"`
//! ([`CHECKBOX_TRUE`]/[`CHECKBOX_FALSE`]), matches
//! `oxidarr_prowl::api::dto::field_value`'s own exact string check
//! (`default == "true"`, in that module's doc comment: "A `checkbox`
//! setting's default renders as a real JSON boolean … matching real
//! Prowlarr's own `bool.TryParse` coercion") bit for bit —
//! [`value_for_field`] is this module's own inverse of that same check.
//! `select`'s value is already documented there as the raw option string,
//! not an index; this module keeps that convention for every kind that
//! isn't `checkbox`.
//!
//! # `password` never displays its own current value
//!
//! Browser-verified defect and its fix: [`render_field`]'s own `password`
//! branch renders a hardcoded blank `value` — never the `value` parameter
//! it's actually given — plus a "leave blank to keep the stored value"
//! hint. `crate::screens::indexers`'s own `initial_form_values` already
//! blanks this out before it ever reaches here, so this is defense in
//! depth, not the only place the secret is kept out of the form: even a
//! caller that got that wrong could never leak it into rendered HTML.
//! Typed characters still show up as the user types them — the DOM node's
//! own live value is what the browser actually displays, and this
//! component's own `value` attribute never changes between renders (always
//! the literal `""`), so nothing here ever fights that back to blank.
//! Saving with the field left blank preserves the stored value —
//! `crate::screens::indexers::preserve_blank_passwords`
//! (`crate::screens::applications::resolve_api_key` for the sibling
//! screen's own hand-shaped `apiKey`) is the save-side half of that
//! contract.

use dioxus::prelude::*;
use serde_json::Value;

use crate::dto::Field;

/// This module's own literal spelling for a `checkbox` field's "on" state —
/// see this module's own doc comment.
pub const CHECKBOX_TRUE: &str = "true";
/// This module's own literal spelling for a `checkbox` field's "off"
/// state — anything other than [`CHECKBOX_TRUE`] means this, matching
/// [`value_for_field`]'s own permissive parse.
pub const CHECKBOX_FALSE: &str = "false";

/// Extracts `field`'s current value as this module's own plain-`String`
/// form-editing state — see this module's own doc comment. A `checkbox`'s
/// JSON boolean becomes [`CHECKBOX_TRUE`]/[`CHECKBOX_FALSE`]; every other
/// kind's JSON string is taken as-is; a non-string, non-bool value (should
/// not occur for these four kinds, but this must still total) falls back to
/// its `Display` rendering; a `None` value (a schema entry with no
/// `default:`) becomes `""` for every kind except `checkbox`, which becomes
/// [`CHECKBOX_FALSE`] — an unset checkbox is unchecked, not blank.
#[must_use]
pub fn field_value_as_string(field: &Field) -> String {
    match &field.value {
        Some(Value::Bool(value)) => if *value {
            CHECKBOX_TRUE
        } else {
            CHECKBOX_FALSE
        }
        .to_string(),
        Some(Value::String(value)) => value.clone(),
        Some(other) => other.to_string(),
        None if field.kind == "checkbox" => CHECKBOX_FALSE.to_string(),
        None => String::new(),
    }
}

/// The inverse of [`field_value_as_string`] for outbound payloads: builds
/// the JSON [`Value`] a `kind`-typed field's current string state should
/// serialize as. `"checkbox"` parses via the same exact `value ==
/// "true"` check `field_value_as_string`'s own doc comment cites — anything
/// else (not just the literal `"false"`) means `false`, matching that
/// server-side function's own permissiveness; every other kind is stored as
/// the raw string verbatim, matching the server's own write path
/// (`crates/oxidarr-prowl/src/api/indexers.rs`'s `settings_from_fields`:
/// "storing that JSON value verbatim").
#[must_use]
pub fn value_for_field(kind: &str, value: &str) -> Value {
    if kind == "checkbox" {
        Value::Bool(value == CHECKBOX_TRUE)
    } else {
        Value::String(value.to_string())
    }
}

/// Renders one form control for `field`, given its current `value` (this
/// module's own string convention) and a shared `oninput` that fires
/// `(field.name.clone(), new_value)` on every change, so one callback in
/// the caller's own form can dispatch by field name rather than every field
/// needing its own handler.
///
/// # Errors
///
/// Never returns `Err` itself — [`Element`] is `dioxus_core`'s own
/// `Result<VNode, RenderError>` alias (every `rsx!`-built value is one), not
/// a fallible outcome this function's own logic can produce.
pub fn render_field(
    field: &Field,
    value: &str,
    oninput: EventHandler<(String, String)>,
) -> Element {
    match field.kind.as_str() {
        "checkbox" => {
            let checked = value == CHECKBOX_TRUE;
            let name = field.name.clone();
            rsx! {
                input {
                    r#type: "checkbox",
                    name: "{field.name}",
                    checked,
                    onchange: move |event| {
                        let checked = event.checked();
                        oninput
                            .call((
                                name.clone(),
                                if checked { CHECKBOX_TRUE } else { CHECKBOX_FALSE }.to_string(),
                            ));
                    },
                }
            }
        }
        "select" => {
            let options = field.select_options.clone().unwrap_or_default();
            let name = field.name.clone();
            let current = value.to_string();
            rsx! {
                select {
                    name: "{field.name}",
                    onchange: move |event| oninput.call((name.clone(), event.value())),
                    for option in options {
                        option {
                            key: "{option.value}",
                            value: "{option.name}",
                            selected: option.name == current,
                            "{option.name}"
                        }
                    }
                }
            }
        }
        "password" => {
            // Browser-verified defect: this input must never display a
            // stored secret, so `value` (whatever the caller's own current
            // state holds — see `crate::screens::indexers`'s own doc
            // comment on why that state can carry the real secret even
            // though it must never be *shown*) is deliberately never
            // reflected here; the literal `""` below is what actually
            // renders, on every render, so a user's own freshly typed
            // characters are never fought back to blank either (the DOM
            // node's own live value is what the browser displays once this
            // attribute stops changing between renders — see this
            // function's own module doc comment). Saving with this left
            // blank preserves the stored value —
            // `crate::screens::indexers::preserve_blank_passwords` (and
            // its `crate::screens::applications::resolve_api_key`
            // counterpart) is the save-side half of that contract.
            let name = field.name.clone();
            rsx! {
                input {
                    r#type: "password",
                    name: "{field.name}",
                    value: "",
                    oninput: move |event| oninput.call((name.clone(), event.value())),
                }
                p { class: "hint", "Leave blank to keep the stored value" }
            }
        }
        _ => {
            let name = field.name.clone();
            rsx! {
                input {
                    r#type: "text",
                    name: "{field.name}",
                    value: "{value}",
                    oninput: move |event| oninput.call((name.clone(), event.value())),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::cell::RefCell;

    use super::*;
    use crate::dto::SelectOption;

    fn field(kind: &str, value: Option<Value>) -> Field {
        Field {
            name: "cookie".to_string(),
            label: "Cookie".to_string(),
            kind: kind.to_string(),
            value,
            select_options: None,
        }
    }

    #[test]
    fn a_textbox_value_extracts_as_its_raw_string() {
        let f = field("textbox", Some(Value::String("abc".to_string())));
        assert_eq!(field_value_as_string(&f), "abc");
    }

    #[test]
    fn a_password_value_extracts_as_its_raw_string() {
        let f = field("password", Some(Value::String("s3cret".to_string())));
        assert_eq!(field_value_as_string(&f), "s3cret");
    }

    #[test]
    fn a_select_value_extracts_as_its_raw_string_not_an_index() {
        let f = field("select", Some(Value::String("seeders".to_string())));
        assert_eq!(field_value_as_string(&f), "seeders");
    }

    #[test]
    fn a_checkbox_true_value_extracts_as_the_literal_true_string() {
        let f = field("checkbox", Some(Value::Bool(true)));
        assert_eq!(field_value_as_string(&f), "true");
    }

    #[test]
    fn a_checkbox_false_value_extracts_as_the_literal_false_string() {
        let f = field("checkbox", Some(Value::Bool(false)));
        assert_eq!(field_value_as_string(&f), "false");
    }

    #[test]
    fn a_missing_checkbox_value_extracts_as_false_not_blank() {
        let f = field("checkbox", None);
        assert_eq!(field_value_as_string(&f), "false");
    }

    #[test]
    fn a_missing_textbox_value_extracts_as_blank() {
        let f = field("textbox", None);
        assert_eq!(field_value_as_string(&f), "");
    }

    #[test]
    fn an_unknown_kinds_missing_value_extracts_as_blank_same_as_textbox() {
        let f = field("magic", None);
        assert_eq!(field_value_as_string(&f), "");
    }

    #[test]
    fn value_for_field_parses_checkbox_true_exactly() {
        assert_eq!(value_for_field("checkbox", "true"), Value::Bool(true));
    }

    #[test]
    fn value_for_field_parses_anything_else_as_checkbox_false() {
        assert_eq!(value_for_field("checkbox", "false"), Value::Bool(false));
        assert_eq!(value_for_field("checkbox", "nonsense"), Value::Bool(false));
        assert_eq!(value_for_field("checkbox", ""), Value::Bool(false));
    }

    #[test]
    fn value_for_field_keeps_every_other_kind_as_a_raw_string() {
        assert_eq!(
            value_for_field("textbox", "abc"),
            Value::String("abc".to_string())
        );
        assert_eq!(
            value_for_field("select", "seeders"),
            Value::String("seeders".to_string())
        );
        assert_eq!(
            value_for_field("password", "s3cret"),
            Value::String("s3cret".to_string())
        );
    }

    #[test]
    fn checkbox_and_textbox_round_trip_through_both_functions() {
        for (kind, wire) in [
            ("checkbox", Value::Bool(true)),
            ("checkbox", Value::Bool(false)),
            ("textbox", Value::String("abc".to_string())),
            ("select", Value::String("seeders".to_string())),
        ] {
            let f = field(kind, Some(wire.clone()));
            let as_string = field_value_as_string(&f);
            assert_eq!(value_for_field(kind, &as_string), wire);
        }
    }

    #[test]
    fn select_options_survive_a_field_clone_for_render_field_to_read() {
        // Not a render assertion (that's `tests/snapshots.rs`'s job) — just
        // proves the fixture shape this module's own doc comment describes
        // is what a schema entry actually carries.
        let f = Field {
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
            ..field("select", Some(Value::String("seeders".to_string())))
        };
        assert_eq!(f.select_options.as_ref().unwrap().len(), 2);
    }

    /// `render_field` builds an `EventHandler`, which — like `Callback::new`
    /// generally — needs a running `dioxus` runtime to construct at all;
    /// wrapping the call in a plain (non-capturing) `fn` passed to
    /// `VirtualDom::new` gives it one, same as `tests/snapshots.rs`'s own
    /// `key_prompt_html`/`banner_html` helpers.
    fn render_field_html(field: &Field, value: &str) -> String {
        thread_local! {
            static FIELD: RefCell<Option<Field>> = const { RefCell::new(None) };
            static VALUE: RefCell<String> = const { RefCell::new(String::new()) };
        }

        fn root() -> Element {
            let field = FIELD.with(|cell| cell.borrow().clone()).unwrap();
            let value = VALUE.with(|cell| cell.borrow().clone());
            render_field(&field, &value, EventHandler::new(|_: (String, String)| {}))
        }

        FIELD.with(|cell| *cell.borrow_mut() = Some(field.clone()));
        VALUE.with(|cell| *cell.borrow_mut() = value.to_string());
        let mut vdom = VirtualDom::new(root);
        vdom.rebuild_in_place();
        dioxus::ssr::render(&vdom)
    }

    #[test]
    fn render_field_falls_back_to_a_text_input_for_an_unknown_kind() {
        let f = field("magic", None);
        let html = render_field_html(&f, "");
        assert_eq!(html, r#"<input type="text" name="cookie" value=""/>"#);
    }

    /// Browser-verified defect: a schema's `password` setting must never
    /// render its actual stored value — regardless of what this function is
    /// handed as `value` — plus a hint explaining that leaving it blank
    /// keeps the stored value (`crate::screens::indexers`'s own
    /// `preserve_blank_passwords` is the save-side half of that same
    /// contract).
    #[test]
    fn render_field_never_shows_a_passwords_actual_value() {
        let f = field("password", Some(Value::String("s3cret".to_string())));
        let html = render_field_html(&f, "s3cret");
        assert!(
            !html.contains("s3cret"),
            "the stored secret leaked into rendered HTML: {html}"
        );
        assert_eq!(
            html,
            concat!(
                r#"<input type="password" name="cookie" value=""/>"#,
                r#"<p class="hint">Leave blank to keep the stored value</p>"#,
            )
        );
    }
}
