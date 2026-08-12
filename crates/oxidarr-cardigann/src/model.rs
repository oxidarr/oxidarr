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
}

/// Declared search capabilities and category mappings.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Caps {
    #[serde(default)]
    pub categorymappings: Vec<CategoryMapping>,
    #[serde(default)]
    pub modes: BTreeMap<String, Vec<String>>,
}

/// Maps a tracker's own category id onto a Newznab category.
#[derive(Debug, Clone, Deserialize)]
pub struct CategoryMapping {
    pub id: String,
    pub cat: String,
    #[serde(default)]
    pub desc: String,
}

/// A user-configurable setting rendered in the UI.
#[derive(Debug, Clone, Deserialize)]
pub struct Setting {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub default: Option<serde_yaml_ng::Value>,
    #[serde(default)]
    pub options: BTreeMap<String, String>,
}

/// How to perform a search and read its results.
#[derive(Debug, Clone, Deserialize)]
pub struct Search {
    #[serde(default)]
    pub paths: Vec<SearchPath>,
    #[serde(default)]
    pub inputs: BTreeMap<String, String>,
    #[serde(default)]
    pub keywordsfilters: Vec<FilterSpec>,
    #[serde(default)]
    pub headers: BTreeMap<String, Vec<String>>,
    pub rows: Rows,
    pub fields: BTreeMap<String, Field>,
}

/// One request path, optionally templated.
#[derive(Debug, Clone, Deserialize)]
pub struct SearchPath {
    pub path: String,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub inputs: BTreeMap<String, String>,
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
    // Cardigann's `cookie`/`selectorinputs`-style logins (e.g. postman.yml)
    // omit `method` entirely, relying on `selectorinputs` and `test`
    // instead of an explicit HTTP method.
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub inputs: BTreeMap<String, String>,
    #[serde(default)]
    pub test: Option<LoginTest>,
}

/// A request proving the session is authenticated.
#[derive(Debug, Clone, Deserialize)]
pub struct LoginTest {
    pub path: String,
    #[serde(default)]
    pub selector: Option<String>,
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
}
