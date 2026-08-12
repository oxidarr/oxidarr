//! serde model for the Cardigann v11 definition schema.

use crate::error::CardigannError;
use serde::Deserialize;
use std::collections::BTreeMap;

/// A complete Cardigann v11 indexer definition.
#[derive(Debug, Clone, Deserialize)]
pub struct Definition {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub language: String,
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub encoding: String,
    #[serde(default)]
    pub links: Vec<String>,
    #[serde(default)]
    pub legacylinks: Vec<String>,
    #[serde(default)]
    pub caps: Caps,
    #[serde(default)]
    pub settings: Vec<Setting>,
    pub search: Search,
    #[serde(default)]
    pub login: Option<Login>,
    #[serde(default, rename = "requestDelay")]
    pub request_delay: Option<f64>,
    /// How to resolve a download link (e.g. a magnet/infohash) from a
    /// result, for indexers where that needs its own request/extraction
    /// pipeline separate from the search row itself.
    #[serde(default)]
    pub download: Option<Download>,
    /// Ids of older definitions this one supersedes.
    #[serde(default)]
    pub replaces: Vec<String>,
    /// Pinned TLS certificate fingerprints for this indexer's host.
    #[serde(default)]
    pub certificates: Vec<String>,
    #[serde(default)]
    pub followredirect: bool,
    #[serde(default, rename = "testlinktorrent")]
    pub test_link_torrent: bool,
}

/// Declared search capabilities and category mappings.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Caps {
    #[serde(default)]
    pub categorymappings: Vec<CategoryMapping>,
    #[serde(default)]
    pub modes: BTreeMap<String, Vec<String>>,
    /// Extra tracker-defined categories declared by id/label, distinct from
    /// `categorymappings` (which maps onto Newznab categories).
    #[serde(default)]
    pub categories: BTreeMap<String, String>,
    #[serde(default)]
    pub allowrawsearch: bool,
    #[serde(default)]
    pub allowtvsearchimdb: bool,
}

/// Maps a tracker's own category id onto a Newznab category.
#[derive(Debug, Clone, Deserialize)]
pub struct CategoryMapping {
    pub id: String,
    pub cat: String,
    #[serde(default)]
    pub desc: String,
    /// Marks the tracker's default category, used when a search specifies
    /// no category at all (660 of 19,014 category-mapping entries across
    /// 26 files, e.g. 1ptbar.yml).
    #[serde(default)]
    pub default: bool,
}

/// A user-configurable setting rendered in the UI.
#[derive(Debug, Clone, Deserialize)]
pub struct Setting {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub label: String,
    // The corpus's `default:` values are exclusively strings, bools, and
    // integers (never a nested list/mapping), and `serde_yaml_ng` coerces
    // any of those scalar shapes into a plain `String` field directly
    // (proven: `default: true`/`default: 5`/`default: seeders` all
    // deserialize into `Some("true")`/`Some("5")`/`Some("seeders")`), so a
    // plain `String` captures every value losslessly without needing a
    // `serde_yaml_ng::Value` or a bespoke enum.
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub options: BTreeMap<String, String>,
}

/// How to perform a search and read its results.
#[derive(Debug, Clone, Deserialize)]
pub struct Search {
    #[serde(default)]
    pub paths: Vec<SearchPath>,
    /// A single unwrapped path, as shorthand for a one-element `paths`
    /// list (e.g. Bittorrentfiles.yml, houseofdevil.yml).
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub inputs: BTreeMap<String, String>,
    #[serde(default)]
    pub keywordsfilters: Vec<FilterSpec>,
    #[serde(default)]
    pub headers: BTreeMap<String, Vec<String>>,
    pub rows: Rows,
    pub fields: BTreeMap<String, Field>,
    /// Response-level error detection, distinct from row-level filters.
    #[serde(default)]
    pub error: Vec<ErrorRule>,
    #[serde(default, rename = "allowEmptyInputs")]
    pub allow_empty_inputs: bool,
}

/// One request path, optionally templated.
#[derive(Debug, Clone, Deserialize)]
pub struct SearchPath {
    pub path: String,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub inputs: BTreeMap<String, String>,
    /// The subset of `Categories` this path is used for, when a definition
    /// splits search across several paths by category.
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub response: Option<Response>,
    #[serde(default)]
    pub followredirect: bool,
}

/// How to interpret a path's response body.
#[derive(Debug, Clone, Deserialize)]
pub struct Response {
    #[serde(rename = "type")]
    pub kind: String,
    /// Text to show when this path's response indicates no results (e.g.
    /// reelflix-api.yml's `"No Torrents Found"`), distinct from a genuine
    /// error.
    #[serde(default, rename = "noResultsMessage")]
    pub no_results_message: Option<String>,
}

/// How to locate result rows in a response.
#[derive(Debug, Clone, Deserialize)]
pub struct Rows {
    pub selector: String,
    #[serde(default)]
    pub attribute: Option<String>,
    #[serde(default)]
    pub filters: Vec<FilterSpec>,
    #[serde(default)]
    pub after: Option<usize>,
    #[serde(default)]
    pub count: Option<Count>,
    /// Extracts a date shared by multiple rows (e.g. a "Torrents added
    /// <date>" heading) rather than repeated per row.
    #[serde(default)]
    pub dateheaders: Option<Field>,
    #[serde(default)]
    pub multiple: bool,
    #[serde(default, rename = "missingAttributeEqualsNoResults")]
    pub missing_attribute_equals_no_results: bool,
}

/// A row count, either given directly or read out of the response via a
/// selector (e.g. a JSON path into an API response body).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Count {
    Literal(usize),
    Selector { selector: String },
}

/// How to extract one field from a row.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Field {
    #[serde(default)]
    pub selector: Option<String>,
    #[serde(default)]
    pub attribute: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub filters: Vec<FilterSpec>,
    /// The only extraction rule for `downloadvolumefactor`/
    /// `uploadvolumefactor` in the majority of the corpus (over half of all
    /// definitions): a first-match-wins list of selector/value pairs, most
    /// commonly ending in a `'*'` catch-all.
    #[serde(default)]
    pub case: Option<Case>,
    // Same coercion argument as `Setting::default`: the corpus's field-level
    // `default:` values are only ever strings or bare integers, and
    // `serde_yaml_ng` deserializes either directly into a plain `String`.
    #[serde(default)]
    pub default: Option<String>,
    /// A CSS selector for elements to strip out of the matched node before
    /// reading its text (e.g. removing `<a>`/`<img>` clutter from a
    /// description column).
    #[serde(default)]
    pub remove: Option<String>,
}

/// A `case:` block: an ordered list of (selector, value) pairs, matched in
/// document order.
///
/// Cardigann evaluates these entries top to bottom and stops at the first
/// match; a `'*'` key is the conventional catch-all and must be tried
/// *last*. `BTreeMap`/`HashMap` cannot represent this: `'*'` (`0x2A`) sorts
/// before every letter and digit, so a sorted or hashed map would silently
/// move the catch-all to the front and make it swallow every case. This
/// type deserializes through `serde_yaml_ng::Mapping`, which preserves the
/// document's original key order, and keeps that order in a `Vec`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Case(pub Vec<(String, String)>);

impl<'de> Deserialize<'de> for Case {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let mapping = serde_yaml_ng::Mapping::deserialize(deserializer)?;
        let mut entries = Vec::with_capacity(mapping.len());
        for (key, value) in mapping {
            let key = scalar_to_string(&key)
                .ok_or_else(|| serde::de::Error::custom("case key is not a scalar"))?;
            let value = scalar_to_string(&value)
                .ok_or_else(|| serde::de::Error::custom("case value is not a scalar"))?;
            entries.push((key, value));
        }
        Ok(Self(entries))
    }
}

/// Renders a scalar YAML value as text, the same coercion `serde_yaml_ng`
/// applies natively to a plain `String`-typed field — needed here because
/// [`Case`] deserializes through an intermediate `Mapping` rather than
/// going straight to `String`.
fn scalar_to_string(value: &serde_yaml_ng::Value) -> Option<String> {
    match value {
        serde_yaml_ng::Value::String(s) => Some(s.clone()),
        serde_yaml_ng::Value::Bool(b) => Some(b.to_string()),
        serde_yaml_ng::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// A `{name, args}` filter invocation from a definition.
#[derive(Debug, Clone, Deserialize)]
pub struct FilterSpec {
    pub name: String,
    #[serde(default)]
    pub args: FilterArgs,
}

/// Filter arguments are variously a scalar, a list, or absent.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(untagged)]
pub enum FilterArgs {
    #[default]
    None,
    One(FilterArg),
    Many(Vec<FilterArg>),
}

impl FilterArgs {
    /// Normalises arguments into a flat list.
    #[must_use]
    pub fn to_vec(&self) -> Vec<String> {
        match self {
            Self::None => Vec::new(),
            Self::One(a) => vec![a.to_string()],
            Self::Many(v) => v.iter().map(std::string::ToString::to_string).collect(),
        }
    }
}

/// A single item within a filter's `args:` list.
///
/// Most args are strings, but filters like `split` take a bare integer
/// index (e.g. `args: ["|", 0]` or `args: [",", -1]`), so a plain
/// `Vec<String>` cannot deserialize those entries directly.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum FilterArg {
    Text(String),
    Number(i64),
}

impl std::fmt::Display for FilterArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text(s) => write!(f, "{s}"),
            Self::Number(n) => write!(f, "{n}"),
        }
    }
}

/// How to authenticate before searching.
#[derive(Debug, Clone, Deserialize)]
pub struct Login {
    #[serde(default)]
    pub path: Option<String>,
    // Cardigann's `cookie`/`selectorinputs`-style logins (e.g. postman.yml,
    // the one login block in the corpus with no `method` at all) rely on
    // `selectorinputs` and `test` instead of an explicit HTTP method.
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub inputs: BTreeMap<String, String>,
    #[serde(default)]
    pub test: Option<LoginTest>,
    /// How to detect a failed login, present in most login blocks in the
    /// corpus.
    #[serde(default)]
    pub error: Vec<ErrorRule>,
    /// A CSS selector for the `<form>` element to submit.
    #[serde(default)]
    pub form: Option<String>,
    /// Overrides where the login form's `action` submits to.
    #[serde(default)]
    pub submitpath: Option<String>,
    /// Input values that must be scraped from the login page itself before
    /// submitting (e.g. a CSRF token), keyed by input name.
    #[serde(default)]
    pub selectorinputs: BTreeMap<String, Field>,
    /// Literal cookies to set before requesting the login page.
    #[serde(default)]
    pub cookies: Vec<String>,
    #[serde(default)]
    pub captcha: Option<Captcha>,
    /// When true, `inputs`' keys are CSS selectors for the form fields to
    /// fill rather than plain input names.
    #[serde(default)]
    pub selectors: bool,
    #[serde(default)]
    pub headers: BTreeMap<String, Vec<String>>,
}

/// A request proving the session is authenticated.
#[derive(Debug, Clone, Deserialize)]
pub struct LoginTest {
    pub path: String,
    #[serde(default)]
    pub selector: Option<String>,
}

/// A rule for detecting a failed request, shared by `login.error` and
/// `search.error`.
#[derive(Debug, Clone, Deserialize)]
pub struct ErrorRule {
    pub selector: String,
    /// How to read the human-readable message for this match. Despite the
    /// name, this is extracted exactly like any other [`Field`] — most
    /// commonly via a literal `text:`, but `selector:`, `attribute:`,
    /// `filters:`, and `remove:` all occur too, so this reuses `Field`
    /// rather than a smaller selector/text-only struct.
    #[serde(default)]
    pub message: Option<Field>,
}

/// An image captcha to solve before submitting the login form.
#[derive(Debug, Clone, Deserialize)]
pub struct Captcha {
    #[serde(rename = "type")]
    pub kind: String,
    pub selector: String,
    pub input: String,
}

/// How to resolve a result's actual download link, for indexers where that
/// takes its own request/extraction pipeline rather than being read
/// straight off the search row (13% of the corpus, e.g. `0magnet.yml`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Download {
    #[serde(default)]
    pub selectors: Vec<DownloadSelector>,
    /// A request to make before the download itself, e.g. to submit a
    /// confirmation form.
    #[serde(default)]
    pub before: Option<DownloadBefore>,
    #[serde(default)]
    pub infohash: Option<Infohash>,
    #[serde(default)]
    pub headers: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub method: Option<String>,
}

/// One candidate selector for a result's download link, tried in order.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DownloadSelector {
    #[serde(default)]
    pub selector: Option<String>,
    #[serde(default)]
    pub attribute: Option<String>,
    #[serde(default)]
    pub filters: Vec<FilterSpec>,
    /// Read this selector against `download.before`'s response instead of
    /// the result row's page.
    #[serde(default)]
    pub usebeforeresponse: bool,
}

/// A request made prior to resolving the download link.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DownloadBefore {
    #[serde(default)]
    pub path: Option<String>,
    /// An alternative to a literal `path`: reads the request path from the
    /// result page via a selector.
    #[serde(default)]
    pub pathselector: Option<Field>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub inputs: BTreeMap<String, String>,
}

/// Extracts a magnet/infohash and its title from a result page.
#[derive(Debug, Clone, Deserialize)]
pub struct Infohash {
    pub hash: Field,
    pub title: Field,
    #[serde(default)]
    pub usebeforeresponse: bool,
}

/// Parses a definition from YAML.
///
/// Some definitions in the wild are saved with a leading UTF-8 BOM; it is
/// stripped before parsing so such files are not rejected as malformed.
///
/// # Errors
///
/// Returns [`CardigannError::Definition`] if `yaml` is not valid YAML or does
/// not match the v11 definition schema.
pub fn parse_definition(yaml: &str) -> Result<Definition, CardigannError> {
    let yaml = yaml.trim_start_matches('\u{FEFF}');
    serde_yaml_ng::from_str(yaml).map_err(|e| CardigannError::Definition {
        reason: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const SAMPLE: &str = r#"
id: example
name: Example Tracker
description: An example
language: en-US
type: public
encoding: UTF-8
links:
  - https://example.org/
caps:
  categorymappings:
    - {id: "1", cat: Movies, desc: "Movies"}
  modes:
    search: [q]
    tv-search: [q, season, ep]
settings:
  - name: sort
    type: select
    label: Sort
    default: seeders
    options:
      seeders: seeders
search:
  paths:
    - path: search/{{ .Keywords }}
  rows:
    selector: tr.result
  fields:
    title:
      selector: a.name
    download:
      selector: a.dl
      attribute: href
"#;

    #[test]
    fn parses_a_minimal_definition() {
        let def = parse_definition(SAMPLE).unwrap();
        assert_eq!(def.id, "example");
        assert_eq!(def.name, "Example Tracker");
        assert_eq!(def.links.len(), 1);
        assert_eq!(def.search.paths.len(), 1);
        assert_eq!(def.search.rows.selector, "tr.result");
        assert!(def.search.fields.contains_key("title"));
    }

    #[test]
    fn field_attribute_is_optional() {
        let def = parse_definition(SAMPLE).unwrap();
        assert!(def.search.fields["title"].attribute.is_none());
        assert_eq!(
            def.search.fields["download"].attribute.as_deref(),
            Some("href")
        );
    }

    #[test]
    fn malformed_yaml_is_an_error() {
        assert!(parse_definition("id: [unclosed").is_err());
    }

    #[test]
    fn rows_count_accepts_a_selector_object() {
        let yaml = r"
id: example
name: Example
search:
  rows:
    selector: item
    count:
      selector: response.total
  fields:
    title:
      selector: a
";
        let def = parse_definition(yaml).unwrap();
        let Some(Count::Selector { selector }) = def.search.rows.count else {
            unreachable!("expected Count::Selector, got {:?}", def.search.rows.count);
        };
        assert_eq!(selector, "response.total");
    }

    #[test]
    fn rows_count_accepts_a_literal_integer() {
        let yaml = r"
id: example
name: Example
search:
  rows:
    selector: item
    count: 25
  fields:
    title:
      selector: a
";
        let def = parse_definition(yaml).unwrap();
        assert!(matches!(def.search.rows.count, Some(Count::Literal(25))));
    }

    #[test]
    fn filter_args_accepts_a_negative_index_and_round_trips_through_to_vec() {
        // The `split` filter's second argument is a bare (possibly
        // negative) integer, e.g. `args: [",", -1]` in nyaasi.yml.
        let spec: FilterSpec = serde_yaml_ng::from_str("name: split\nargs: [\",\", -1]").unwrap();
        assert_eq!(spec.args.to_vec(), vec![",".to_string(), "-1".to_string()]);
    }

    #[test]
    fn filter_arg_number_displays_as_its_decimal_form() {
        assert_eq!(FilterArg::Number(-1).to_string(), "-1");
        assert_eq!(FilterArg::Number(0).to_string(), "0");
        assert_eq!(FilterArg::Text("|".to_string()).to_string(), "|");
    }

    #[test]
    fn login_without_a_method_still_parses() {
        // postman.yml's login authenticates via `selectorinputs` + `test`
        // and has no `method` key at all.
        let yaml = r#"
id: example
name: Example
login:
  path: index.php
  selectorinputs:
    formtoken:
      selector: input[name="formtoken"]
      attribute: value
  test:
    path: /
    selector: a[href^="logout.php"]
search:
  rows:
    selector: item
  fields:
    title:
      selector: a
"#;
        let def = parse_definition(yaml).unwrap();
        let login = def.login.unwrap();
        assert!(login.method.is_none());
        assert!(login.selectorinputs.contains_key("formtoken"));
    }

    #[test]
    fn a_leading_utf8_bom_is_stripped_before_parsing() {
        let with_bom = format!("\u{FEFF}{SAMPLE}");
        let def = parse_definition(&with_bom).unwrap();
        assert_eq!(def.id, "example");
    }

    #[test]
    fn case_entries_keep_document_order_with_the_wildcard_last() {
        // A `BTreeMap` would sort `'*'` (0x2A) before every alphanumeric
        // key, which would silently move the catch-all to the front and
        // make it swallow every other case. `Case` must preserve the
        // document's original order through deserialization.
        let yaml = r#"
'img[src$="/freedownload.gif"]': 0
'img[src$="/halfdownload.gif"]': 0.5
'*': 1
"#;
        let case: Case = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(
            case.0,
            vec![
                (
                    r#"img[src$="/freedownload.gif"]"#.to_string(),
                    "0".to_string()
                ),
                (
                    r#"img[src$="/halfdownload.gif"]"#.to_string(),
                    "0.5".to_string()
                ),
                ("*".to_string(), "1".to_string()),
            ]
        );
        // The wildcard is specifically last, not merely present.
        assert_eq!(case.0.last().unwrap().0, "*");
    }

    #[test]
    fn case_on_a_field_deserializes_within_a_full_definition() {
        let yaml = r#"
id: example
name: Example
search:
  rows:
    selector: item
  fields:
    title:
      selector: a
    downloadvolumefactor:
      case:
        'img[src$="/freedownload.gif"]': 0
        '*': 1
"#;
        let def = parse_definition(yaml).unwrap();
        let case = def.search.fields["downloadvolumefactor"]
            .case
            .as_ref()
            .unwrap();
        assert_eq!(case.0.len(), 2);
        assert_eq!(case.0[1].0, "*");
    }

    #[test]
    fn category_mapping_default_flag_is_captured() {
        // 1ptbar.yml: `{id: 401, cat: Movies, desc: "Movie", default: true}`
        // marks the tracker's default category; without this field it was
        // silently dropped on 660 of 19,014 category-mapping entries.
        let yaml = r#"
id: example
name: Example
caps:
  categorymappings:
    - {id: "401", cat: Movies, desc: "Movie", default: true}
    - {id: "402", cat: TV, desc: "TV"}
search:
  rows:
    selector: item
  fields:
    title:
      selector: a
"#;
        let def = parse_definition(yaml).unwrap();
        assert!(def.caps.categorymappings[0].default);
        assert!(!def.caps.categorymappings[1].default);
    }

    #[test]
    fn path_response_no_results_message_is_captured() {
        // reelflix-api.yml carries a real, non-empty noResultsMessage.
        let yaml = r#"
id: example
name: Example
search:
  paths:
    - path: api/search
      response:
        type: json
        noResultsMessage: "No Torrents Found"
  rows:
    selector: item
  fields:
    title:
      selector: a
"#;
        let def = parse_definition(yaml).unwrap();
        let response = def.search.paths[0].response.as_ref().unwrap();
        assert_eq!(response.kind, "json");
        assert_eq!(
            response.no_results_message.as_deref(),
            Some("No Torrents Found")
        );
    }
}
