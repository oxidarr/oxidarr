//! Whole-corpus gate: request building against every real Cardigann
//! definition. Requires `scripts/fetch-definitions.sh` to have run.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use oxidarr_cardigann::model::{Search, SearchPath, parse_definition};
use oxidarr_indexer::{Method, SearchQuery, Settings, build_search_requests};

/// Locates `.definitions/v11` by walking up from the crate directory.
///
/// # Panics
///
/// Panics if `.definitions/v11` is not found in any parent directory.
#[must_use]
pub fn corpus_dir() -> PathBuf {
    let mut dir: &Path = Path::new(env!("CARGO_MANIFEST_DIR"));
    loop {
        let candidate = dir.join(".definitions/v11");
        if candidate.is_dir() {
            return candidate;
        }
        dir = dir.parent().expect(
            "`.definitions/v11` not found. Run `scripts/fetch-definitions.sh` from the repo root.",
        );
    }
}

/// Returns all `.yml` file paths in the corpus directory, sorted.
///
/// # Panics
///
/// Panics if the corpus directory is not readable.
#[must_use]
pub fn definition_paths() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(corpus_dir())
        .expect("corpus directory is readable")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "yml"))
        .collect();
    paths.sort();
    paths
}

#[test]
fn corpus_is_present_and_large() {
    let paths = definition_paths();
    assert!(
        paths.len() > 500,
        "expected 500+ definitions, found {}",
        paths.len()
    );
}

/// Mirrors `oxidarr_indexer::builder`'s private `effective_paths`/
/// `is_reachable` closely enough to pair each [`SearchPath`] with the
/// [`oxidarr_indexer::HttpRequest`] `build_search_requests` built from it,
/// in the same order — needed only so this gate can tell, per produced
/// request, whether *that* path declares any inputs at all (see
/// `every_definition_builds_search_requests` below). `q.categories` is
/// always empty in this gate's representative query, so every path is
/// always reachable; the categories filter is kept anyway so this stays a
/// faithful mirror rather than a check that happens to work for one query.
fn effective_reachable_paths(search: &Search, q: &SearchQuery) -> Vec<SearchPath> {
    let paths: Vec<SearchPath> = if search.paths.is_empty() {
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
    } else {
        search.paths.clone()
    };
    paths
        .into_iter()
        .filter(|path| {
            path.categories.is_empty()
                || q.categories.is_empty()
                || q.categories
                    .iter()
                    .any(|c| path.categories.iter().any(|pc| *pc == c.to_string()))
        })
        .collect()
}

/// Overlays `path.inputs` onto `search.inputs`, mirroring
/// `oxidarr_indexer::builder`'s private `merged_inputs`.
fn merged_inputs(search: &Search, path: &SearchPath) -> BTreeMap<String, String> {
    let mut merged = search.inputs.clone();
    for (key, value) in &path.inputs {
        merged.insert(key.clone(), value.clone());
    }
    merged
}

/// Runs the gate's result-asserting checks (every URL has a host; a GET
/// request whose path declares any inputs at all carries a non-empty query
/// string) against an already-built request list, pushing any violation
/// into `failures`. Shared between the main pass and the known-upstream-
/// defect re-check below, so both apply exactly the same assertions.
fn check_requests(
    def_path: &Path,
    search: &Search,
    q: &SearchQuery,
    requests: &[oxidarr_indexer::HttpRequest],
    failures: &mut Vec<String>,
) {
    if requests.is_empty() {
        failures.push(format!("{}: built zero requests", def_path.display()));
        return;
    }

    let paths = effective_reachable_paths(search, q);
    for (i, req) in requests.iter().enumerate() {
        if req.url.host().is_none() {
            failures.push(format!(
                "{}: request[{i}] URL has no host: {}",
                def_path.display(),
                req.url
            ));
        }

        if req.method == Method::Get
            && let Some(path) = paths.get(i)
            && !merged_inputs(search, path).is_empty()
            && req.url.query().is_none_or(str::is_empty)
        {
            failures.push(format!(
                "{}: request[{i}] for path {:?} declares inputs but the built \
                 GET request has an empty query string: {}",
                def_path.display(),
                path.path,
                req.url
            ));
        }
    }
}

/// Genuine upstream defects, not gaps in this crate: a (file, signature)
/// pair this gate treats as a known, expected `build_search_requests`
/// failure rather than a regression. Keyed on the offending template
/// fragment (the defect's signature) rather than the filename alone, so a
/// different failure in the same file — or this same failure appearing in a
/// file not named here — is never silently swallowed by this exemption.
///
/// `1337x.yml`'s TV search path (`search.paths[1]`) has an unbalanced
/// parenthesis, `(eq .Config.disablesort .False))`: a genuine upstream typo
/// that would fail to parse in Cardigann's own Go template engine too, not a
/// gap in this one (the same string is exempted, for the same reason, by
/// `oxidarr_cardigann`'s `tests/corpus.rs::every_corpus_template_string_renders`).
/// `build_search_requests` builds all of a definition's reachable paths as
/// one unit, so this one broken path fails the *whole* definition rather
/// than just itself. Rather than exempting the whole file from every
/// assertion below, the handler re-builds the definition with only the
/// broken path removed and re-runs the same checks against what remains
/// (Movies, Music, Other), so a regression anywhere else in this file still
/// fails the gate.
const KNOWN_UPSTREAM_DEFECTS: &[(&str, &str)] =
    &[("1337x.yml", "(eq .Config.disablesort .False))")];

/// The completion gate for request building: every real corpus definition
/// must build at least one usable HTTP request for a representative search.
///
/// Result-asserting, not absence-of-error: beyond `build_search_requests`
/// returning `Ok`, every produced URL must have a host (a template that
/// rendered to garbage could still parse as *some* URL), and a GET request
/// whose path declares any inputs at all must carry a non-empty query string
/// (a silently-dropped or mis-appended input would otherwise pass unnoticed).
/// Failures are collected across the whole corpus and reported together, not
/// as a first-failure abort, so a fix pass can see the full extent of a
/// problem in one run.
#[test]
fn every_definition_builds_search_requests() {
    let q = SearchQuery {
        q: Some("ubuntu".to_string()),
        ..SearchQuery::default()
    };

    let mut total = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for def_path in definition_paths() {
        total += 1;
        let raw = match std::fs::read_to_string(&def_path) {
            Ok(raw) => raw,
            Err(err) => {
                failures.push(format!("{}: cannot read: {err}", def_path.display()));
                continue;
            }
        };
        let def = match parse_definition(&raw) {
            Ok(def) => def,
            Err(err) => {
                failures.push(format!(
                    "{}: does not deserialize: {err}",
                    def_path.display()
                ));
                continue;
            }
        };

        // Settings::resolved's defaults: no user-supplied values, so every
        // setting falls back to its own `default:` (or the empty string when
        // it declares none) — see Settings::resolved's doc comment.
        let settings = Settings::default();

        let requests = match build_search_requests(&def, &q, &settings) {
            Ok(requests) => requests,
            Err(err) => {
                let message = err.to_string();
                let known = KNOWN_UPSTREAM_DEFECTS.iter().find(|(file, signature)| {
                    def_path.file_name().is_some_and(|f| f == *file) && message.contains(signature)
                });
                let Some((_, signature)) = known else {
                    failures.push(format!("{}: {message}", def_path.display()));
                    continue;
                };
                // Drop only the path carrying the known defect signature and
                // re-check what remains, so the file's other paths stay
                // gated instead of being waved through untested.
                let mut patched = def.clone();
                patched.search.paths.retain(|p| !p.path.contains(signature));
                match build_search_requests(&patched, &q, &settings) {
                    Ok(requests) => {
                        check_requests(&def_path, &patched.search, &q, &requests, &mut failures);
                    }
                    Err(err) => failures.push(format!(
                        "{}: known-defect path removed, but the remaining paths still \
                         failed to build: {err}",
                        def_path.display()
                    )),
                }
                continue;
            }
        };

        check_requests(&def_path, &def.search, &q, &requests, &mut failures);
    }

    assert!(total > 500, "expected 500+ definitions, found {total}");
    assert!(
        failures.is_empty(),
        "{} of {total} definitions failed to build search requests:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
