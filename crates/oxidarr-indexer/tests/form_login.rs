//! Integration-style test for Task 14's form login flow, against the
//! hand-written fixture in `tests/fixtures/login_form.html` (a hidden CSRF
//! input plus empty user/pass fields, `<form action="/take_login">`).
#![allow(clippy::unwrap_used)]

use oxidarr_cardigann::model::parse_definition;
use oxidarr_indexer::testing::{FakeClient, ok_html};
use oxidarr_indexer::{Body, Method, Settings, authenticate};

const LOGIN_FORM_HTML: &str = include_str!("fixtures/login_form.html");

const DEFINITION: &str = r#"
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
  path: login.php
  method: form
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

fn settings() -> Settings {
    let mut values = std::collections::BTreeMap::new();
    values.insert("username".to_string(), "alice".to_string());
    values.insert("password".to_string(), "hunter2".to_string());
    Settings::new(values)
}

#[tokio::test]
async fn form_login_scrapes_csrf_posts_credentials_then_verifies_the_session() {
    let def = parse_definition(DEFINITION).unwrap();
    let client = FakeClient::new()
        .expect(
            |r| r.method == Method::Get && r.url.path() == "/login.php",
            ok_html("https://example.org/login.php", LOGIN_FORM_HTML),
        )
        .expect(
            |r| {
                r.method == Method::Post
                    && r.url.path() == "/take_login"
                    && r.body
                        == Some(Body::Form(vec![
                            ("csrf_token".to_string(), "scraped-csrf-value".to_string()),
                            ("username".to_string(), "alice".to_string()),
                            ("password".to_string(), "hunter2".to_string()),
                            ("submit".to_string(), "Login".to_string()),
                        ]))
            },
            ok_html("https://example.org/take_login", "<html>ok</html>"),
        )
        .expect(
            |r| r.method == Method::Get && r.url.path() == "/account",
            ok_html(
                "https://example.org/account",
                r#"<a href="/logout">Logout</a>"#,
            ),
        );

    authenticate(&client, &def, &settings()).await.unwrap();

    let requests = client.requests();
    assert_eq!(requests.len(), 3, "expected GET, POST, then login.test GET");
    assert_eq!(requests[0].method, Method::Get);
    assert_eq!(requests[0].url.path(), "/login.php");
    assert_eq!(requests[1].method, Method::Post);
    assert_eq!(requests[1].url.path(), "/take_login");
    assert_eq!(requests[2].method, Method::Get);
    assert_eq!(requests[2].url.path(), "/account");
}
