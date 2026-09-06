//! Builds [`HttpRequest`]s from a definition's `search.paths`.
//!
//! This is the bridge from a parsed [`Definition`] plus one [`SearchQuery`]
//! into the concrete requests a search actually issues: one per reachable
//! path, each with its templated path, inputs, and headers rendered against
//! a single shared [`Scope`] built by [`scope_for`].

use std::collections::BTreeMap;

use oxidarr_cardigann::model::{Definition, Search, SearchPath};
use oxidarr_cardigann::template::{self, Scope};
use url::Url;

use crate::client::{Body, HttpRequest, Method};
use crate::error::IndexerError;
use crate::query::{SearchQuery, Settings, scope_for};

/// Builds one [`HttpRequest`] per reachable entry in `def.search.paths`.
///
/// A path is reachable when it declares no `categories:` restriction, when
/// `q.categories` is empty, or when the two lists intersect — Prowlarr's
/// simplified gating rule, not Jackett's own (which additionally maps
/// Newznab category ids through `caps.categorymappings` before comparing;
/// that mapping layer is out of scope here, so `q.categories` is compared
/// directly against `path.categories` as decimal strings).
///
/// Every reachable path renders against the *same* [`Scope`], built once via
/// [`scope_for`] — a path's `categories:` restriction does not narrow
/// `.Categories` the way Jackett's per-path intersection does, since
/// `scope_for`'s signature (fixed by Task 9) takes no path argument.
///
/// A path's own `inputs:` overlay `search.inputs` (path wins on key
/// collision); the model has no `inheritinputs`-style opt-out field, so
/// every path always inherits. Each merged input's value is template
/// rendered; a rendered value that is empty (or whitespace-only) is dropped
/// unless `search.allowEmptyInputs` is set (mirrors Jackett's
/// `CardigannIndexer.PerformQuery`). For GET, `$raw`'s rendered value is
/// appended verbatim to the request's query string — not parsed into
/// sub-pairs and not percent-encoded again. For POST, there is no query
/// string to append it to, so it is instead parsed into `key=value`
/// sub-pairs (percent-decoded, matching what `Body::Form` expects since
/// `reqwest`'s `.form()` re-encodes at send time) and folded into the form
/// body alongside the other inputs — mirroring how Jackett itself handles a
/// POST path's `$raw` (see `build_request`'s doc comment for the source
/// line and corpus examples).
///
/// GET requests carry inputs as URL query pairs (percent-encoded by
/// [`url::Url`]); POST requests carry them as a [`Body::Form`]. `method`
/// defaults to GET when a path does not declare one.
///
/// The request URL is `def.links`' first entry (normalised to end with `/`,
/// as Jackett does) joined with the rendered path via [`Url::join`], whose
/// RFC 3986 relative-resolution semantics already give the needed
/// "absolute-with-scheme wins wholesale" / leading-slash / relative-path
/// behaviour for free.
///
/// `search.headers`' first value per name (Jackett never reads more than
/// one) is rendered against the same scope and set on every request.
///
/// # Errors
///
/// Returns [`IndexerError::RequestBuild`] naming `def.id` if `def.links` is
/// empty, if the first link does not parse as a URL, if a path/input/header
/// template fails to render, or if joining the rendered path onto the base
/// URL does not parse.
pub fn build_search_requests(
    def: &Definition,
    q: &SearchQuery,
    s: &Settings,
) -> Result<Vec<HttpRequest>, IndexerError> {
    let scope = scope_for(def, q, s);
    let base = base_url(def)?;

    effective_paths(&def.search)
        .iter()
        .filter(|path| is_reachable(path, q))
        .map(|path| build_request(def, &def.search, path, &base, &scope))
        .collect()
}

/// `search.paths`, or — when that list is empty — a single-element list
/// built from `search.path`'s shorthand form (e.g. Bittorrentfiles.yml,
/// houseofdevil.yml: a bare `path:` instead of a one-entry `paths:` list).
fn effective_paths(search: &Search) -> Vec<SearchPath> {
    if !search.paths.is_empty() {
        return search.paths.clone();
    }
    search
        .path
        .as_ref()
        .map(|path| {
            vec![SearchPath {
                path: path.clone(),
                method: None,
                inputs: BTreeMap::new(),
                categories: Vec::new(),
                response: None,
                followredirect: false,
            }]
        })
        .unwrap_or_default()
}

fn request_build_error(def: &Definition, reason: impl Into<String>) -> IndexerError {
    IndexerError::RequestBuild {
        definition: def.id.clone(),
        reason: reason.into(),
    }
}

/// Resolves `def.links`' first entry into a base URL, normalised to end
/// with `/` (matching Jackett). Returns the failure reason as a plain
/// string rather than an [`IndexerError`] so [`crate::login::authenticate`]
/// can reuse this same base-URL convention while wrapping the reason in its
/// own [`crate::login::LoginError`].
pub(crate) fn resolve_base_url(def: &Definition) -> Result<Url, String> {
    let link = def
        .links
        .first()
        .ok_or_else(|| "definition declares no links".to_string())?;
    let normalised = if link.ends_with('/') {
        link.clone()
    } else {
        format!("{link}/")
    };
    Url::parse(&normalised).map_err(|err| format!("invalid base link {link:?}: {err}"))
}

fn base_url(def: &Definition) -> Result<Url, IndexerError> {
    resolve_base_url(def).map_err(|reason| request_build_error(def, reason))
}

/// Whether `path` should be included for a search with `q`'s categories.
fn is_reachable(path: &SearchPath, q: &SearchQuery) -> bool {
    if path.categories.is_empty() || q.categories.is_empty() {
        return true;
    }
    q.categories
        .iter()
        .any(|c| path.categories.iter().any(|pc| *pc == c.to_string()))
}

/// Overlays `path.inputs` onto `search.inputs`, path winning on collision.
fn merged_inputs(search: &Search, path: &SearchPath) -> BTreeMap<String, String> {
    let mut merged = search.inputs.clone();
    for (key, value) in &path.inputs {
        merged.insert(key.clone(), value.clone());
    }
    merged
}

/// One template-rendered input, split into a "goes in the query pairs /
/// form body" pair or a verbatim `$raw` fragment.
enum RenderedInput {
    Pair(String, String),
    Raw(String),
    /// Dropped: empty and `search.allowEmptyInputs` is not set.
    Empty,
}

fn render_input(
    def: &Definition,
    key: &str,
    template_str: &str,
    scope: &Scope,
    allow_empty: bool,
) -> Result<RenderedInput, IndexerError> {
    let rendered = template::render(template_str, scope)
        .map_err(|err| request_build_error(def, err.to_string()))?;
    if key == "$raw" {
        return Ok(if rendered.is_empty() {
            RenderedInput::Empty
        } else {
            RenderedInput::Raw(rendered)
        });
    }
    if rendered.trim().is_empty() && !allow_empty {
        return Ok(RenderedInput::Empty);
    }
    Ok(RenderedInput::Pair(key.to_string(), rendered))
}

fn build_request(
    def: &Definition,
    search: &Search,
    path: &SearchPath,
    base: &Url,
    scope: &Scope,
) -> Result<HttpRequest, IndexerError> {
    let rendered_path = template::render(&path.path, scope)
        .map_err(|err| request_build_error(def, err.to_string()))?;
    let mut url = base.join(&rendered_path).map_err(|err| {
        request_build_error(def, format!("invalid path {rendered_path:?}: {err}"))
    })?;

    let method = match path.method.as_deref() {
        Some(m) if m.eq_ignore_ascii_case("post") => Method::Post,
        _ => Method::Get,
    };

    let inputs = merged_inputs(search, path);
    let mut pairs = Vec::new();
    let mut raw_fragments = Vec::new();
    for (key, template_str) in &inputs {
        match render_input(def, key, template_str, scope, search.allow_empty_inputs)? {
            RenderedInput::Pair(k, v) => pairs.push((k, v)),
            // GET has somewhere verbatim to put a `$raw` fragment (the URL's
            // query string — see below). POST does not: there is no query
            // string, only `Body::Form`'s structured pairs, and folding the
            // whole fragment in as one opaque field would corrupt the form.
            // Jackett's own POST path (CardigannIndexer.cs's PerformQuery,
            // ~line 1531-1544) parses `$raw` into key/value sub-pairs and
            // folds *those* into the very same collection used for the POST
            // body, so mirror that instead of dropping the fragment (9
            // corpus definitions, e.g. audionews.yml, rely on `$raw` for
            // category filtering on a POST path). `url::form_urlencoded`
            // gives the same split-on-`&`-then-first-`=` plus percent-decode
            // Jackett's manual parsing does; a segment whose key half is
            // empty (the trailing `&` a `range` template typically leaves
            // behind) is skipped, matching Jackett's `key.Length == 0`
            // check.
            RenderedInput::Raw(raw) => match method {
                Method::Get => raw_fragments.push(raw),
                Method::Post => {
                    for (k, v) in url::form_urlencoded::parse(raw.as_bytes()) {
                        if !k.is_empty() {
                            pairs.push((k.into_owned(), v.into_owned()));
                        }
                    }
                }
            },
            RenderedInput::Empty => {}
        }
    }

    let body = match method {
        Method::Get => {
            if !pairs.is_empty() {
                url.query_pairs_mut()
                    .extend_pairs(pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())));
            }
            if !raw_fragments.is_empty() {
                let mut query = url.query().unwrap_or("").to_string();
                for fragment in &raw_fragments {
                    if !query.is_empty() {
                        query.push('&');
                    }
                    query.push_str(fragment);
                }
                url.set_query(Some(&query));
            }
            None
        }
        Method::Post => Some(Body::Form(pairs)),
    };

    let headers = search
        .headers
        .iter()
        .filter_map(|(name, values)| values.first().map(|value| (name.clone(), value.clone())))
        .map(|(name, value_template)| {
            template::render(&value_template, scope)
                .map(|rendered| (name, rendered))
                .map_err(|err| request_build_error(def, err.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(HttpRequest {
        method,
        url,
        headers,
        body,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use oxidarr_cardigann::model::parse_definition;

    fn parse(yaml: &str) -> Definition {
        parse_definition(yaml).unwrap()
    }

    fn query(q: &str, categories: Vec<u32>) -> SearchQuery {
        SearchQuery {
            q: Some(q.to_string()),
            categories,
            ..SearchQuery::default()
        }
    }

    const MINIMAL: &str = r#"
id: example
name: Example
links:
  - https://example.org/
search:
  paths:
    - path: browse
  inputs:
    q: "{{ .Keywords }}"
    cat: "{{ join .Categories \",\" }}"
  rows:
    selector: item
  fields:
    title:
      selector: a
"#;

    #[test]
    fn minimal_get_definition_renders_query_pairs_from_inputs() {
        let def = parse(MINIMAL);
        let q = query("ubuntu", vec![200]);

        let requests = build_search_requests(&def, &q, &Settings::default()).unwrap();

        assert_eq!(requests.len(), 1);
        let req = &requests[0];
        assert_eq!(req.method, Method::Get);
        assert_eq!(req.body, None);
        // BTreeMap-ordered inputs ("cat" < "q"), not the brief's illustrative
        // "q" then "cat" — see the task report.
        assert_eq!(
            req.url.as_str(),
            "https://example.org/browse?cat=200&q=ubuntu"
        );
    }

    const POST_DEF: &str = r#"
id: example
name: Example
links:
  - https://example.org/
search:
  paths:
    - path: login
      method: post
  inputs:
    q: "{{ .Keywords }}"
  rows:
    selector: item
  fields:
    title:
      selector: a
"#;

    #[test]
    fn a_post_path_yields_a_form_body_and_no_url_query() {
        let def = parse(POST_DEF);
        let q = query("ubuntu", vec![]);

        let requests = build_search_requests(&def, &q, &Settings::default()).unwrap();

        assert_eq!(requests.len(), 1);
        let req = &requests[0];
        assert_eq!(req.method, Method::Post);
        assert_eq!(req.url.as_str(), "https://example.org/login");
        assert_eq!(
            req.body,
            Some(Body::Form(vec![("q".to_string(), "ubuntu".to_string())]))
        );
    }

    const TWO_PATHS: &str = r#"
id: example
name: Example
links:
  - https://example.org/
search:
  paths:
    - path: browse
    - path: xxx
      categories: ["600"]
  inputs:
    q: "{{ .Keywords }}"
  rows:
    selector: item
  fields:
    title:
      selector: a
"#;

    #[test]
    fn a_category_gated_path_is_excluded_when_query_categories_do_not_intersect() {
        let def = parse(TWO_PATHS);
        let q = query("ubuntu", vec![200]);

        let requests = build_search_requests(&def, &q, &Settings::default()).unwrap();

        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].url.path(), "/browse");
    }

    #[test]
    fn a_category_gated_path_is_included_when_query_categories_intersect() {
        let def = parse(TWO_PATHS);
        let q = query("ubuntu", vec![600]);

        let requests = build_search_requests(&def, &q, &Settings::default()).unwrap();

        assert_eq!(requests.len(), 2);
    }

    #[test]
    fn a_category_gated_path_is_included_when_query_categories_are_empty() {
        let def = parse(TWO_PATHS);
        let q = query("ubuntu", vec![]);

        let requests = build_search_requests(&def, &q, &Settings::default()).unwrap();

        assert_eq!(requests.len(), 2);
    }

    const BAD_BASE: &str = r#"
id: example
name: Example
links:
  - "not a valid url"
search:
  paths:
    - path: browse
  rows:
    selector: item
  fields:
    title:
      selector: a
"#;

    #[test]
    fn an_unparseable_base_link_is_a_request_build_error_naming_the_definition() {
        let def = parse(BAD_BASE);
        let q = query("ubuntu", vec![]);

        let err = build_search_requests(&def, &q, &Settings::default()).unwrap_err();

        let IndexerError::RequestBuild { definition, .. } = err else {
            unreachable!("expected RequestBuild, got {err:?}");
        };
        assert_eq!(definition, "example");
    }

    #[test]
    fn no_links_at_all_is_a_request_build_error() {
        let def = parse(
            r"
id: example
name: Example
search:
  paths:
    - path: browse
  rows:
    selector: item
  fields:
    title:
      selector: a
",
        );
        let q = query("ubuntu", vec![]);

        let err = build_search_requests(&def, &q, &Settings::default()).unwrap_err();

        assert!(matches!(err, IndexerError::RequestBuild { .. }));
    }

    const RAW_DEF: &str = r#"
id: example
name: Example
links:
  - https://example.org/
search:
  paths:
    - path: browse
  inputs:
    $raw: "{{ range .Categories }}c{{.}}=1&{{end}}"
    q: "{{ .Keywords }}"
  rows:
    selector: item
  fields:
    title:
      selector: a
"#;

    #[test]
    fn raw_input_is_appended_verbatim_to_the_query_string() {
        let def = parse(RAW_DEF);
        let q = query("ubuntu", vec![200, 300]);

        let requests = build_search_requests(&def, &q, &Settings::default()).unwrap();

        assert_eq!(requests.len(), 1);
        // Normal pairs are appended first, then raw fragments verbatim
        // (including the trailing "&" the range template itself produces) —
        // see build_request's doc comment for why raw is handled last.
        assert_eq!(
            requests[0].url.as_str(),
            "https://example.org/browse?q=ubuntu&c200=1&c300=1&"
        );
    }

    const RAW_POST_DEF: &str = r#"
id: example
name: Example
links:
  - https://example.org/
search:
  paths:
    - path: takelogin.php
      method: post
  inputs:
    $raw: "{{ if .Categories }}{{ range .Categories }}f[]={{.}}&{{end}}{{ else }}f[]=-1{{ end }}"
    search: "{{ .Keywords }}"
  rows:
    selector: item
  fields:
    title:
      selector: a
"#;

    #[test]
    fn raw_input_on_a_post_path_is_parsed_into_form_pairs_not_dropped() {
        // The real shape (e.g. audionews.yml): a POST path whose $raw does
        // category filtering. Jackett parses $raw into key/value sub-pairs
        // and folds them into the very same collection used for the POST
        // body (CardigannIndexer.cs's PerformQuery, ~line 1531-1544), so
        // dropping it here would issue an unfiltered POST for real
        // definitions — this proves it doesn't.
        let def = parse(RAW_POST_DEF);
        let q = query("ubuntu", vec![200, 300]);

        let requests = build_search_requests(&def, &q, &Settings::default()).unwrap();

        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].url.as_str(),
            "https://example.org/takelogin.php"
        );
        assert_eq!(
            requests[0].body,
            Some(Body::Form(vec![
                ("f[]".to_string(), "200".to_string()),
                ("f[]".to_string(), "300".to_string()),
                ("search".to_string(), "ubuntu".to_string()),
            ]))
        );
    }

    const OVERLAY_DEF: &str = r#"
id: example
name: Example
links:
  - https://example.org/
search:
  inputs:
    q: "{{ .Keywords }}"
    sort: "created"
  paths:
    - path: browse
      inputs:
        sort: "seeders"
  rows:
    selector: item
  fields:
    title:
      selector: a
"#;

    #[test]
    fn path_inputs_overlay_search_inputs_on_key_collision() {
        let def = parse(OVERLAY_DEF);
        let q = query("ubuntu", vec![]);

        let requests = build_search_requests(&def, &q, &Settings::default()).unwrap();

        assert_eq!(
            requests[0].url.as_str(),
            "https://example.org/browse?q=ubuntu&sort=seeders"
        );
    }

    #[test]
    fn empty_rendered_inputs_are_dropped_unless_allow_empty_inputs_is_set() {
        let def = parse(
            r#"
id: example
name: Example
links:
  - https://example.org/
search:
  paths:
    - path: browse
  inputs:
    q: "{{ .Keywords }}"
    imdb: "{{ .Query.IMDBID }}"
  rows:
    selector: item
  fields:
    title:
      selector: a
"#,
        );
        let q = query("ubuntu", vec![]);

        let requests = build_search_requests(&def, &q, &Settings::default()).unwrap();

        assert_eq!(
            requests[0].url.as_str(),
            "https://example.org/browse?q=ubuntu"
        );
    }

    #[test]
    fn allow_empty_inputs_keeps_an_empty_rendered_value() {
        let def = parse(
            r#"
id: example
name: Example
links:
  - https://example.org/
search:
  allowEmptyInputs: true
  paths:
    - path: browse
  inputs:
    imdb: "{{ .Query.IMDBID }}"
  rows:
    selector: item
  fields:
    title:
      selector: a
"#,
        );
        let q = query("ubuntu", vec![]);

        let requests = build_search_requests(&def, &q, &Settings::default()).unwrap();

        assert_eq!(requests[0].url.as_str(), "https://example.org/browse?imdb=");
    }

    #[test]
    fn an_absolute_rendered_path_wins_wholesale_over_the_base_link() {
        let def = parse(
            r"
id: example
name: Example
links:
  - https://example.org/
search:
  paths:
    - path: https://other.example/api/search
  rows:
    selector: item
  fields:
    title:
      selector: a
",
        );
        let q = query("ubuntu", vec![]);

        let requests = build_search_requests(&def, &q, &Settings::default()).unwrap();

        assert_eq!(requests[0].url.as_str(), "https://other.example/api/search");
    }

    #[test]
    fn search_headers_first_value_is_rendered_onto_every_request() {
        let def = parse(
            r#"
id: example
name: Example
settings:
  - name: token
    type: text
    label: Token
links:
  - https://example.org/
search:
  headers:
    X-Requested-With: ["{{ .Config.token }}", "unused-second-value"]
  paths:
    - path: browse
  rows:
    selector: item
  fields:
    title:
      selector: a
"#,
        );
        let mut values = BTreeMap::new();
        values.insert("token".to_string(), "abc123".to_string());
        let settings = Settings::new(values);
        let q = query("ubuntu", vec![]);

        let requests = build_search_requests(&def, &q, &settings).unwrap();

        assert_eq!(
            requests[0].headers,
            vec![("X-Requested-With".to_string(), "abc123".to_string())]
        );
    }

    #[test]
    fn a_path_with_no_leading_slash_joins_cleanly_onto_the_base_link() {
        let def = parse(
            r"
id: example
name: Example
links:
  - https://example.org/sub/
search:
  paths:
    - path: browse
  rows:
    selector: item
  fields:
    title:
      selector: a
",
        );
        let q = query("ubuntu", vec![]);

        let requests = build_search_requests(&def, &q, &Settings::default()).unwrap();

        assert_eq!(requests[0].url.as_str(), "https://example.org/sub/browse");
    }

    #[test]
    fn a_bare_search_path_shorthand_is_treated_as_a_single_element_paths_list() {
        // e.g. Bittorrentfiles.yml / houseofdevil.yml: `search.path` instead
        // of a one-entry `search.paths`.
        let def = parse(
            r#"
id: example
name: Example
links:
  - https://example.org/
search:
  path: browse.php
  inputs:
    search: "{{ .Keywords }}"
  rows:
    selector: item
  fields:
    title:
      selector: a
"#,
        );
        let q = query("ubuntu", vec![]);

        let requests = build_search_requests(&def, &q, &Settings::default()).unwrap();

        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].url.as_str(),
            "https://example.org/browse.php?search=ubuntu"
        );
    }
}
