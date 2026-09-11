//! A search request's query parameters, user-supplied settings, and the
//! bridge from both into a Cardigann [`Scope`](oxidarr_cardigann::template::Scope)
//! for rendering a definition's templated paths/inputs.

use std::collections::BTreeMap;

use oxidarr_cardigann::model::Definition;
use oxidarr_cardigann::template::Scope;

/// One search request's parameters, independent of any indexer definition.
///
/// Derives `Default` because login flows build a [`Scope`] via [`scope_for`]
/// before any actual search query exists, e.g. `scope_for(def,
/// &SearchQuery::default(), s, &[])`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchQuery {
    /// A free-text query string.
    pub q: Option<String>,
    /// An IMDB id to search by, e.g. `tt0000000`.
    pub imdb_id: Option<String>,
    /// A TV season number.
    pub season: Option<u32>,
    /// A TV episode number.
    pub episode: Option<u32>,
    /// Newznab category ids to restrict the search to.
    pub categories: Vec<u32>,
}

impl SearchQuery {
    /// Splits [`Self::q`] on whitespace and lowercases each piece; an absent
    /// `q` yields an empty list.
    #[must_use]
    pub fn keywords(&self) -> Vec<String> {
        match &self.q {
            Some(q) => q.split_whitespace().map(str::to_lowercase).collect(),
            None => Vec::new(),
        }
    }
}

/// User-supplied values for a definition's `settings:`, keyed by
/// [`oxidarr_cardigann::model::Setting::name`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings(BTreeMap<String, String>);

impl Settings {
    /// Wraps already-collected user values.
    #[must_use]
    pub fn new(values: BTreeMap<String, String>) -> Self {
        Self(values)
    }

    /// Overlays these user values onto `def`'s declared settings, filling
    /// any setting the user did not supply with its own `default:` (or the
    /// empty string when a setting declares no default at all).
    #[must_use]
    pub fn resolved(&self, def: &Definition) -> BTreeMap<String, String> {
        def.settings
            .iter()
            .map(|setting| {
                let value = self
                    .0
                    .get(&setting.name)
                    .cloned()
                    .or_else(|| setting.default.clone())
                    .unwrap_or_default();
                (setting.name.clone(), value)
            })
            .collect()
    }
}

/// Builds the [`Scope`] a definition's templates render against for one
/// search: `.Config.*` from `s`'s resolved settings, `.Keywords` and
/// `.Query.Keywords` as the raw (unsplit, unlowercased) query string,
/// `.Query.Q` likewise, `.Query.IMDBID`/`.Query.Season`/`.Query.Ep` from
/// the matching `SearchQuery` field — each an empty string when its field is
/// absent, matching the "unset variable renders empty" convention used
/// throughout this crate's template scopes — and `.Categories` from
/// `mapped_categories`.
///
/// `mapped_categories` is `def`'s TRACKER-id category list for this search
/// (already translated from `q.categories`' Newznab ids and defaults-
/// fallen-back, via `oxidarr_indexer::builder`'s private `mapped_categories`
/// helper — not this module's `q.categories` directly), matching Jackett's
/// and Prowlarr's own `variables[".Categories"] = mappedCategories;`. A
/// caller with no notion of a category-scoped search (`login`'s two call
/// sites, which always use `SearchQuery::default()`) passes an empty slice:
/// real Jackett's `GetBaseTemplateVariables` — the login-time equivalent —
/// never sets `.Categories` at all, so an empty list renders the same as
/// "unset" for every corpus template (none reference `.Categories` from a
/// login block).
///
/// `.Keywords`/`.Query.Keywords` are the raw query text, not
/// [`SearchQuery::keywords`]'s lowercased/whitespace-split tokens.
/// Jackett's `CardigannIndexer.PerformQuery` (src/Jackett.Common/Indexers/
/// Definitions/CardigannIndexer.cs:1416, 1467-1479) builds `.Query.Keywords`
/// by joining `KeywordTokens` — `.Query.Q` (`query.SearchTerm`, verbatim,
/// never lowercased) plus a handful of TV/movie-specific tokens this crate
/// does not model — and then sets `.Keywords` to that value run through
/// `Search.Keywordsfilters` (a no-op when a definition declares none, which
/// is the common case this crate currently handles). There is no
/// lowercasing or word-splitting anywhere in that path, so reusing
/// `keywords()` here would diverge from every real Cardigann template that
/// inspects casing or exact spacing (e.g. a `re_replace` matching literal
/// whitespace runs). `keywords()` itself is kept as-is: it is not used here
/// any more, but is a plausible building block for the title-matching filter
/// Jackett applies separately (`query.MatchQueryStringAND`, same file,
/// ~line 2458), which is out of this task's scope.
#[must_use]
pub fn scope_for(
    def: &Definition,
    q: &SearchQuery,
    s: &Settings,
    mapped_categories: &[String],
) -> Scope {
    let mut scope = Scope::new();

    for (key, value) in s.resolved(def) {
        scope.set_config(&key, &value);
    }

    let raw_query = q.q.as_deref().unwrap_or_default();
    scope.set_keywords(raw_query);
    scope.set_query("Keywords", raw_query);
    scope.set_query("Q", raw_query);
    scope.set_query("IMDBID", q.imdb_id.as_deref().unwrap_or_default());
    scope.set_query(
        "Season",
        &q.season.map(|s| s.to_string()).unwrap_or_default(),
    );
    scope.set_query("Ep", &q.episode.map(|e| e.to_string()).unwrap_or_default());
    scope.set_categories(mapped_categories.iter().cloned());

    scope
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use oxidarr_cardigann::model::parse_definition;
    use oxidarr_cardigann::template::render;

    fn definition_with_sort_setting() -> Definition {
        let yaml = r"
id: example
name: Example
settings:
  - name: sort
    type: select
    label: Sort
    default: created
search:
  rows:
    selector: item
  fields:
    title:
      selector: a
";
        parse_definition(yaml).unwrap()
    }

    #[test]
    fn resolved_overlays_a_user_value_onto_a_settings_default() {
        let def = definition_with_sort_setting();
        let mut values = BTreeMap::new();
        values.insert("sort".to_string(), "seeders".to_string());
        let settings = Settings::new(values);

        let resolved = settings.resolved(&def);

        assert_eq!(resolved.get("sort").map(String::as_str), Some("seeders"));
    }

    #[test]
    fn resolved_falls_back_to_the_settings_default_when_absent() {
        let def = definition_with_sort_setting();
        let settings = Settings::default();

        let resolved = settings.resolved(&def);

        assert_eq!(resolved.get("sort").map(String::as_str), Some("created"));
    }

    #[test]
    fn scope_for_renders_config_and_query_variables() {
        let def = definition_with_sort_setting();
        let mut values = BTreeMap::new();
        values.insert("sort".to_string(), "seeders".to_string());
        let settings = Settings::new(values);
        let query = SearchQuery {
            q: Some("ubuntu".to_string()),
            ..SearchQuery::default()
        };

        let scope = scope_for(&def, &query, &settings, &[]);

        assert_eq!(
            render("{{ .Config.sort }}-{{ .Query.Q }}", &scope).unwrap(),
            "seeders-ubuntu"
        );
    }

    #[test]
    fn scope_for_defaults_query_variables_to_empty_when_absent() {
        let def = definition_with_sort_setting();
        let scope = scope_for(&def, &SearchQuery::default(), &Settings::default(), &[]);

        assert_eq!(
            render(
                "[{{ .Query.IMDBID }}][{{ .Query.Season }}][{{ .Query.Ep }}]",
                &scope
            )
            .unwrap(),
            "[][][]"
        );
    }

    #[test]
    fn scope_for_binds_categories_from_the_mapped_categories_argument_not_the_query() {
        // scope_for takes the already-translated tracker-id list, not
        // `q.categories` (Newznab ids) — see the doc comment and
        // `oxidarr_indexer::builder`'s `mapped_categories` for where that
        // translation happens. A query with Newznab categories set still
        // renders whatever tracker ids `mapped_categories` was given, not
        // those Newznab ids.
        let def = definition_with_sort_setting();
        let query = SearchQuery {
            categories: vec![5030],
            ..SearchQuery::default()
        };
        let mapped = vec!["6".to_string(), "12".to_string()];

        let scope = scope_for(&def, &query, &Settings::default(), &mapped);

        assert_eq!(
            render("{{ join .Categories \",\" }}", &scope).unwrap(),
            "6,12"
        );
    }

    #[test]
    fn scope_for_mirrors_the_raw_query_into_keywords_and_query_keywords() {
        let def = definition_with_sort_setting();
        let query = SearchQuery {
            q: Some("Big Buck Bunny".to_string()),
            ..SearchQuery::default()
        };
        let scope = scope_for(&def, &query, &Settings::default(), &[]);

        assert_eq!(
            render("{{ .Keywords }}|{{ .Query.Keywords }}", &scope).unwrap(),
            "Big Buck Bunny|Big Buck Bunny"
        );
    }

    #[test]
    fn scope_for_does_not_lowercase_or_normalise_whitespace_in_keywords() {
        // Jackett's `.Query.Keywords` is `query.SearchTerm` verbatim (see
        // `scope_for`'s doc comment) — no case folding, no whitespace
        // collapsing. `SearchQuery::keywords()` does both, so this proves
        // `scope_for` does not route through it.
        let def = definition_with_sort_setting();
        let query = SearchQuery {
            q: Some("Big  Buck\tBunny".to_string()),
            ..SearchQuery::default()
        };
        let scope = scope_for(&def, &query, &Settings::default(), &[]);

        assert_eq!(
            render("{{ .Keywords }}", &scope).unwrap(),
            "Big  Buck\tBunny"
        );
    }

    #[test]
    fn keywords_splits_on_whitespace_and_lowercases() {
        let query = SearchQuery {
            q: Some("Big  Buck\tBunny".to_string()),
            ..SearchQuery::default()
        };

        assert_eq!(
            query.keywords(),
            vec!["big".to_string(), "buck".to_string(), "bunny".to_string()]
        );
    }

    #[test]
    fn keywords_is_empty_when_q_is_absent() {
        assert_eq!(SearchQuery::default().keywords(), Vec::<String>::new());
    }
}
