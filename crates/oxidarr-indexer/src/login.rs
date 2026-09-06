//! Login failure detection and session verification.
//!
//! Two independent checks that Tasks 13-14 build the login flow on top of:
//! whether a login response indicates the credentials were rejected
//! ([`check_error_rules`]), and whether an existing session is still
//! authenticated ([`verify_login`]).

use scraper::{ElementRef, Html};

use oxidarr_cardigann::model::{ErrorRule, Field, LoginTest};
use oxidarr_cardigann::selector::{self, CompiledSelector};

use crate::client::{HttpClient, HttpRequest, Method};
use crate::error::HttpError;

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
// Not yet called by anything in this crate: Tasks 13-14 wire this into the
// login POST flow, checking a submitted login form's response against
// `login.error`. `verify_login` below is a separate check (session/`login.test`)
// with no need for it.
#[allow(dead_code)]
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
// Only reachable from `check_error_rules`, which itself has no caller yet
// (see its own `#[allow(dead_code)]`).
#[allow(dead_code)]
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use url::Url;

    use crate::client::HttpResponse;
    use crate::testing::{FakeClient, ok_html};

    use super::{LoginError, check_error_rules, verify_login};
    use oxidarr_cardigann::model::{ErrorRule, Field, LoginTest};

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
}
