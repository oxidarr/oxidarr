//! Login flow orchestration, failure detection, and session verification.
//!
//! [`authenticate`] dispatches on a definition's `login.method` and drives
//! the post/get/cookie flows on top of two independent checks: whether a
//! login response indicates the credentials were rejected
//! ([`check_error_rules`]), and whether an existing session is still
//! authenticated ([`verify_login`]). The scraped-hidden-input `form` flow
//! (and the method-less `selectorinputs` style, e.g. `postman.yml`) is
//! Task 14's seam — [`authenticate`] returns a placeholder
//! [`LoginError::Definition`] for both until then.

use scraper::{ElementRef, Html};

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
/// - Anything else — `method: form`, or no `method` at all (the
///   scraped-hidden-input `selectorinputs` style with no explicit `form:`,
///   e.g. `postman.yml`) — is not yet implemented: returns
///   [`LoginError::Definition`] naming the gap. Task 14 replaces both arms.
///   Note `login.cookies: Vec<String>` (e.g. `coastalcrew.yml`:
///   `cookies: ["JAVA=OK"]`) is an *unrelated* field — a list of literal
///   cookie strings to set on the client before requesting the login page
///   at all, always paired with `method: form` in the corpus, never a
///   marker for the `cookie`-auth style above. Task 14's form flow is
///   responsible for setting those cookies before its initial GET; nothing
///   here reads `login.cookies`.
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
        Some(m) if m.eq_ignore_ascii_case("form") => Err(LoginError::Definition(
            "form login not yet implemented".to_string(),
        )),
        None => Err(LoginError::Definition(
            "login block has no method (scraped selectorinputs login) is not yet implemented"
                .to_string(),
        )),
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::collections::BTreeMap;

    use url::Url;

    use crate::client::{Body, HttpResponse, Method};
    use crate::query::Settings;
    use crate::testing::{FakeClient, ok_html};

    use super::{LoginError, authenticate, check_error_rules, verify_login};
    use oxidarr_cardigann::model::{Definition, ErrorRule, Field, LoginTest, parse_definition};

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

    const FORM_LOGIN: &str = r#"
id: example
name: Example
links:
  - https://example.org/
login:
  path: haustuer.php
  method: form
  form: form[action="haustuer.php"]
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
    async fn form_method_is_an_unimplemented_placeholder_for_task_14() {
        let def = parse(FORM_LOGIN);
        let client = FakeClient::new();

        let err = authenticate(&client, &def, &Settings::default())
            .await
            .unwrap_err();

        assert!(matches!(err, LoginError::Definition(msg) if msg.contains("form login")));
        assert!(client.requests().is_empty());
    }

    const METHODLESS_LOGIN_NO_COOKIES: &str = r#"
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

    #[tokio::test]
    async fn methodless_login_without_cookies_is_an_unimplemented_placeholder() {
        // e.g. postman.yml: no `method:` at all, relies on
        // `selectorinputs` instead — Task 14's territory, not the
        // `cookie`-injection style this task implements. `login.cookies`
        // (a literal pre-request cookie list, unrelated to `method: cookie`
        // auth) plays no part in this dispatch any more — see
        // `unknown_login_method_is_a_definition_error_naming_the_method`
        // and `authenticate`'s doc comment for why.
        let def = parse(METHODLESS_LOGIN_NO_COOKIES);
        let client = FakeClient::new();

        let err = authenticate(&client, &def, &Settings::default())
            .await
            .unwrap_err();

        assert!(matches!(err, LoginError::Definition(_)));
        assert!(client.requests().is_empty());
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
