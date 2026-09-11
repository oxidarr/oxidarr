//! Builds [`HttpRequest`]s from a definition's `search.paths`.
//!
//! This is the bridge from a parsed [`Definition`] plus one [`SearchQuery`]
//! into the concrete requests a search actually issues: one per reachable
//! path, each with its templated path, inputs, and headers rendered against
//! a single shared [`Scope`] built by [`scope_for`].

use std::collections::BTreeMap;

use oxidarr_cardigann::catmap::CategoryMap;
use oxidarr_cardigann::model::{Definition, Search, SearchPath};
use oxidarr_cardigann::template::{self, Scope};
use url::Url;

use crate::client::{Body, HttpRequest, Method};
use crate::error::IndexerError;
use crate::query::{SearchQuery, Settings, scope_for};

/// Builds one [`HttpRequest`] per reachable entry in `def.search.paths`.
///
/// `q.categories` (Newznab ids) is translated into `def`'s tracker-id space
/// via [`mapped_categories`] *before* anything else: that translated list —
/// not the raw Newznab query — is what both path reachability ([`is_reachable`])
/// and the `.Categories` scope variable ([`scope_for`]) consume.
///
/// A path's own `categories:` (a list of TRACKER ids, taken straight from
/// `caps.categorymappings.id` — verified against the real corpus, e.g.
/// `diablotorrent.yml`, `1ptbar.yml`, `haitang.yml`, where every path-level
/// `categories:` entry is one of that same file's `categorymappings.id`
/// values; see the task report) is reachable when it declares no
/// restriction, when the translated list is empty — nothing to filter with,
/// mirroring Prowlarr's `CardigannRequestGenerator.GetRequest`, which guards
/// its per-path intersection check on the mapped list being non-empty — or
/// when the two tracker-id lists intersect. This replaces an earlier
/// simplification that compared `q.categories` directly against
/// `path.categories` as raw Newznab-shaped decimal strings — verified wrong
/// by both the corpus (path categories are tracker ids, not Newznab ids)
/// and Jackett's/Prowlarr's own source (both intersect the same tracker-id
/// space; see the task report's citation).
///
/// Every reachable path renders against the *same* [`Scope`], built once via
/// [`scope_for`] — a path's `categories:` restriction does not narrow
/// `.Categories` the way Jackett's per-path intersection does, since
/// `scope_for`'s signature takes no path argument.
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
    let mapped = mapped_categories(def, q);
    let scope = scope_for(def, q, s, &mapped);
    let base = base_url(def)?;

    effective_paths(&def.search)
        .iter()
        .filter(|path| is_reachable(path, &mapped))
        .map(|path| build_request(def, &def.search, path, &base, &scope))
        .collect()
}

/// Translates `q.categories` (Newznab ids) into `def`'s tracker-space
/// category ids via [`CategoryMap::to_tracker`] (parent-expanding: a
/// top-level block id also reaches every tracker id mapped under it — see
/// `CategoryMap`'s module docs), falling back to `def`'s `default:
/// true`-flagged tracker ids when that translation is empty.
///
/// Mirrors Jackett's and Prowlarr's `PerformQuery`/`GetRequest` exactly:
///
/// ```csharp
/// var mappedCategories = MapTorznabCapsToTrackers(query);
/// if (mappedCategories.Count == 0)
/// {
///     mappedCategories = DefaultCategories;
/// }
/// ```
///
/// (`CardigannIndexer.cs` ~1461-1464; `CardigannRequestGenerator.cs`
/// ~1074-1077 — byte-identical logic in both engines). The fallback fires
/// on an empty *result*, not on an empty *query*: a non-empty `q.categories`
/// that happens to match none of `def`'s categorymappings falls back to the
/// same defaults a wholly empty query would (pinned by
/// `a_non_empty_query_that_maps_to_nothing_falls_back_to_defaults_exactly_like_an_empty_query`
/// in this module's tests) — there is no separate "no reachable
/// category-gated paths" case for that scenario in either real engine.
///
/// Computed once per search so [`is_reachable`] and [`scope_for`]'s
/// `.Categories` value agree on the same list, the way both engines share
/// one `mappedCategories` variable for both purposes.
fn mapped_categories(def: &Definition, q: &SearchQuery) -> Vec<String> {
    let map = CategoryMap::from_definition(def);
    let mapped = map.to_tracker(&q.categories);
    if mapped.is_empty() {
        map.defaults().to_vec()
    } else {
        mapped
    }
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
                followredirect: None,
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

/// Whether `path` should be included, given `mapped` — the tracker-space
/// category ids [`mapped_categories`] computed for this search.
///
/// A path with no `categories:` restriction is always reachable. Otherwise
/// reachable when `mapped` is empty (nothing to filter with — see
/// [`mapped_categories`]'s doc comment for why this can happen even after
/// its own defaults fallback, e.g. a definition with no
/// `caps.categorymappings` at all) or when the two tracker-id lists
/// intersect.
fn is_reachable(path: &SearchPath, mapped: &[String]) -> bool {
    if path.categories.is_empty() || mapped.is_empty() {
        return true;
    }
    mapped.iter().any(|m| path.categories.contains(m))
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

/// Builds one [`HttpRequest`] for `path`.
///
/// `path.followredirect` maps straight onto [`HttpRequest::follow_redirects`],
/// defaulting to `true` when the key is absent from the definition — not
/// Jackett's own default. Jackett's HTTP client disables auto-redirect
/// globally (`AllowAutoRedirect = false`, `HttpWebClient2.cs`) and only
/// follows a search path's redirect when `SearchPath.Followredirect` is
/// explicitly `true` (`CardigannIndexer.cs`, guarding every
/// `FollowIfRedirect` call for a search response on `response.IsRedirect &&
/// SearchPath.Followredirect`); the corpus confirms this is opt-in — of the
/// 21 `followredirect` occurrences across `.definitions/v11`, every single
/// one sets `true`, none ever sets `false`. Defaulting an *absent* key to
/// `false` here would therefore flip this crate's pre-Task-6 behaviour
/// (`ReqwestClient` always followed every redirect unconditionally) to
/// "never follow unless declared" for the entire rest of the corpus that
/// never mentions the key — a real regression risk this task deliberately
/// avoids by keeping the always-follow default and only opting a path *out*
/// via an explicit `followredirect: false` (see the task report for the
/// full citation and the corpus survey command).
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
        follow_redirects: path.followredirect.unwrap_or(true),
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
        // MINIMAL declares no `caps.categorymappings`, so translation always
        // yields nothing (no entries to translate to, no `default: true`
        // fallback either) — see `no_categorymapping_definition_...` below
        // for that behaviour pinned as its own test. `cat` renders empty and
        // is dropped by `render_input`'s empty-input rule, so this fixture
        // is queried with no categories at all: this test's actual subject
        // is BTreeMap input ordering, not category translation.
        let q = query("ubuntu", vec![]);

        let requests = build_search_requests(&def, &q, &Settings::default()).unwrap();

        assert_eq!(requests.len(), 1);
        let req = &requests[0];
        assert_eq!(req.method, Method::Get);
        assert_eq!(req.body, None);
        assert_eq!(req.url.as_str(), "https://example.org/browse?q=ubuntu");
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

    // Tracker "1" -> Movies (2000, top-level, default: true).
    // Tracker "2" -> TV/SD (5030, a child of TV/5000, not default).
    // Tracker "3" -> TV (5000, the bare top-level block, not default).
    // Mirrors the real corpus shape (e.g. `diablotorrent.yml`,
    // `1ptbar.yml`): path-level `categories:` lists are TRACKER ids drawn
    // straight from `categorymappings.id`, not Newznab ids — see the task
    // report's Jackett/Prowlarr source citation for why.
    const CATEGORY_DEF: &str = r#"
id: example
name: Example
links:
  - https://example.org/
caps:
  categorymappings:
    - {id: "1", cat: Movies, desc: "Movies", default: true}
    - {id: "2", cat: TV/SD, desc: "TV SD"}
    - {id: "3", cat: TV, desc: "TV block only"}
search:
  paths:
    - path: browse
    - path: movies
      categories: ["1"]
    - path: tv
      categories: ["2"]
    - path: tv-block-only
      categories: ["3"]
  inputs:
    q: "{{ .Keywords }}"
    cat: "{{ join .Categories \",\" }}"
  rows:
    selector: item
  fields:
    title:
      selector: a
"#;

    fn paths_of(requests: &[HttpRequest]) -> Vec<&str> {
        requests.iter().map(|r| r.url.path()).collect()
    }

    #[test]
    fn a_top_level_newznab_query_expands_to_every_tracker_id_mapped_under_it() {
        // Querying the parent block (5000, TV) must reach a path gated on a
        // tracker id mapped to *any* of its children (tv, tracker "2" ->
        // 5030) as well as one mapped to the bare block itself (tv-block-
        // only, tracker "3" -> 5000) — `CategoryMap::to_tracker`'s parent
        // expansion (Task 2), exercised end to end through request
        // building. "movies" (tracker "1" -> 2000, an unrelated top-level
        // block) must NOT be reached.
        let def = parse(CATEGORY_DEF);
        let q = query("ubuntu", vec![5000]);

        let requests = build_search_requests(&def, &q, &Settings::default()).unwrap();

        let mut paths = paths_of(&requests);
        paths.sort_unstable();
        assert_eq!(paths, vec!["/browse", "/tv", "/tv-block-only"]);
        // `.Categories` carries the TRANSLATED tracker ids ("2", "3"), not
        // the raw queried Newznab id (5000) — the core contract of this
        // task. Same scope is shared by every path (see `build_request`'s
        // doc comment), so any reachable request's `cat` shows it.
        let browse = requests.iter().find(|r| r.url.path() == "/browse").unwrap();
        assert_eq!(browse.url.query(), Some("cat=2%2C3&q=ubuntu"));
    }

    #[test]
    fn a_subcategory_newznab_query_does_not_reach_a_path_gated_on_a_block_only_tracker_mapping() {
        // Mirrors `catmap`'s one-directional rule end to end: querying the
        // child (5030, TV/SD) reaches "tv" (tracker "2" -> 5030 exactly)
        // but NOT "tv-block-only" (tracker "3" -> bare 5000), since
        // `to_tracker` never expands a child query to a parent-only
        // mapping.
        let def = parse(CATEGORY_DEF);
        let q = query("ubuntu", vec![5030]);

        let requests = build_search_requests(&def, &q, &Settings::default()).unwrap();

        let mut paths = paths_of(&requests);
        paths.sort_unstable();
        assert_eq!(paths, vec!["/browse", "/tv"]);
    }

    #[test]
    fn an_empty_query_falls_back_to_default_flagged_tracker_ids() {
        // Mirrors Jackett's/Prowlarr's `PerformQuery`/`GetRequest`: `if
        // (mappedCategories.Count == 0) mappedCategories =
        // DefaultCategories;` (see the task report for the source
        // citation). An empty query maps to nothing, so `.Categories`
        // becomes the definition's `default: true`-flagged tracker ids
        // ("1" only) — "movies" (gated on "1") is reached, "tv" and
        // "tv-block-only" (gated on non-default ids) are not.
        let def = parse(CATEGORY_DEF);
        let q = query("ubuntu", vec![]);

        let requests = build_search_requests(&def, &q, &Settings::default()).unwrap();

        let mut paths = paths_of(&requests);
        paths.sort_unstable();
        assert_eq!(paths, vec!["/browse", "/movies"]);
    }

    #[test]
    fn a_non_empty_query_that_maps_to_nothing_falls_back_to_defaults_exactly_like_an_empty_query() {
        // The brief's working assumption was that an empty *translation* of
        // a non-empty query should behave differently from a genuinely
        // empty query (no reachable gated paths at all). Verified against
        // Jackett's and Prowlarr's actual source: both key the
        // `DefaultCategories` fallback purely off `mappedCategories.Count
        // == 0`, with no distinction for why it's empty — a query for a
        // Newznab id this definition never maps (8000, Other) behaves
        // identically to an empty query. See the task report for the two
        // source citations.
        let def = parse(CATEGORY_DEF);
        let q = query("ubuntu", vec![8000]);

        let requests = build_search_requests(&def, &q, &Settings::default()).unwrap();

        let mut paths = paths_of(&requests);
        paths.sort_unstable();
        assert_eq!(paths, vec!["/browse", "/movies"]);
    }

    const NO_CATEGORYMAPPINGS_DEF: &str = r#"
id: example
name: Example
links:
  - https://example.org/
search:
  paths:
    - path: browse
    - path: gated
      categories: ["1000"]
  inputs:
    q: "{{ .Keywords }}"
  rows:
    selector: item
  fields:
    title:
      selector: a
"#;

    #[test]
    fn a_gated_path_is_reachable_regardless_of_query_when_the_definition_declares_no_categorymappings()
     {
        // Real corpus case (`totheglory.yml`): a definition with no
        // `caps.categorymappings` at all, so translation is always empty
        // AND there is no `default: true` id to fall back to either —
        // `mapped_categories` stays empty no matter what's queried.
        // Prowlarr's `CardigannRequestGenerator.GetRequest` guards its
        // per-path Intersect/Except check with `mappedCategories.Count >
        // 0`, so an empty `mappedCategories` skips category filtering
        // entirely rather than excluding every gated path — every path
        // (gated or not) is reachable. (Jackett's own `CardigannIndexer`
        // lacks that extra guard and would exclude the gated path instead;
        // this crate follows Prowlarr, the actual compatibility target —
        // see the task report.)
        let def = parse(NO_CATEGORYMAPPINGS_DEF);
        let q = query("ubuntu", vec![2000]);

        let requests = build_search_requests(&def, &q, &Settings::default()).unwrap();

        let mut paths = paths_of(&requests);
        paths.sort_unstable();
        assert_eq!(paths, vec!["/browse", "/gated"]);
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

    // Tracker "1" -> Movies (2000), tracker "2" -> TV/SD (5030): both exact,
    // unambiguous matches (no parent expansion in play) so the queried
    // Newznab ids translate one-for-one to these tracker ids.
    const RAW_DEF: &str = r#"
id: example
name: Example
links:
  - https://example.org/
caps:
  categorymappings:
    - {id: "1", cat: Movies, desc: "Movies"}
    - {id: "2", cat: TV/SD, desc: "TV SD"}
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
        let q = query("ubuntu", vec![2000, 5030]);

        let requests = build_search_requests(&def, &q, &Settings::default()).unwrap();

        assert_eq!(requests.len(), 1);
        // Normal pairs are appended first, then raw fragments verbatim
        // (including the trailing "&" the range template itself produces) —
        // see build_request's doc comment for why raw is handled last.
        // `.Categories` carries the translated tracker ids ("1", "2"), not
        // the queried Newznab ids (2000, 5030).
        assert_eq!(
            requests[0].url.as_str(),
            "https://example.org/browse?q=ubuntu&c1=1&c2=1&"
        );
    }

    const RAW_POST_DEF: &str = r#"
id: example
name: Example
links:
  - https://example.org/
caps:
  categorymappings:
    - {id: "1", cat: Movies, desc: "Movies"}
    - {id: "2", cat: TV/SD, desc: "TV SD"}
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
        // definitions — this proves it doesn't. `.Categories` carries the
        // translated tracker ids ("1", "2"), not the queried Newznab ids.
        let def = parse(RAW_POST_DEF);
        let q = query("ubuntu", vec![2000, 5030]);

        let requests = build_search_requests(&def, &q, &Settings::default()).unwrap();

        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].url.as_str(),
            "https://example.org/takelogin.php"
        );
        assert_eq!(
            requests[0].body,
            Some(Body::Form(vec![
                ("f[]".to_string(), "1".to_string()),
                ("f[]".to_string(), "2".to_string()),
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
    fn a_path_with_no_followredirect_key_defaults_to_following_redirects() {
        // Absent `followredirect` must map to `follow_redirects: true` —
        // the overwhelming majority of the corpus never declares the key at
        // all, and pre-Task-6 behaviour always followed redirects
        // unconditionally; see the task report for the corpus survey and
        // the Jackett source citations behind this default.
        let def = parse(MINIMAL);
        let q = query("ubuntu", vec![]);

        let requests = build_search_requests(&def, &q, &Settings::default()).unwrap();

        assert!(requests[0].follow_redirects);
    }

    #[test]
    fn a_path_with_explicit_followredirect_false_disables_following() {
        let def = parse(
            r"
id: example
name: Example
links:
  - https://example.org/
search:
  paths:
    - path: browse
      followredirect: false
  rows:
    selector: item
  fields:
    title:
      selector: a
",
        );
        let q = query("ubuntu", vec![]);

        let requests = build_search_requests(&def, &q, &Settings::default()).unwrap();

        assert!(!requests[0].follow_redirects);
    }

    #[test]
    fn a_path_with_explicit_followredirect_true_follows() {
        let def = parse(
            r"
id: example
name: Example
links:
  - https://example.org/
search:
  paths:
    - path: browse
      followredirect: true
  rows:
    selector: item
  fields:
    title:
      selector: a
",
        );
        let q = query("ubuntu", vec![]);

        let requests = build_search_requests(&def, &q, &Settings::default()).unwrap();

        assert!(requests[0].follow_redirects);
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
