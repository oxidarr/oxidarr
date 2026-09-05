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
/// &SearchQuery::default(), s)`.
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
/// `.Query.Keywords` as the query's keywords joined with a single space,
/// `.Query.Q` as the raw (unsplit, unlowercased) query string, and
/// `.Query.IMDBID`/`.Query.Season`/`.Query.Ep` from the matching
/// `SearchQuery` field — each an empty string when its field is absent, per
/// Plan 1's "unset variable renders empty" semantics.
#[must_use]
pub fn scope_for(def: &Definition, q: &SearchQuery, s: &Settings) -> Scope {
    let mut scope = Scope::new();

    for (key, value) in s.resolved(def) {
        scope.set_config(&key, &value);
    }

    let keywords = q.keywords().join(" ");
    scope.set_keywords(&keywords);
    scope.set_query("Keywords", &keywords);
    scope.set_query("Q", q.q.as_deref().unwrap_or_default());
    scope.set_query("IMDBID", q.imdb_id.as_deref().unwrap_or_default());
    scope.set_query(
        "Season",
        &q.season.map(|s| s.to_string()).unwrap_or_default(),
    );
    scope.set_query("Ep", &q.episode.map(|e| e.to_string()).unwrap_or_default());
    scope.set_categories(q.categories.iter().map(std::string::ToString::to_string));

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

        let scope = scope_for(&def, &query, &settings);

        assert_eq!(
            render("{{ .Config.sort }}-{{ .Query.Q }}", &scope).unwrap(),
            "seeders-ubuntu"
        );
    }

    #[test]
    fn scope_for_defaults_query_variables_to_empty_when_absent() {
        let def = definition_with_sort_setting();
        let scope = scope_for(&def, &SearchQuery::default(), &Settings::default());

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
    fn scope_for_joins_keywords_into_query_keywords_and_top_level_keywords() {
        let def = definition_with_sort_setting();
        let query = SearchQuery {
            q: Some("Big Buck Bunny".to_string()),
            ..SearchQuery::default()
        };
        let scope = scope_for(&def, &query, &Settings::default());

        assert_eq!(
            render("{{ .Keywords }}|{{ .Query.Keywords }}", &scope).unwrap(),
            "big buck bunny|big buck bunny"
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
