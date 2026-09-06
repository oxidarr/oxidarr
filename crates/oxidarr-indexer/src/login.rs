//! Login flow orchestration, failure detection, and session verification.
//!
//! [`authenticate`] dispatches on a definition's `login.method` and drives
//! the post/get/cookie/form flows on top of two independent checks: whether
//! a login response indicates the credentials were rejected
//! ([`check_error_rules`]), and whether an existing session is still
//! authenticated ([`verify_login`]). The scraped-hidden-input `form` flow
//! (and the method-less `selectorinputs` style, e.g. `postman.yml`, which
//! shares the same machinery — see [`form_login`]'s doc comment) is
//! implemented by [`form_login`].

use chrono::Utc;
use scraper::{ElementRef, Html};

use oxidarr_cardigann::filters::{self, FilterCtx, FilterOutcome};
use oxidarr_cardigann::model::{Definition, ErrorRule, Field, Login, LoginTest};
use oxidarr_cardigann::selector::{self, CompiledSelector};
use oxidarr_cardigann::template;

use crate::builder::resolve_base_url;
use crate::client::{Body, HttpClient, HttpRequest, Method};
use crate::error::HttpError;
use crate::query::{SearchQuery, Settings, scope_for};

/// Failures from checking a login response or verifying a session.
#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    /// One of the definition's `login.error` rules matched the response;
    /// the login was rejected for the reason named.
    #[error("login rejected: {0}")]
    Rejected(String),
    /// The definition's login block requires solving a captcha, which this
    /// engine cannot do.
    #[error("definition requires a captcha")]
    NeedsCaptcha,
    /// The request made to check the login response or session status
    /// failed at the transport.
    #[error(transparent)]
    Http(#[from] HttpError),
    /// The definition's login block itself is malformed (e.g. a selector
    /// that fails to compile, or a JSON field-path selector where only
    /// HTML checks are supported).
    #[error("login definition problem: {0}")]
    Definition(String),
}

/// Checks `body` against each of `rules` in declaration order, returning
/// [`LoginError::Rejected`] with the message from the first rule whose
/// selector matches. A rule's message comes from its own `message` field
/// when present — a literal `text:`, or the text of the element found by
/// `message`'s `selector:` (searched within the rule's matched element) —
/// and otherwise from the matched element's own text, trimmed.
///
/// Returns `Ok(())` when `rules` is empty or none of them match.
///
/// # Errors
/// Returns [`LoginError::Definition`] if a rule's selector, or its
/// message's selector, fails to compile.
pub(crate) fn check_error_rules(rules: &[ErrorRule], body: &str) -> Result<(), LoginError> {
    let doc = Html::parse_document(body);
    let root = doc.root_element();

    for rule in rules {
        let compiled = compile_html_selector(&rule.selector)?;
        let Some(matched) = compiled.select(root).into_iter().next() else {
            continue;
        };
        return Err(LoginError::Rejected(error_message(
            rule.message.as_ref(),
            matched,
        )?));
    }
    Ok(())
}

/// Resolves an `ErrorRule`'s human-readable message for a matched element.
fn error_message(message: Option<&Field>, matched: ElementRef<'_>) -> Result<String, LoginError> {
    if let Some(field) = message {
        if let Some(text) = &field.text {
            return Ok(text.clone());
        }
        if let Some(sel) = &field.selector {
            let compiled = compile_html_selector(sel)?;
            let text = compiled
                .select(matched)
                .into_iter()
                .next()
                .map(|el| el.text().collect::<String>())
                .unwrap_or_default();
            return Ok(text.trim().to_string());
        }
    }
    Ok(matched.text().collect::<String>().trim().to_string())
}

/// Compiles `raw` for use against a parsed HTML document, rejecting JSON
/// field-path selectors (login checks only ever run against HTML login
/// pages, never a JSON API response).
fn compile_html_selector(raw: &str) -> Result<CompiledSelector, LoginError> {
    let compiled = selector::compile(raw).map_err(|e| LoginError::Definition(e.to_string()))?;
    if compiled.is_json_path() {
        return Err(LoginError::Definition(format!(
            "selector {raw:?} is a JSON field path; login checks only understand HTML"
        )));
    }
    Ok(compiled)
}

/// Verifies an existing session is still authenticated by requesting
/// `test.path` (resolved against `base`) and checking the response:
/// `test.selector` present ⇒ the session is valid iff it matches somewhere
/// in the body, regardless of status; absent ⇒ the session is valid iff
/// the response status is 2xx.
///
/// Returns `Ok(false)` for a response that fails either check — a failed
/// check is not itself an error, it just means the caller should
/// re-authenticate. Only a transport-level failure to make the request at
/// all is an `Err`.
///
/// # Errors
/// Returns [`LoginError::Definition`] if `test.path` cannot be resolved
/// against `base`, or if `test.selector` fails to compile.
/// Returns [`LoginError::Http`] if the request could not be executed.
pub async fn verify_login<C: HttpClient>(
    client: &C,
    base: &url::Url,
    test: &LoginTest,
) -> Result<bool, LoginError> {
    let url = base.join(&test.path).map_err(|e| {
        LoginError::Definition(format!("invalid login test path {:?}: {e}", test.path))
    })?;
    let req = HttpRequest {
        method: Method::Get,
        url,
        headers: Vec::new(),
        body: None,
    };
    let resp = client.execute(req).await?;

    let Some(sel) = &test.selector else {
        return Ok((200..300).contains(&resp.status));
    };

    let compiled = compile_html_selector(sel)?;
    let body = resp.text();
    let doc = Html::parse_document(&body);
    Ok(!compiled.select(doc.root_element()).is_empty())
}

/// Authenticates against `def`'s `login` block, dispatching on
/// `login.method` (case-insensitively, matching how `builder::build_request`
/// reads `search.paths[].method`).
///
/// - No `login` block: `Ok(())`, no request issued at all.
/// - `login.captcha` set: `Err(LoginError::NeedsCaptcha)`, checked before
///   any request — this engine cannot solve captchas.
/// - `method: post` / `method: get`: renders `login.inputs` and
///   `login.headers` against a [`oxidarr_cardigann::template::Scope`] built
///   the same way a search's would be
///   (`scope_for(def, &SearchQuery::default(), s)`), submits them to
///   `login.path` (resolved against `def.links`' first entry, the same
///   convention `builder::build_search_requests` uses) — a form body for
///   POST, query pairs for GET — then runs [`check_error_rules`] on the
///   response body, then [`verify_login`] when `login.test` is set. A
///   `login.test` that fails to verify is [`LoginError::Rejected`] with the
///   message `"login test failed"`.
/// - `method: cookie`: issues no request of its own. Cardigann's convention
///   for this style is a user-supplied setting named `cookie`
///   (`login.inputs.cookie: "{{ .Config.cookie }}"`, e.g. `teamos.yml`)
///   whose value is injected into the transport at construction time (see
///   [`crate::ReqwestClient::with_cookie`]), not something `authenticate`
///   submits itself — but `authenticate` does require that setting be
///   configured at all: an empty/absent `cookie` value is
///   [`LoginError::Definition`], since a login that can never work
///   shouldn't silently report success just because `login.test` happens
///   to be absent. When a `cookie` value is present, still runs
///   [`verify_login`] when `login.test` is set, exactly like the post/get
///   flows.
/// - `method: form` — drives the scraped-hidden-input flow; see
///   [`form_login`]'s doc comment for the full sequence.
/// - No `method` at all — the scraped-hidden-input `selectorinputs` style
///   with no explicit `form:` (e.g. `postman.yml`) — is routed through
///   [`form_login`] as well: it shares every step of that flow (the default
///   `form` selector, baseline-input collection, `selectorinputs`/`inputs`
///   overlay, action resolution), so there is no distinct "methodless" flow
///   to implement.
///   Note `login.cookies: Vec<String>` (e.g. `coastalcrew.yml`:
///   `cookies: ["JAVA=OK"]`) is an *unrelated* field — a list of literal
///   cookie strings to set on the client before requesting the login page
///   at all, always paired with `method: form` in the corpus, never a
///   marker for the `cookie`-auth style above. [`form_login`] is
///   responsible for setting those cookies before its initial GET.
///
/// # Errors
/// Returns [`LoginError::NeedsCaptcha`], [`LoginError::Rejected`],
/// [`LoginError::Http`], or [`LoginError::Definition`] (a missing/malformed
/// `login.path`, an unresolvable base link, an unconfigured `cookie`
/// setting, or an unimplemented method) — see [`LoginError`]'s variants.
pub async fn authenticate<C: HttpClient>(
    client: &C,
    def: &Definition,
    settings: &Settings,
) -> Result<(), LoginError> {
    let Some(login) = &def.login else {
        return Ok(());
    };
    if login.captcha.is_some() {
        return Err(LoginError::NeedsCaptcha);
    }

    match login.method.as_deref() {
        Some(m) if m.eq_ignore_ascii_case("post") => {
            submit_login(client, def, login, settings, Method::Post).await
        }
        Some(m) if m.eq_ignore_ascii_case("get") => {
            submit_login(client, def, login, settings, Method::Get).await
        }
        Some(m) if m.eq_ignore_ascii_case("cookie") => {
            cookie_login(client, def, login, settings).await
        }
        Some(m) if m.eq_ignore_ascii_case("form") => form_login(client, def, login, settings).await,
        None => form_login(client, def, login, settings).await,
        Some(other) => Err(LoginError::Definition(format!(
            "login method {other:?} not yet implemented"
        ))),
    }
}

/// Resolves `def.links`' first entry into a base URL, wrapping
/// [`resolve_base_url`]'s plain-string failure reason into a
/// [`LoginError::Definition`] naming the definition.
fn base_url(def: &Definition) -> Result<url::Url, LoginError> {
    resolve_base_url(def)
        .map_err(|reason| LoginError::Definition(format!("definition {}: {reason}", def.id)))
}

/// The `post`/`get` login flow: render `login.path`/`inputs`/`headers`,
/// submit, check for rejection, then verify the session when `login.test`
/// is set.
async fn submit_login<C: HttpClient>(
    client: &C,
    def: &Definition,
    login: &Login,
    settings: &Settings,
    method: Method,
) -> Result<(), LoginError> {
    let path = login.path.as_deref().ok_or_else(|| {
        LoginError::Definition(format!("login block for {} declares no path", def.id))
    })?;
    let base = base_url(def)?;
    let scope = scope_for(def, &SearchQuery::default(), settings);

    let rendered_path =
        template::render(path, &scope).map_err(|err| LoginError::Definition(err.to_string()))?;
    let mut url = base.join(&rendered_path).map_err(|err| {
        LoginError::Definition(format!("invalid login path {rendered_path:?}: {err}"))
    })?;

    let mut pairs = Vec::with_capacity(login.inputs.len());
    for (key, template_str) in &login.inputs {
        let rendered = template::render(template_str, &scope)
            .map_err(|err| LoginError::Definition(err.to_string()))?;
        pairs.push((key.clone(), rendered));
    }

    let headers = rendered_headers(&login.headers, &scope)?;

    let body = match method {
        Method::Post => Some(Body::Form(pairs)),
        Method::Get => {
            if !pairs.is_empty() {
                url.query_pairs_mut()
                    .extend_pairs(pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())));
            }
            None
        }
    };

    let req = HttpRequest {
        method,
        url,
        headers,
        body,
    };
    let resp = client.execute(req).await?;
    check_error_rules(&login.error, &resp.text())?;
    verify_test(client, &base, login.test.as_ref()).await
}

/// Renders `login.headers`' first value per name against `scope` — the same
/// "first value wins" convention `builder::build_request` applies to
/// `search.headers`.
fn rendered_headers(
    headers: &std::collections::BTreeMap<String, Vec<String>>,
    scope: &template::Scope,
) -> Result<Vec<(String, String)>, LoginError> {
    headers
        .iter()
        .filter_map(|(name, values)| values.first().map(|value| (name.clone(), value.clone())))
        .map(|(name, template_str)| {
            template::render(&template_str, scope)
                .map(|rendered| (name, rendered))
                .map_err(|err| LoginError::Definition(err.to_string()))
        })
        .collect()
}

/// The `cookie` login flow: no request of its own beyond confirming the
/// user actually configured a `cookie` setting value — the cookie itself is
/// injected into the transport elsewhere (see [`crate::ReqwestClient::with_cookie`]) —
/// then `login.test` verification when present.
async fn cookie_login<C: HttpClient>(
    client: &C,
    def: &Definition,
    login: &Login,
    settings: &Settings,
) -> Result<(), LoginError> {
    let resolved = settings.resolved(def);
    let cookie = resolved.get("cookie").map_or("", String::as_str);
    if cookie.trim().is_empty() {
        return Err(LoginError::Definition(
            "cookie login requires the 'cookie' setting".to_string(),
        ));
    }

    let base = base_url(def)?;
    verify_test(client, &base, login.test.as_ref()).await
}

/// Runs [`verify_login`] when `test` is set, turning a failed check into
/// [`LoginError::Rejected`]; `Ok(())` when there is no test to run.
async fn verify_test<C: HttpClient>(
    client: &C,
    base: &url::Url,
    test: Option<&LoginTest>,
) -> Result<(), LoginError> {
    let Some(test) = test else {
        return Ok(());
    };
    if verify_login(client, base, test).await? {
        Ok(())
    } else {
        Err(LoginError::Rejected("login test failed".to_string()))
    }
}

/// The scraped-hidden-input `form` login flow, and the method-less style
/// that relies on the same machinery (e.g. `postman.yml`; see
/// [`authenticate`]'s doc comment):
///
/// 1. GET `login.path` (resolved against `def.links`' first entry), with a
///    `Cookie` header built from `login.cookies` when that list is
///    non-empty (joined with `"; "`, matching Jackett's own
///    `string.Join("; ", Login.Cookies)`). `login.cookies` is a list of
///    *literal* cookie strings, unrelated to `method: cookie` auth — see
///    [`authenticate`]'s doc comment.
/// 2. Parse the response body and select the form via `login.form` (default
///    `"form"`); no match is [`LoginError::Definition`] naming the selector.
/// 3. Collect a baseline pair for every `input[name]` inside that form,
///    value = its `value` attribute (default `""`), in document order.
///    Unlike Jackett's `DoLogin`, this does not special-case disabled
///    inputs or unchecked checkboxes/radios — not needed by any definition
///    this task covers, and simpler to reason about.
/// 4. Overlay `login.selectorinputs`: each key is an input name, each value
///    a [`Field`] extracted against the *whole page* document (not just the
///    form) via [`evaluate_login_field`].
/// 5. Overlay rendered `login.inputs` (scope built the same way
///    `submit_login` builds one, via `scope_for(def, &SearchQuery::default(),
///    settings)`). When `login.selectors` is set, `login.inputs`' keys are
///    CSS selectors resolved (via [`resolve_input_name`]) to the matched
///    element's `name` attribute rather than being used as literal input
///    names.
///
///    Steps 4 and 5 run in the reverse of Jackett's own order (Jackett
///    overlays `Login.Inputs` before `Login.Selectorinputs`, so a scraped
///    value would win a key collision there). This flow overlays
///    `selectorinputs` first so rendered credentials always win a
///    collision — the two key sets never overlap in the corpus
///    (`selectorinputs` scrapes tokens, `inputs` renders credentials), so
///    this is not a behavioural difference for any real definition, just a
///    more defensive default.
/// 6. POST the resulting pairs as a [`Body::Form`] to the form's `action`
///    attribute (or `login.submitpath` when set) resolved against the
///    login page's redirect-*final* URL — not the originally-requested
///    login URL Jackett's C# literally resolves against
///    (`resolvePath(submitUrlstr, new Uri(loginUrl))`). A relative form
///    `action` is written by the page author relative to wherever the page
///    actually ended up being served from, which is what ordinary browsers
///    do too; this task's brief calls for that behaviour explicitly.
///    `login.headers` is rendered and sent on this request (plus the same
///    `Cookie` header as the GET, when `login.cookies` is set).
/// 7. [`check_error_rules`] on the POST response, then [`verify_login`] when
///    `login.test` is set — identical to `submit_login`'s tail.
///
/// # Errors
/// Returns [`LoginError::Definition`] for a missing/malformed `login.path`,
/// an unresolvable base link, a form/selector that fails to compile or
/// match (naming it), an unknown filter in a `selectorinputs` field, or a
/// `login.selectors` selector that matches no element / an element with no
/// `name` attribute. Returns [`LoginError::Rejected`] or
/// [`LoginError::Http`] from the same checks `submit_login` performs.
async fn form_login<C: HttpClient>(
    client: &C,
    def: &Definition,
    login: &Login,
    settings: &Settings,
) -> Result<(), LoginError> {
    let path = login.path.as_deref().ok_or_else(|| {
        LoginError::Definition(format!("login block for {} declares no path", def.id))
    })?;
    let base = base_url(def)?;
    let scope = scope_for(def, &SearchQuery::default(), settings);

    let rendered_path =
        template::render(path, &scope).map_err(|err| LoginError::Definition(err.to_string()))?;
    let login_url = base.join(&rendered_path).map_err(|err| {
        LoginError::Definition(format!("invalid login path {rendered_path:?}: {err}"))
    })?;

    let cookie = login_cookie_header(login);
    let get_req = HttpRequest {
        method: Method::Get,
        url: login_url,
        headers: cookie.clone().into_iter().collect(),
        body: None,
    };
    let landing = client.execute(get_req).await?;
    let body = landing.text().into_owned();

    // Everything `scraper`-typed (`Html`, `ElementRef`, compiled selectors)
    // is confined to this block so it is dropped before the POST `.await`
    // below. `ElementRef` borrows from a tree whose nodes hold `Cell`s, so
    // it is `!Sync` and therefore `!Send`; left in scope across an await
    // point it would make this whole async fn's future non-`Send` even
    // though nothing here is actually used afterward — which matters
    // because callers that must return a `Send` future (e.g.
    // `CardigannIndexer::search`) call `authenticate` directly.
    let (pairs, submit_url) = {
        let doc = Html::parse_document(&body);
        let root = doc.root_element();

        let form_selector_raw = login.form.as_deref().unwrap_or("form");
        let form_selector = compile_html_selector(form_selector_raw)?;
        let Some(form) = form_selector.select(root).into_iter().next() else {
            return Err(LoginError::Definition(format!(
                "no form found on the login page using form selector {form_selector_raw:?}"
            )));
        };

        let input_selector = compile_html_selector("input[name]")?;
        let mut pairs: Vec<(String, String)> = Vec::new();
        for input in input_selector.select(form) {
            let Some(name) = input.attr("name") else {
                continue;
            };
            let value = input.attr("value").unwrap_or_default().to_string();
            upsert(&mut pairs, name, value);
        }

        let field_ctx = FilterCtx {
            now: Utc::now(),
            keywords: Vec::new(),
        };
        for (name, field) in &login.selectorinputs {
            let value = evaluate_login_field(name, field, root, &field_ctx)?;
            upsert(&mut pairs, name, value);
        }

        for (key, template_str) in &login.inputs {
            let rendered = template::render(template_str, &scope)
                .map_err(|err| LoginError::Definition(err.to_string()))?;
            let name = resolve_input_name(login, key, root)?;
            upsert(&mut pairs, &name, rendered);
        }

        let action = form.attr("action").unwrap_or_default();
        let submit_path = login.submitpath.as_deref().unwrap_or(action);
        let submit_url = landing.final_url.join(submit_path).map_err(|err| {
            LoginError::Definition(format!("invalid form submit path {submit_path:?}: {err}"))
        })?;

        (pairs, submit_url)
    };

    let mut headers = rendered_headers(&login.headers, &scope)?;
    headers.extend(cookie);

    let post_req = HttpRequest {
        method: Method::Post,
        url: submit_url,
        headers,
        body: Some(Body::Form(pairs)),
    };
    let resp = client.execute(post_req).await?;
    check_error_rules(&login.error, &resp.text())?;
    verify_test(client, &base, login.test.as_ref()).await
}

/// Builds the `Cookie` header from `login.cookies`, when non-empty.
fn login_cookie_header(login: &Login) -> Option<(String, String)> {
    if login.cookies.is_empty() {
        None
    } else {
        Some(("Cookie".to_string(), login.cookies.join("; ")))
    }
}

/// Sets `name` to `value` in `pairs`, updating an existing entry in place
/// (preserving its original position) rather than appending a duplicate.
fn upsert(pairs: &mut Vec<(String, String)>, name: &str, value: String) {
    if let Some(entry) = pairs.iter_mut().find(|(k, _)| k == name) {
        entry.1 = value;
    } else {
        pairs.push((name.to_string(), value));
    }
}

/// Resolves one `login.inputs` key into the input name to set: the key
/// itself, or — when `login.selectors` is set — the `name` attribute of the
/// element `key` (a CSS selector) matches against the whole page.
fn resolve_input_name(
    login: &Login,
    key: &str,
    root: ElementRef<'_>,
) -> Result<String, LoginError> {
    if !login.selectors {
        return Ok(key.to_string());
    }
    let compiled = compile_html_selector(key)?;
    let Some(el) = compiled.select(root).into_iter().next() else {
        return Err(LoginError::Definition(format!(
            "login.inputs selector {key:?} matched no element (login.selectors is set)"
        )));
    };
    el.attr("name").map(str::to_string).ok_or_else(|| {
        LoginError::Definition(format!("element selected by {key:?} has no name attribute"))
    })
}

/// A minimal, login-scoped field extraction: `selector:` (+ optional
/// `attribute:`, defaulting to the element's text) or a literal `text:`,
/// then `filters:`, then `default:` when the result is still empty.
///
/// This is deliberately smaller than the search engine's field evaluation
/// (`oxidarr_cardigann::engine`'s private `evaluate_field`, not part of that
/// crate's public API): no `case:`/`remove:` support, and a `text:` is used
/// literally rather than template-rendered. Login pages in the corpus only
/// ever need selector+attribute (a CSRF token's `value`), so this covers
/// the real shape without depending on engine internals.
///
/// `name` is `login.selectorinputs`' key for `field` — used only to name it
/// in an error message, never as a selector or input name itself.
///
/// `ctx` is threaded in by the caller (rather than built here) so a single
/// [`FilterCtx`] is shared across every `selectorinputs` field in one login
/// (avoiding a redundant `Utc::now()` per field) and so tests can supply
/// one directly — see [`FilterOutcome::DropRow`]'s handling below.
///
/// A filter chain that yields [`FilterOutcome::DropRow`] (only `andmatch`
/// does this, and only when `ctx.keywords` is both non-empty and not fully
/// contained in the value) is a [`LoginError::Definition`] naming the
/// field: "drop this row" has no meaning outside search-result extraction,
/// and `form_login` always calls this with an empty `ctx.keywords` (a login
/// page has no search query), so `andmatch` can never actually produce
/// `DropRow` through the real flow — this mapping exists purely so a filter
/// chain that somehow does yield one fails loudly rather than silently
/// emitting an empty value (which a caller could easily mistake for "the
/// selector legitimately matched nothing").
///
/// # Errors
/// Returns [`LoginError::Definition`] if `field.selector` fails to compile,
/// if any of `field.filters` names a filter this engine does not implement
/// or with unusable arguments, or if a filter yields
/// [`FilterOutcome::DropRow`].
fn evaluate_login_field(
    name: &str,
    field: &Field,
    root: ElementRef<'_>,
    ctx: &FilterCtx,
) -> Result<String, LoginError> {
    let mut value = if let Some(text) = &field.text {
        text.clone()
    } else if let Some(sel) = &field.selector {
        let compiled = compile_html_selector(sel)?;
        match compiled.select(root).into_iter().next() {
            Some(el) => match &field.attribute {
                Some(attr) => el.attr(attr).unwrap_or_default().to_string(),
                None => el.text().collect::<String>(),
            },
            None => String::new(),
        }
    } else {
        String::new()
    };

    for spec in &field.filters {
        let filter = filters::parse_filter(&spec.name, &spec.args.to_vec())
            .map_err(|err| LoginError::Definition(err.to_string()))?;
        match filters::apply(&filter, &value, ctx)
            .map_err(|err| LoginError::Definition(err.to_string()))?
        {
            FilterOutcome::Value(v) => value = v,
            FilterOutcome::DropRow => {
                return Err(LoginError::Definition(format!(
                    "selectorinputs field {name:?}'s filter chain dropped it; \
                     a login field cannot be dropped"
                )));
            }
        }
    }

    if value.is_empty()
        && let Some(default) = &field.default
    {
        value.clone_from(default);
    }

    Ok(value)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::collections::BTreeMap;

    use url::Url;

    use crate::client::{Body, HttpResponse, Method};
    use crate::query::Settings;
    use crate::testing::{FakeClient, ok_html};

    use scraper::Html;

    use super::{
        FilterCtx, LoginError, authenticate, check_error_rules, evaluate_login_field, verify_login,
    };
    use oxidarr_cardigann::model::{
        Definition, ErrorRule, Field, FilterArgs, FilterSpec, LoginTest, parse_definition,
    };

    fn rule(selector: &str) -> ErrorRule {
        ErrorRule {
            selector: selector.to_string(),
            message: None,
        }
    }

    #[test]
    fn matching_rule_rejects_with_the_matched_elements_own_text() {
        let body = r#"<html><body><div class="error">Wrong password</div></body></html>"#;
        let err = check_error_rules(&[rule("div.error")], body).unwrap_err();
        assert!(matches!(err, LoginError::Rejected(msg) if msg == "Wrong password"));
    }

    #[test]
    fn clean_page_is_ok() {
        let body = r#"<html><body><div class="welcome">Hi there</div></body></html>"#;
        assert!(check_error_rules(&[rule("div.error")], body).is_ok());
    }

    #[test]
    fn no_rules_is_ok() {
        let body = r#"<html><body><div class="error">Wrong password</div></body></html>"#;
        assert!(check_error_rules(&[], body).is_ok());
    }

    #[test]
    fn first_matching_rule_wins() {
        let body = r#"<html><body><div class="error">Wrong password</div></body></html>"#;
        let rules = vec![rule("div.missing"), rule("div.error")];
        let err = check_error_rules(&rules, body).unwrap_err();
        assert!(matches!(err, LoginError::Rejected(msg) if msg == "Wrong password"));
    }

    #[test]
    fn message_selector_reads_a_nested_elements_text() {
        let body = r#"<html><body>
            <div class="error"><span class="msg">Bad credentials</span></div>
        </body></html>"#;
        let mut r = rule("div.error");
        r.message = Some(Field {
            selector: Some("span.msg".to_string()),
            ..Field::default()
        });
        let err = check_error_rules(&[r], body).unwrap_err();
        assert!(matches!(err, LoginError::Rejected(msg) if msg == "Bad credentials"));
    }

    #[test]
    fn message_text_is_used_literally() {
        let body = r#"<html><body><div class="error">ignored</div></body></html>"#;
        let mut r = rule("div.error");
        r.message = Some(Field {
            text: Some("Custom rejection message".to_string()),
            ..Field::default()
        });
        let err = check_error_rules(&[r], body).unwrap_err();
        assert!(matches!(err, LoginError::Rejected(msg) if msg == "Custom rejection message"));
    }

    #[test]
    fn jquery_contains_dialect_is_understood() {
        // The corpus's login.error rules lean heavily on jQuery's
        // `:contains()`, which `scraper`/standard CSS has no equivalent
        // for (e.g. 0daykiev.yml: `div.maintitle:contains("Ошибка")`).
        let body = r#"<html><body><div class="maintitle">Ошибка входа</div></body></html>"#;
        let err =
            check_error_rules(&[rule(r#"div.maintitle:contains("Ошибка")"#)], body).unwrap_err();
        assert!(matches!(err, LoginError::Rejected(msg) if msg == "Ошибка входа"));
    }

    #[test]
    fn unparseable_selector_is_a_definition_error() {
        let body = "<html></html>";
        let err = check_error_rules(&[rule("div:::bogus(")], body).unwrap_err();
        assert!(matches!(err, LoginError::Definition(_)));
    }

    fn status(status: u16, url: &str, body: &str) -> HttpResponse {
        HttpResponse {
            status,
            headers: vec![],
            body: body.as_bytes().to_vec(),
            final_url: url.parse().unwrap(),
        }
    }

    #[tokio::test]
    async fn verify_login_true_when_selector_matches() {
        let client = FakeClient::new().expect(
            |r| r.url.path() == "/account",
            ok_html(
                "https://t.example/account",
                r#"<a href="/logout">Logout</a>"#,
            ),
        );
        let base: Url = "https://t.example/".parse().unwrap();
        let test = LoginTest {
            path: "account".to_string(),
            selector: Some(r#"a[href="/logout"]"#.to_string()),
        };
        assert!(verify_login(&client, &base, &test).await.unwrap());
    }

    #[tokio::test]
    async fn verify_login_false_when_selector_element_is_missing() {
        let client = FakeClient::new().expect(
            |r| r.url.path() == "/account",
            ok_html("https://t.example/account", r"<p>please log in</p>"),
        );
        let base: Url = "https://t.example/".parse().unwrap();
        let test = LoginTest {
            path: "account".to_string(),
            selector: Some(r#"a[href="/logout"]"#.to_string()),
        };
        assert!(!verify_login(&client, &base, &test).await.unwrap());
    }

    #[tokio::test]
    async fn verify_login_true_on_2xx_when_no_selector_is_given() {
        let client = FakeClient::new().expect(
            |r| r.url.path() == "/account",
            ok_html("https://t.example/account", "<html></html>"),
        );
        let base: Url = "https://t.example/".parse().unwrap();
        let test = LoginTest {
            path: "account".to_string(),
            selector: None,
        };
        assert!(verify_login(&client, &base, &test).await.unwrap());
    }

    #[tokio::test]
    async fn verify_login_false_on_non_2xx_when_no_selector_is_given() {
        let client = FakeClient::new().expect(
            |r| r.url.path() == "/account",
            status(401, "https://t.example/account", "<html></html>"),
        );
        let base: Url = "https://t.example/".parse().unwrap();
        let test = LoginTest {
            path: "account".to_string(),
            selector: None,
        };
        assert!(!verify_login(&client, &base, &test).await.unwrap());
    }

    #[tokio::test]
    async fn verify_login_propagates_transport_errors() {
        let client = FakeClient::new();
        let base: Url = "https://t.example/".parse().unwrap();
        let test = LoginTest {
            path: "account".to_string(),
            selector: None,
        };
        assert!(matches!(
            verify_login(&client, &base, &test).await,
            Err(LoginError::Http(_))
        ));
    }

    fn parse(yaml: &str) -> Definition {
        parse_definition(yaml).unwrap()
    }

    fn settings_of(pairs: &[(&str, &str)]) -> Settings {
        let mut values = BTreeMap::new();
        for (key, value) in pairs {
            values.insert((*key).to_string(), (*value).to_string());
        }
        Settings::new(values)
    }

    const POST_LOGIN: &str = r#"
id: example
name: Example
links:
  - https://example.org/
settings:
  - name: username
    type: text
    label: Username
  - name: password
    type: password
    label: Password
login:
  path: takelogin.php
  method: post
  inputs:
    username: "{{ .Config.username }}"
    password: "{{ .Config.password }}"
  error:
    - selector: div.error
search:
  paths:
    - path: browse
  rows:
    selector: item
  fields:
    title:
      selector: a
"#;

    #[tokio::test]
    async fn post_flow_sends_rendered_credentials_as_a_form_body() {
        let def = parse(POST_LOGIN);
        let settings = settings_of(&[("username", "alice"), ("password", "hunter2")]);
        let client = FakeClient::new().expect(
            |r| r.method == Method::Post && r.url.path() == "/takelogin.php",
            ok_html("https://example.org/takelogin.php", "<html>ok</html>"),
        );

        authenticate(&client, &def, &settings).await.unwrap();

        let requests = client.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].body,
            Some(Body::Form(vec![
                ("password".to_string(), "hunter2".to_string()),
                ("username".to_string(), "alice".to_string()),
            ]))
        );
    }

    #[tokio::test]
    async fn post_flow_wrong_password_fixture_rejects_with_the_fixtures_message() {
        let def = parse(POST_LOGIN);
        let settings = settings_of(&[("username", "alice"), ("password", "wrong")]);
        let body =
            r#"<html><body><div class="error">Invalid username or password</div></body></html>"#;
        let client = FakeClient::new().expect(
            |r| r.url.path() == "/takelogin.php",
            ok_html("https://example.org/takelogin.php", body),
        );

        let err = authenticate(&client, &def, &settings).await.unwrap_err();

        assert!(matches!(
            err,
            LoginError::Rejected(msg) if msg == "Invalid username or password"
        ));
    }

    const GET_LOGIN: &str = r#"
id: example
name: Example
links:
  - https://example.org/
settings:
  - name: username
    type: text
    label: Username
  - name: password
    type: password
    label: Password
login:
  path: takelogin.php
  method: get
  inputs:
    username: "{{ .Config.username }}"
    password: "{{ .Config.password }}"
search:
  paths:
    - path: browse
  rows:
    selector: item
  fields:
    title:
      selector: a
"#;

    #[tokio::test]
    async fn get_flow_puts_inputs_in_the_query() {
        let def = parse(GET_LOGIN);
        let settings = settings_of(&[("username", "alice"), ("password", "hunter2")]);
        let client = FakeClient::new().expect(
            |r| r.method == Method::Get && r.url.query() == Some("password=hunter2&username=alice"),
            ok_html("https://example.org/takelogin.php", "<html></html>"),
        );

        authenticate(&client, &def, &settings).await.unwrap();

        assert_eq!(client.requests()[0].body, None);
    }

    const CAPTCHA_LOGIN: &str = r"
id: example
name: Example
links:
  - https://example.org/
login:
  path: takelogin.php
  method: post
  captcha:
    type: image
    selector: img.captcha
    input: captchaText
search:
  paths:
    - path: browse
  rows:
    selector: item
  fields:
    title:
      selector: a
";

    #[tokio::test]
    async fn captcha_present_is_needs_captcha_before_any_request() {
        let def = parse(CAPTCHA_LOGIN);
        let client = FakeClient::new();

        let err = authenticate(&client, &def, &Settings::default())
            .await
            .unwrap_err();

        assert!(matches!(err, LoginError::NeedsCaptcha));
        assert!(client.requests().is_empty());
    }

    const NO_LOGIN: &str = r"
id: example
name: Example
links:
  - https://example.org/
search:
  paths:
    - path: browse
  rows:
    selector: item
  fields:
    title:
      selector: a
";

    #[tokio::test]
    async fn no_login_block_is_ok_with_zero_requests() {
        let def = parse(NO_LOGIN);
        let client = FakeClient::new();

        let result = authenticate(&client, &def, &Settings::default()).await;

        assert!(result.is_ok());
        assert!(client.requests().is_empty());
    }

    const MISSING_PATH_LOGIN: &str = r#"
id: example
name: Example
links:
  - https://example.org/
login:
  method: post
  inputs:
    username: "{{ .Config.username }}"
search:
  paths:
    - path: browse
  rows:
    selector: item
  fields:
    title:
      selector: a
"#;

    #[tokio::test]
    async fn missing_login_path_is_a_definition_error() {
        let def = parse(MISSING_PATH_LOGIN);
        let client = FakeClient::new();

        let err = authenticate(&client, &def, &Settings::default())
            .await
            .unwrap_err();

        assert!(matches!(err, LoginError::Definition(_)));
        assert!(client.requests().is_empty());
    }

    const COOKIE_LOGIN_WITH_TEST: &str = r#"
id: example
name: Example
links:
  - https://example.org/
settings:
  - name: cookie
    type: text
    label: Cookie
login:
  method: cookie
  inputs:
    cookie: "{{ .Config.cookie }}"
  test:
    path: /
    selector: a[href="/account/"]
search:
  paths:
    - path: browse
  rows:
    selector: item
  fields:
    title:
      selector: a
"#;

    #[tokio::test]
    async fn cookie_flow_with_login_test_runs_verification() {
        let def = parse(COOKIE_LOGIN_WITH_TEST);
        let settings = settings_of(&[("cookie", "session=abc")]);
        let client = FakeClient::new().expect(
            |r| r.method == Method::Get && r.url.path() == "/",
            ok_html("https://example.org/", r#"<a href="/account/">Account</a>"#),
        );

        authenticate(&client, &def, &settings).await.unwrap();

        assert_eq!(client.requests().len(), 1);
    }

    const COOKIE_LOGIN_NO_TEST: &str = r#"
id: example
name: Example
links:
  - https://example.org/
settings:
  - name: cookie
    type: text
    label: Cookie
login:
  method: cookie
  inputs:
    cookie: "{{ .Config.cookie }}"
search:
  paths:
    - path: browse
  rows:
    selector: item
  fields:
    title:
      selector: a
"#;

    #[tokio::test]
    async fn cookie_flow_without_a_login_test_is_ok_with_zero_requests() {
        let def = parse(COOKIE_LOGIN_NO_TEST);
        let settings = settings_of(&[("cookie", "session=abc")]);
        let client = FakeClient::new();

        let result = authenticate(&client, &def, &settings).await;

        assert!(result.is_ok());
        assert!(client.requests().is_empty());
    }

    #[tokio::test]
    async fn cookie_login_without_a_configured_cookie_value_is_a_definition_error() {
        // Fix round 1, Important finding 3: a `method: cookie` definition
        // whose user never configured the `cookie` setting used to
        // silently succeed (when `login.test` is absent) rather than
        // surfacing that the login can never actually work.
        let def = parse(COOKIE_LOGIN_NO_TEST);
        let client = FakeClient::new();

        let err = authenticate(&client, &def, &Settings::default())
            .await
            .unwrap_err();

        assert!(matches!(err, LoginError::Definition(_)));
        assert!(client.requests().is_empty());
    }

    const POST_LOGIN_WITH_TEST: &str = r#"
id: example
name: Example
links:
  - https://example.org/
settings:
  - name: username
    type: text
    label: Username
  - name: password
    type: password
    label: Password
login:
  path: takelogin.php
  method: post
  inputs:
    username: "{{ .Config.username }}"
    password: "{{ .Config.password }}"
  test:
    path: account
    selector: a[href="/logout"]
search:
  paths:
    - path: browse
  rows:
    selector: item
  fields:
    title:
      selector: a
"#;

    #[tokio::test]
    async fn post_flow_runs_login_test_after_a_successful_submit() {
        let def = parse(POST_LOGIN_WITH_TEST);
        let settings = settings_of(&[("username", "alice"), ("password", "hunter2")]);
        let client = FakeClient::new()
            .expect(
                |r| r.url.path() == "/takelogin.php",
                ok_html("https://example.org/takelogin.php", "<html></html>"),
            )
            .expect(
                |r| r.url.path() == "/account",
                ok_html(
                    "https://example.org/account",
                    r#"<a href="/logout">Logout</a>"#,
                ),
            );

        authenticate(&client, &def, &settings).await.unwrap();

        assert_eq!(client.requests().len(), 2);
    }

    #[tokio::test]
    async fn post_flow_login_test_failure_is_rejected_with_a_dedicated_message() {
        let def = parse(POST_LOGIN_WITH_TEST);
        let settings = settings_of(&[("username", "alice"), ("password", "hunter2")]);
        let client = FakeClient::new()
            .expect(
                |r| r.url.path() == "/takelogin.php",
                ok_html("https://example.org/takelogin.php", "<html></html>"),
            )
            .expect(
                |r| r.url.path() == "/account",
                ok_html("https://example.org/account", "<p>please log in</p>"),
            );

        let err = authenticate(&client, &def, &settings).await.unwrap_err();

        assert!(matches!(err, LoginError::Rejected(msg) if msg == "login test failed"));
    }

    const SIMPLE_FORM_PAGE: &str =
        r#"<html><body><form action="/take_login"></form></body></html>"#;

    const FORM_LOGIN_MINIMAL: &str = r"
id: example
name: Example
links:
  - https://example.org/
login:
  path: login.php
  method: form
search:
  paths:
    - path: browse
  rows:
    selector: item
  fields:
    title:
      selector: a
";

    #[tokio::test]
    async fn form_flow_posts_to_the_forms_action_with_no_inputs_declared() {
        let def = parse(FORM_LOGIN_MINIMAL);
        let client = FakeClient::new()
            .expect(
                |r| r.method == Method::Get && r.url.path() == "/login.php",
                ok_html("https://example.org/login.php", SIMPLE_FORM_PAGE),
            )
            .expect(
                |r| r.method == Method::Post && r.url.path() == "/take_login",
                ok_html("https://example.org/take_login", "<html>ok</html>"),
            );

        authenticate(&client, &def, &Settings::default())
            .await
            .unwrap();

        assert_eq!(client.requests().len(), 2);
        assert_eq!(client.requests()[1].body, Some(Body::Form(vec![])));
    }

    const FORM_LOGIN_CUSTOM_SELECTOR: &str = r#"
id: example
name: Example
links:
  - https://example.org/
login:
  path: login.php
  method: form
  form: "form.login-form"
search:
  paths:
    - path: browse
  rows:
    selector: item
  fields:
    title:
      selector: a
"#;

    #[tokio::test]
    async fn form_flow_missing_form_selector_is_a_definition_error_naming_the_selector() {
        let def = parse(FORM_LOGIN_CUSTOM_SELECTOR);
        let client = FakeClient::new().expect(
            |r| r.url.path() == "/login.php",
            ok_html(
                "https://example.org/login.php",
                "<html><body><p>no form here</p></body></html>",
            ),
        );

        let err = authenticate(&client, &def, &Settings::default())
            .await
            .unwrap_err();

        assert!(matches!(err, LoginError::Definition(msg) if msg.contains("form.login-form")));
    }

    const FORM_LOGIN_WITH_SELECTORINPUTS: &str = r#"
id: example
name: Example
links:
  - https://example.org/
login:
  path: login.php
  method: form
  selectorinputs:
    csrf_token:
      selector: meta[name="csrf"]
      attribute: content
search:
  paths:
    - path: browse
  rows:
    selector: item
  fields:
    title:
      selector: a
"#;

    const SELECTORINPUTS_PAGE: &str = r#"<html><head><meta name="csrf" content="server-token"></head><body>
<form action="/take_login">
<input type="text" name="username" value="">
</form>
</body></html>"#;

    #[tokio::test]
    async fn form_flow_overlays_selectorinputs_scraped_from_the_whole_page() {
        // Extracted against the whole document, not just inside the form
        // (mirrors Jackett's `landingResultDocument.FirstElementChild`) —
        // `csrf_token` here isn't even a form input.
        let def = parse(FORM_LOGIN_WITH_SELECTORINPUTS);
        let client = FakeClient::new()
            .expect(
                |r| r.url.path() == "/login.php",
                ok_html("https://example.org/login.php", SELECTORINPUTS_PAGE),
            )
            .expect(
                |r| r.url.path() == "/take_login",
                ok_html("https://example.org/take_login", "<html>ok</html>"),
            );

        authenticate(&client, &def, &Settings::default())
            .await
            .unwrap();

        assert_eq!(
            client.requests()[1].body,
            Some(Body::Form(vec![
                ("username".to_string(), String::new()),
                ("csrf_token".to_string(), "server-token".to_string()),
            ]))
        );
    }

    const FORM_LOGIN_WITH_UNKNOWN_FILTER: &str = r#"
id: example
name: Example
links:
  - https://example.org/
login:
  path: login.php
  method: form
  selectorinputs:
    csrf_token:
      selector: input[name="csrf_token"]
      attribute: value
      filters:
        - name: not_a_real_filter
search:
  paths:
    - path: browse
  rows:
    selector: item
  fields:
    title:
      selector: a
"#;

    const SELECTORINPUTS_FILTER_PAGE: &str = r#"<html><body>
<form action="/take_login">
<input type="hidden" name="csrf_token" value="  padded-token  ">
</form>
</body></html>"#;

    #[tokio::test]
    async fn form_flow_unknown_selectorinputs_filter_is_a_definition_error_naming_it_before_any_post()
     {
        // The flow must abort before the POST — no partial/garbage submit
        // when a selectorinputs field can't even be evaluated.
        let def = parse(FORM_LOGIN_WITH_UNKNOWN_FILTER);
        let client = FakeClient::new().expect(
            |r| r.url.path() == "/login.php",
            ok_html("https://example.org/login.php", SELECTORINPUTS_FILTER_PAGE),
        );

        let err = authenticate(&client, &def, &Settings::default())
            .await
            .unwrap_err();

        assert!(matches!(err, LoginError::Definition(msg) if msg.contains("not_a_real_filter")));
        assert_eq!(client.requests().len(), 1, "only the GET should have run");
    }

    const FORM_LOGIN_WITH_SELECTORINPUTS_FILTER: &str = r#"
id: example
name: Example
links:
  - https://example.org/
login:
  path: login.php
  method: form
  selectorinputs:
    csrf_token:
      selector: input[name="csrf_token"]
      attribute: value
      filters:
        - name: trim
search:
  paths:
    - path: browse
  rows:
    selector: item
  fields:
    title:
      selector: a
"#;

    #[tokio::test]
    async fn form_flow_applies_selectorinputs_filters_to_the_scraped_value() {
        let def = parse(FORM_LOGIN_WITH_SELECTORINPUTS_FILTER);
        let client = FakeClient::new()
            .expect(
                |r| r.url.path() == "/login.php",
                ok_html("https://example.org/login.php", SELECTORINPUTS_FILTER_PAGE),
            )
            .expect(
                |r| r.url.path() == "/take_login",
                ok_html("https://example.org/take_login", "<html>ok</html>"),
            );

        authenticate(&client, &def, &Settings::default())
            .await
            .unwrap();

        assert_eq!(
            client.requests()[1].body,
            Some(Body::Form(vec![(
                "csrf_token".to_string(),
                "padded-token".to_string()
            )]))
        );
    }

    #[test]
    fn evaluate_login_field_maps_a_dropped_row_outcome_to_a_definition_error_naming_the_field() {
        // A login field's filter chain yielding `DropRow` is nonsense (a
        // login page has no "row" to drop) — pinned as a `Definition` error
        // naming the field rather than silently becoming an empty value.
        // `andmatch` is the only filter that can produce `DropRow`, and only
        // when `ctx.keywords` is non-empty and not fully present in the
        // value; `form_login` itself always builds a `FilterCtx` with empty
        // keywords (a login page has no search query), so this can never
        // actually fire through the real flow — this calls
        // `evaluate_login_field` directly with a constructed `FilterCtx` to
        // exercise the branch regardless.
        let doc = Html::parse_document(r#"<html><body><div id="token">abc</div></body></html>"#);
        let root = doc.root_element();
        let field = Field {
            selector: Some("#token".to_string()),
            filters: vec![FilterSpec {
                name: "andmatch".to_string(),
                args: FilterArgs::None,
            }],
            ..Field::default()
        };
        let ctx = FilterCtx {
            now: chrono::Utc::now(),
            keywords: vec!["missing-keyword".to_string()],
        };

        let err = evaluate_login_field("token", &field, root, &ctx).unwrap_err();

        assert!(matches!(err, LoginError::Definition(msg) if msg.contains("token")));
    }

    #[tokio::test]
    async fn form_flow_missing_action_attribute_resolves_to_the_login_pages_final_url() {
        // An absent `action` attribute (and, identically, an empty
        // `action=""`) leaves nothing to resolve against the base but the
        // empty string, which `Url::join` resolves back to the login page's
        // own (already redirect-final) URL, per RFC 3986 5.3's empty
        // relative-reference rule.
        let def = parse(FORM_LOGIN_MINIMAL);
        let client = FakeClient::new()
            .expect(
                |r| r.url.path() == "/login.php",
                ok_html(
                    "https://example.org/login.php",
                    "<html><body><form></form></body></html>",
                ),
            )
            .expect(
                |r| r.method == Method::Post && r.url.path() == "/login.php",
                ok_html("https://example.org/login.php", "<html>ok</html>"),
            );

        authenticate(&client, &def, &Settings::default())
            .await
            .unwrap();

        assert_eq!(client.requests().len(), 2);
        assert_eq!(
            client.requests()[1].url.as_str(),
            "https://example.org/login.php"
        );
    }

    const FORM_LOGIN_ORDERING: &str = r#"
id: example
name: Example
links:
  - https://example.org/
settings:
  - name: override
    type: text
    label: Override
login:
  path: login.php
  method: form
  selectorinputs:
    token:
      selector: input[name="token"]
      attribute: value
  inputs:
    token: "{{ .Config.override }}"
search:
  paths:
    - path: browse
  rows:
    selector: item
  fields:
    title:
      selector: a
"#;

    const ORDERING_PAGE: &str = r#"<html><body>
<form action="/take_login">
<input type="hidden" name="token" value="baseline-value">
</form>
</body></html>"#;

    #[tokio::test]
    async fn form_flow_rendered_login_inputs_overlay_selectorinputs_on_key_collision() {
        // Deliberately the reverse of Jackett's own overlay order — see
        // `form_login`'s doc comment for why.
        let def = parse(FORM_LOGIN_ORDERING);
        let settings = settings_of(&[("override", "from-config")]);
        let client = FakeClient::new()
            .expect(
                |r| r.url.path() == "/login.php",
                ok_html("https://example.org/login.php", ORDERING_PAGE),
            )
            .expect(
                |r| r.url.path() == "/take_login",
                ok_html("https://example.org/take_login", "<html>ok</html>"),
            );

        authenticate(&client, &def, &settings).await.unwrap();

        assert_eq!(
            client.requests()[1].body,
            Some(Body::Form(vec![(
                "token".to_string(),
                "from-config".to_string()
            )]))
        );
    }

    const FORM_LOGIN_SELECTORS_TRUE: &str = r#"
id: example
name: Example
links:
  - https://example.org/
settings:
  - name: username
    type: text
    label: Username
login:
  path: login.php
  method: form
  selectors: true
  inputs:
    "input.user-field": "{{ .Config.username }}"
search:
  paths:
    - path: browse
  rows:
    selector: item
  fields:
    title:
      selector: a
"#;

    const SELECTORS_PAGE: &str = r#"<html><body>
<form action="/take_login">
<input type="text" class="user-field" name="uname" value="">
</form>
</body></html>"#;

    #[tokio::test]
    async fn form_flow_selectors_true_resolves_input_keys_via_css_selector_to_the_elements_name() {
        let def = parse(FORM_LOGIN_SELECTORS_TRUE);
        let settings = settings_of(&[("username", "alice")]);
        let client = FakeClient::new()
            .expect(
                |r| r.url.path() == "/login.php",
                ok_html("https://example.org/login.php", SELECTORS_PAGE),
            )
            .expect(
                |r| r.url.path() == "/take_login",
                ok_html("https://example.org/take_login", "<html>ok</html>"),
            );

        authenticate(&client, &def, &settings).await.unwrap();

        assert_eq!(
            client.requests()[1].body,
            Some(Body::Form(vec![("uname".to_string(), "alice".to_string())]))
        );
    }

    const FORM_LOGIN_SUBMITPATH: &str = r"
id: example
name: Example
links:
  - https://example.org/
login:
  path: login.php
  method: form
  submitpath: /custom_submit
search:
  paths:
    - path: browse
  rows:
    selector: item
  fields:
    title:
      selector: a
";

    #[tokio::test]
    async fn form_flow_submitpath_overrides_the_forms_action() {
        let def = parse(FORM_LOGIN_SUBMITPATH);
        let client = FakeClient::new()
            .expect(
                |r| r.url.path() == "/login.php",
                ok_html("https://example.org/login.php", SIMPLE_FORM_PAGE),
            )
            .expect(
                |r| r.url.path() == "/custom_submit",
                ok_html("https://example.org/custom_submit", "<html>ok</html>"),
            );

        authenticate(&client, &def, &Settings::default())
            .await
            .unwrap();

        assert_eq!(client.requests()[1].url.path(), "/custom_submit");
    }

    const RELATIVE_ACTION_PAGE: &str =
        r#"<html><body><form action="relative_submit"></form></body></html>"#;

    #[tokio::test]
    async fn form_flow_resolves_the_action_against_the_login_pages_final_url_after_redirects() {
        // The GET response's `final_url` ("/sub/login.php") differs from the
        // originally-requested "/login.php" — as it would after a redirect
        // the transport already followed. A relative action must resolve
        // against the FINAL url ("/sub/relative_submit"), not the original
        // one ("/relative_submit") — see `form_login`'s doc comment for why
        // this differs from Jackett's own literal resolution base.
        let def = parse(FORM_LOGIN_MINIMAL);
        let client = FakeClient::new()
            .expect(
                |r| r.url.path() == "/login.php",
                ok_html("https://example.org/sub/login.php", RELATIVE_ACTION_PAGE),
            )
            .expect(
                |r| r.url.path() == "/sub/relative_submit",
                ok_html("https://example.org/sub/relative_submit", "<html>ok</html>"),
            );

        authenticate(&client, &def, &Settings::default())
            .await
            .unwrap();

        assert_eq!(client.requests().len(), 2);
    }

    const FORM_LOGIN_WITH_COOKIES: &str = r#"
id: example
name: Example
links:
  - https://example.org/
login:
  path: login.php
  method: form
  cookies: ["JAVA=OK", "lang=en"]
search:
  paths:
    - path: browse
  rows:
    selector: item
  fields:
    title:
      selector: a
"#;

    #[tokio::test]
    async fn form_flow_sends_login_cookies_as_a_cookie_header_on_both_requests() {
        let def = parse(FORM_LOGIN_WITH_COOKIES);
        let expected_cookie = ("Cookie".to_string(), "JAVA=OK; lang=en".to_string());
        let client = FakeClient::new()
            .expect(
                {
                    let expected = expected_cookie.clone();
                    move |r| r.url.path() == "/login.php" && r.headers.contains(&expected)
                },
                ok_html("https://example.org/login.php", SIMPLE_FORM_PAGE),
            )
            .expect(
                move |r| r.url.path() == "/take_login" && r.headers.contains(&expected_cookie),
                ok_html("https://example.org/take_login", "<html>ok</html>"),
            );

        authenticate(&client, &def, &Settings::default())
            .await
            .unwrap();

        assert_eq!(client.requests().len(), 2);
    }

    const FORM_LOGIN_WITH_ERROR_RULE: &str = r"
id: example
name: Example
links:
  - https://example.org/
login:
  path: login.php
  method: form
  error:
    - selector: div.error
search:
  paths:
    - path: browse
  rows:
    selector: item
  fields:
    title:
      selector: a
";

    #[tokio::test]
    async fn form_flow_check_error_rules_rejects_a_failed_submit() {
        let def = parse(FORM_LOGIN_WITH_ERROR_RULE);
        let client = FakeClient::new()
            .expect(
                |r| r.url.path() == "/login.php",
                ok_html("https://example.org/login.php", SIMPLE_FORM_PAGE),
            )
            .expect(
                |r| r.url.path() == "/take_login",
                ok_html(
                    "https://example.org/take_login",
                    r#"<html><body><div class="error">Bad credentials</div></body></html>"#,
                ),
            );

        let err = authenticate(&client, &def, &Settings::default())
            .await
            .unwrap_err();

        assert!(matches!(err, LoginError::Rejected(msg) if msg == "Bad credentials"));
    }

    const FORM_LOGIN_WITH_TEST: &str = r#"
id: example
name: Example
links:
  - https://example.org/
login:
  path: login.php
  method: form
  test:
    path: account
    selector: a[href="/logout"]
search:
  paths:
    - path: browse
  rows:
    selector: item
  fields:
    title:
      selector: a
"#;

    #[tokio::test]
    async fn form_flow_runs_login_test_after_a_successful_submit() {
        let def = parse(FORM_LOGIN_WITH_TEST);
        let client = FakeClient::new()
            .expect(
                |r| r.url.path() == "/login.php",
                ok_html("https://example.org/login.php", SIMPLE_FORM_PAGE),
            )
            .expect(
                |r| r.url.path() == "/take_login",
                ok_html("https://example.org/take_login", "<html>ok</html>"),
            )
            .expect(
                |r| r.url.path() == "/account",
                ok_html(
                    "https://example.org/account",
                    r#"<a href="/logout">Logout</a>"#,
                ),
            );

        authenticate(&client, &def, &Settings::default())
            .await
            .unwrap();

        assert_eq!(client.requests().len(), 3);
    }

    const METHODLESS_LOGIN: &str = r#"
id: example
name: Example
links:
  - https://example.org/
login:
  path: "index.php?view=Main"
  selectorinputs:
    formtoken:
      selector: input[name="formtoken"]
      attribute: value
  test:
    path: /
    selector: a[href^="index.php?view=Login"]
search:
  paths:
    - path: browse
  rows:
    selector: item
  fields:
    title:
      selector: a
"#;

    const METHODLESS_PAGE: &str = r#"<html><body>
<form action="take_login.php">
<input type="hidden" name="formtoken" value="server-token">
</form>
</body></html>"#;

    #[tokio::test]
    async fn methodless_login_routes_through_the_same_form_flow() {
        // e.g. postman.yml: no `method:` at all, relies on `selectorinputs`
        // + `test` instead — routed through `form_login` since it shares
        // every step (default `form` selector, baseline inputs,
        // `selectorinputs` overlay, action resolution). See
        // `authenticate`'s doc comment.
        let def = parse(METHODLESS_LOGIN);
        let client = FakeClient::new()
            .expect(
                |r| r.url.path() == "/index.php" && r.url.query() == Some("view=Main"),
                ok_html("https://example.org/index.php?view=Main", METHODLESS_PAGE),
            )
            .expect(
                |r| r.url.path() == "/take_login.php",
                ok_html("https://example.org/take_login.php", "<html>ok</html>"),
            )
            .expect(
                |r| r.method == Method::Get && r.url.path() == "/",
                ok_html(
                    "https://example.org/",
                    r#"<a href="index.php?view=Login">Login</a>"#,
                ),
            );

        authenticate(&client, &def, &Settings::default())
            .await
            .unwrap();

        assert_eq!(client.requests().len(), 3);
    }

    const UNKNOWN_METHOD_LOGIN: &str = r"
id: example
name: Example
links:
  - https://example.org/
login:
  path: takelogin.php
  method: telepathy
search:
  paths:
    - path: browse
  rows:
    selector: item
  fields:
    title:
      selector: a
";

    #[tokio::test]
    async fn unknown_login_method_is_a_definition_error_naming_the_method() {
        // Fix round 1, Important finding 2: an arbitrary/typo'd
        // `login.method` value had no dedicated test even though the
        // `Some(other)` catch-all already produced the right error.
        let def = parse(UNKNOWN_METHOD_LOGIN);
        let client = FakeClient::new();

        let err = authenticate(&client, &def, &Settings::default())
            .await
            .unwrap_err();

        assert!(matches!(err, LoginError::Definition(msg) if msg.contains("telepathy")));
        assert!(client.requests().is_empty());
    }

    const POST_LOGIN_UPPERCASE_METHOD: &str = r#"
id: example
name: Example
links:
  - https://example.org/
settings:
  - name: username
    type: text
    label: Username
  - name: password
    type: password
    label: Password
login:
  path: takelogin.php
  method: POST
  inputs:
    username: "{{ .Config.username }}"
    password: "{{ .Config.password }}"
search:
  paths:
    - path: browse
  rows:
    selector: item
  fields:
    title:
      selector: a
"#;

    #[tokio::test]
    async fn login_method_matching_is_case_insensitive() {
        let def = parse(POST_LOGIN_UPPERCASE_METHOD);
        let settings = settings_of(&[("username", "alice"), ("password", "hunter2")]);
        let client = FakeClient::new().expect(
            |r| r.method == Method::Post && r.url.path() == "/takelogin.php",
            ok_html("https://example.org/takelogin.php", "<html>ok</html>"),
        );

        authenticate(&client, &def, &settings).await.unwrap();

        assert_eq!(client.requests().len(), 1);
    }

    #[tokio::test]
    async fn post_flow_check_error_rules_fires_regardless_of_http_status() {
        // Fix round 1, cheap Minor: check_error_rules must fire on the
        // response body content alone, not gated on a 2xx status — a
        // rejected login often comes back as a non-2xx status alongside
        // the same error markup.
        let def = parse(POST_LOGIN);
        let settings = settings_of(&[("username", "alice"), ("password", "wrong")]);
        let body =
            r#"<html><body><div class="error">Invalid username or password</div></body></html>"#;
        let client = FakeClient::new().expect(
            |r| r.url.path() == "/takelogin.php",
            status(403, "https://example.org/takelogin.php", body),
        );

        let err = authenticate(&client, &def, &settings).await.unwrap_err();

        assert!(matches!(
            err,
            LoginError::Rejected(msg) if msg == "Invalid username or password"
        ));
    }
}
