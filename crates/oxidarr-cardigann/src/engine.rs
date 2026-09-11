//! Executes a definition's `search` block against a response body.

use crate::catmap::CategoryMap;
use crate::error::CardigannError;
use crate::filters::{self, FilterCtx, FilterOutcome};
use crate::model::{Case, Definition, Field};
use crate::{selector, template};
use chrono::{DateTime, Utc};
use oxidarr_core::Release;
use scraper::{ElementRef, Html, Node};
use std::collections::{BTreeMap, HashSet};

/// Runs a definition's row and field extraction over a response body.
///
/// Fields are evaluated in the order the YAML declares them, and each result
/// is written into that row's scope as it is produced (under `.Result.<name>`),
/// so a later field's `text:` template can reference an earlier one. Order is
/// preserved by [`crate::model::Fields`]; it is semantic, not incidental.
///
/// This path understands HTML only. A definition whose search response is
/// declared as JSON is rejected outright rather than run against a parsed
/// HTML document, where its selectors would match nothing and produce an
/// empty result indistinguishable from a genuinely empty search.
///
/// A row whose filter chain yields [`FilterOutcome::DropRow`] for any field
/// (`andmatch` does this for a row missing a search keyword — see
/// [`FilterOutcome`]) is skipped entirely rather than emitted with a
/// partial value.
///
/// # Errors
///
/// Returns [`CardigannError`] if the definition declares a JSON response, or
/// if the rows selector, a field's `selector:`, a `case:` key, or a `remove:`
/// selector fails to compile, or if a field's `text:` template or filter
/// chain fails.
pub fn extract(
    def: &Definition,
    body: &str,
    config: &BTreeMap<String, String>,
    ctx: &FilterCtx,
) -> Result<Vec<Release>, CardigannError> {
    // Checked from the DECLARED response type, not inferred from selector
    // shape. 95 of the 101 JSON definitions use a bare-identifier row
    // selector such as `data` or `item`, which parses as a perfectly valid
    // CSS tag selector — so shape inference catches only 6 of them and the
    // rest would silently yield zero releases.
    if def.declares_json_response() {
        return Err(CardigannError::Definition {
            reason: "definition declares a JSON search response; the HTML extraction \
                     engine cannot read it"
                .to_string(),
        });
    }

    let doc = Html::parse_document(body);
    let rows_selector = compile_html_selector(&def.search.rows.selector)?;
    // Built once per `extract()` call, not per row: it only depends on the
    // definition's `caps.categorymappings`, which never changes across rows.
    let category_map = CategoryMap::from_definition(def);

    let mut out = Vec::new();
    let skip = def.search.rows.after.unwrap_or(0);

    'rows: for row in rows_selector
        .select(doc.root_element())
        .into_iter()
        .skip(skip)
    {
        let mut scope = template::Scope::new();
        for (k, v) in config {
            scope.set_config(k, v);
        }

        let mut values: BTreeMap<String, String> = BTreeMap::new();
        for (name, field) in &def.search.fields {
            let raw = match evaluate_field(field, row, &scope, ctx)? {
                FilterOutcome::Value(v) => v,
                FilterOutcome::DropRow => continue 'rows,
            };
            scope.set_result(name, &raw);
            values.insert(name.clone(), raw);
        }
        out.push(build_release(&values, &category_map));
    }

    Ok(out)
}

/// Resolves a single field: locate a value, then run its filter chain.
///
/// Resolution order matters and mirrors Cardigann:
/// 1. `case:` — try each entry in DECLARATION ORDER against the row and
///    take the mapped value of the first whose selector matches. `'*'` is
///    the catch-all and, because entries are ordered, is only reached if
///    nothing earlier in the list matched — which also means it wins for
///    every row when a definition declares it first. This is the ONLY
///    extraction rule for `downloadvolumefactor`/`uploadvolumefactor` in
///    roughly 55% of the corpus, so it must be handled before anything
///    else.
/// 2. `text:` — a template rendered against the row scope.
/// 3. `selector:` (+ optional `attribute:`) — read from the first element
///    under the row that matches, in document order.
/// 4. `remove:` — strip elements matching this selector (and their
///    subtrees) out of the matched element's own subtree before reading
///    its text. Has no effect when `attribute:` is set: an attribute value
///    has no subtree to strip from.
/// 5. `filters:` — the filter chain, applied to whatever the above
///    produced.
/// 6. `default:` — substituted only when the result, after filtering, is
///    still empty.
///
/// Returns [`FilterOutcome::DropRow`] when any filter in the chain says to
/// drop the row this field belongs to; the caller must skip the whole row,
/// not just this field, when that happens.
fn evaluate_field(
    field: &Field,
    row: ElementRef<'_>,
    scope: &template::Scope,
    ctx: &FilterCtx,
) -> Result<FilterOutcome, CardigannError> {
    let mut value = if let Some(case) = &field.case {
        case_value(case, row, scope)?
    } else if let Some(text) = &field.text {
        template::render(text, scope)?
    } else if let Some(sel) = &field.selector {
        select_value(field, sel, row)?
    } else {
        String::new()
    };

    for spec in &field.filters {
        let filter = filters::parse_filter(&spec.name, &spec.args.to_vec())?;
        match filters::apply(&filter, &value, ctx)? {
            FilterOutcome::Value(v) => value = v,
            FilterOutcome::DropRow => return Ok(FilterOutcome::DropRow),
        }
    }

    if value.is_empty()
        && let Some(default) = &field.default
    {
        value.clone_from(default);
    }

    Ok(FilterOutcome::Value(value))
}

/// Resolves a `case:` field: the mapped value of the first entry (in
/// declaration order) whose key matches `row`.
///
/// `'*'` always matches without being compiled as CSS — it is a catch-all
/// token, not a real selector, and letting it fall through to
/// [`compile_html_selector`] would either fail to parse or (worse) parse
/// as some unintended universal-ish selector. Every other key is a CSS
/// selector tested against `row`'s descendants.
///
/// Returns an empty string if no entry matches (including when `case` is
/// empty) — the same "produced nothing" outcome as an unmatched
/// `selector:`, so the caller's `default:` handling applies uniformly.
fn case_value(
    case: &Case,
    row: ElementRef<'_>,
    scope: &template::Scope,
) -> Result<String, CardigannError> {
    for (key, value) in &case.0 {
        // Matches Prowlarr's HTML branch: `'*'` is the catch-all, and a key
        // matches when the selected element ITSELF matches it or any
        // descendant does. Checking only descendants misses the common
        // `case:` on the element the selector already found.
        let matched = if key == "*" {
            true
        } else {
            let compiled = compile_html_selector(key)?;
            compiled.matches(row) || !compiled.select(row).is_empty()
        };
        if matched {
            // Case values are templates, not literals — `{{ .False }}` has to
            // render, otherwise the literal braces flow into the release.
            return template::render(value, scope);
        }
    }
    Ok(String::new())
}

/// Reads a field's value from the first element under `row` that matches
/// `sel`, honouring `attribute:` and `remove:`.
///
/// Returns an empty string if nothing matches, matching `case_value`'s
/// "produced nothing" convention.
fn select_value(field: &Field, sel: &str, row: ElementRef<'_>) -> Result<String, CardigannError> {
    let compiled = compile_html_selector(sel)?;
    let Some(el) = compiled.select(row).into_iter().next() else {
        return Ok(String::new());
    };

    if let Some(attr) = &field.attribute {
        return Ok(el.attr(attr).unwrap_or_default().to_string());
    }

    let Some(remove_sel) = &field.remove else {
        return Ok(el.text().collect());
    };

    // `remove:` strips elements matching `remove_sel` (and everything
    // beneath them) out of `el`'s own subtree before reading text. scraper
    // parses an immutable tree, so there is no DOM node to actually
    // delete; instead, every matching element's id is recorded, and text
    // nodes descending from any of them are skipped while walking `el`'s
    // subtree.
    let remove_compiled = compile_html_selector(remove_sel)?;
    let removed: HashSet<_> = remove_compiled
        .select(el)
        .into_iter()
        .map(|removed_el| removed_el.id())
        .collect();

    let mut out = String::new();
    for node in el.descendants() {
        let Node::Text(text) = node.value() else {
            continue;
        };
        let mut ancestor = node.parent();
        let mut under_removed = false;
        while let Some(a) = ancestor {
            if a.id() == el.id() {
                break;
            }
            if removed.contains(&a.id()) {
                under_removed = true;
                break;
            }
            ancestor = a.parent();
        }
        if !under_removed {
            out.push_str(text);
        }
    }
    Ok(out)
}

/// Compiles `raw` for use against an HTML document, rejecting JSON
/// field-path selectors outright rather than letting them silently select
/// nothing.
///
/// # Errors
///
/// Returns [`CardigannError::Selector`] if `raw` is a JSON field-path
/// selector (`$`, `..field`, `items[0].name`, ...) — this extraction path
/// has no JSON extraction pipeline — or if `raw` is otherwise invalid CSS.
fn compile_html_selector(raw: &str) -> Result<selector::CompiledSelector, CardigannError> {
    let compiled = selector::compile(raw)?;
    if compiled.is_json_path() {
        return Err(CardigannError::Selector {
            selector: raw.to_string(),
            reason: "JSON field-path selectors are not supported by the HTML extraction engine"
                .to_string(),
        });
    }
    Ok(compiled)
}

/// Maps extracted field values onto the canonical [`Release`].
///
/// `publish_date` is populated from the `date` field's final (post-filter)
/// value when, and only when, that value parses as RFC 3339 — which is
/// exactly what `dateparse`/`timeparse`/`timeago`/`fuzzytime` emit on
/// success. A definition whose `date` field has no such filter, or whose
/// filter chain failed to recognise the input (both leave the raw
/// tracker-formatted text untouched), yields `None` here rather than a
/// guessed value: there is no way to turn arbitrary unparsed text into a
/// `DateTime<Utc>` without inventing ad hoc parsing that would duplicate,
/// and likely contradict, what those filters already do. The raw `date`
/// value stays available under `.Result.date` for template references
/// regardless.
///
/// `categories` comes from the row's `category` field value (the corpus's
/// only extraction-time category field — surveyed across the full v11
/// corpus: 517 of 548 definitions declare a `fields.category` (counted via
/// `yaml.safe_load` over every `.definitions/v11/*.yml`, checking for a
/// `search.fields.category` key), none declare a second `category2`-style
/// variant, and no `category` field's filter chain splits its result into
/// more than one id), resolved through `category_map`
/// via [`CategoryMap::to_newznab`]. A tracker id with no matching
/// `categorymappings` row (or a definition with no `caps.categorymappings`
/// at all) contributes nothing: the row's `categories` stays empty rather
/// than guessing. The result is sorted and deduplicated so two
/// `categorymappings` rows that happen to name the same Newznab category
/// under different tracker ids do not produce a visibly unordered or
/// duplicated vector.
fn build_release(values: &BTreeMap<String, String>, category_map: &CategoryMap) -> Release {
    let get = |k: &str| values.get(k).map(String::as_str).unwrap_or_default();
    let non_empty = |k: &str| {
        let v = get(k);
        (!v.is_empty()).then(|| v.to_string())
    };

    // `Release` is `#[non_exhaustive]`, which forbids struct-literal
    // construction from outside its crate even with a `..Release::default()`
    // base — only field assignment on an existing value is allowed here.
    let mut release = Release::default();
    release.title = get("title").to_string();
    release.size = parse_size(get("size"));
    release.seeders = get("seeders").replace(',', "").parse().ok();
    release.leechers = get("leechers").replace(',', "").parse().ok();
    release.grabs = get("grabs").replace(',', "").parse().ok();
    release.details_url = non_empty("details");
    release.download_url = non_empty("download");
    release.magnet_url = non_empty("magnet");
    release.info_hash = non_empty("infohash");
    release.imdb_id = non_empty("imdbid");
    release.publish_date = DateTime::parse_from_rfc3339(get("date"))
        .ok()
        .map(|dt| dt.with_timezone(&Utc));
    release.tmdb_id = get("tmdbid").parse().ok();
    release.tvdb_id = get("tvdbid").parse().ok();
    release.download_volume_factor = get("downloadvolumefactor").parse().unwrap_or(1.0);
    release.upload_volume_factor = get("uploadvolumefactor").parse().unwrap_or(1.0);
    release.description = non_empty("description");
    release.poster = non_empty("poster");
    release.genre = non_empty("genre");
    release.files = get("files").replace(',', "").parse().ok();
    release.minimum_seed_time = get("minimumseedtime").parse().ok();
    release.minimum_ratio = get("minimumratio").parse().ok();
    let mut categories = category_map.to_newznab(get("category"));
    categories.sort_unstable();
    categories.dedup();
    release.categories = categories;
    release
}

/// Parses a human-readable size such as `1.4 GB` into bytes.
///
/// Returns `None` when no size can be read, rather than `0` — a zero-byte
/// sentinel would be indistinguishable from a genuinely tiny release and
/// would sort as "smallest" wherever sizes are compared.
///
/// Integer arithmetic throughout. A float multiply would need a lossy cast
/// back to `u64`, which the workspace lints forbid and which would give
/// byte counts binary-float rounding error for no benefit.
fn parse_size(input: &str) -> Option<u64> {
    let cleaned = input.replace(',', "").trim().to_uppercase();

    let digits: String = cleaned
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();

    let (whole_str, frac_raw) = digits.split_once('.').unwrap_or((digits.as_str(), ""));

    let whole = whole_str.parse::<u64>().ok()?;

    // Bound the fraction so `10u64.pow(places)` cannot overflow. The
    // characters are guaranteed ASCII digits by the `take_while` above, so
    // slicing on a byte index is safe.
    // A second dot means the input is malformed (`1.2.3 GB`). Fail closed
    // rather than silently reading it as `1 GB`.
    if frac_raw.contains('.') {
        return None;
    }
    let frac_str = &frac_raw[..frac_raw.len().min(6)];
    let frac: u64 = if frac_str.is_empty() {
        0
    } else {
        frac_str.parse().ok()?
    };
    let places = u32::try_from(frac_str.len()).unwrap_or(0);

    let unit: String = cleaned
        .chars()
        .skip_while(|c| c.is_ascii_digit() || *c == '.' || c.is_whitespace())
        .collect();

    let multiplier: u64 = match unit.trim() {
        u if u.starts_with("TB") || u.starts_with("TIB") => 1024_u64.pow(4),
        u if u.starts_with("GB") || u.starts_with("GIB") => 1024_u64.pow(3),
        u if u.starts_with("MB") || u.starts_with("MIB") => 1024_u64.pow(2),
        u if u.starts_with("KB") || u.starts_with("KIB") => 1024,
        _ => 1,
    };

    let scale = 10_u64.pow(places);
    Some(
        whole
            .saturating_mul(multiplier)
            .saturating_add(frac.saturating_mul(multiplier) / scale),
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::model::parse_definition;
    use chrono::TimeZone;

    const DEF: &str = r"
id: simple
name: Simple
search:
  paths:
    - path: search
  rows:
    selector: tr.result
  fields:
    title:
      selector: td.name a
    details:
      selector: td.name a
      attribute: href
    download:
      selector: td.dl a
      attribute: href
    seeders:
      selector: td.seeds
    leechers:
      selector: td.leech
    size:
      selector: td.size
";

    fn releases() -> Vec<Release> {
        let def = parse_definition(DEF).unwrap();
        let html = include_str!("../tests/fixtures/simple_tracker.html");
        extract(&def, html, &BTreeMap::new(), &FilterCtx::fixed_for_tests()).unwrap()
    }

    #[test]
    fn extracts_one_release_per_row() {
        assert_eq!(releases().len(), 2);
    }

    #[test]
    fn reads_element_text_for_title() {
        assert_eq!(releases()[0].title, "Big.Buck.Bunny.2008.1080p.BluRay.x264");
    }

    #[test]
    fn reads_attributes_when_specified() {
        assert_eq!(
            releases()[1].download_url.as_deref(),
            Some("/download/2.torrent")
        );
        assert_eq!(releases()[1].details_url.as_deref(), Some("/details/2"));
    }

    #[test]
    fn parses_numeric_fields() {
        assert_eq!(releases()[0].seeders, Some(120));
        assert_eq!(releases()[0].leechers, Some(7));
    }

    #[test]
    fn parses_human_readable_sizes() {
        assert_eq!(releases()[1].size, Some(700 * 1024 * 1024));
        assert_eq!(releases()[0].size, Some(1_503_238_553));
    }

    #[test]
    fn unreadable_size_is_none_not_zero() {
        // A tracker that reports no size must not be indistinguishable from
        // one reporting a zero-byte release.
        assert_eq!(parse_size(""), None);
        assert_eq!(parse_size("N/A"), None);
    }

    // `case:` is the only extraction rule for the volume factors in ~55% of
    // the corpus, so these three tests are the ones that matter most.

    #[test]
    fn case_takes_the_first_matching_selector() {
        // A row containing the freeleech marker must resolve to 0, while a
        // row without it falls through to the '*' catch-all and resolves to 1.
        let yaml = r#"
id: simple
name: Simple
search:
  rows:
    selector: tr.result
  fields:
    title:
      selector: td.name
    downloadvolumefactor:
      case:
        'img[src$="/freeleech.gif"]': 0
        '*': 1
"#;
        let html = r#"<table>
<tr class="result"><td class="name">A</td><td class="dl"><img src="/freeleech.gif"></td></tr>
<tr class="result"><td class="name">B</td><td class="dl"></td></tr>
</table>"#;
        let def = parse_definition(yaml).unwrap();
        let releases =
            extract(&def, html, &BTreeMap::new(), &FilterCtx::fixed_for_tests()).unwrap();
        assert!((releases[0].download_volume_factor - 0.0).abs() < f32::EPSILON);
        assert!((releases[1].download_volume_factor - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn case_wildcard_is_only_reached_when_nothing_else_matches() {
        // With '*' declared FIRST in the YAML, Cardigann still evaluates in
        // document order, so '*' wins for every row. Assert that, so a future
        // change to an unordered map is caught here rather than in production.
        let yaml = r#"
id: simple
name: Simple
search:
  rows:
    selector: tr.result
  fields:
    title:
      selector: td.name
    downloadvolumefactor:
      case:
        '*': 1
        'img[src$="/freeleech.gif"]': 0
"#;
        let html = r#"<table>
<tr class="result"><td class="name">A</td><td class="dl"><img src="/freeleech.gif"></td></tr>
<tr class="result"><td class="name">B</td><td class="dl"></td></tr>
</table>"#;
        let def = parse_definition(yaml).unwrap();
        let releases =
            extract(&def, html, &BTreeMap::new(), &FilterCtx::fixed_for_tests()).unwrap();
        assert!((releases[0].download_volume_factor - 1.0).abs() < f32::EPSILON);
        assert!((releases[1].download_volume_factor - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn default_substitutes_only_when_the_result_is_empty() {
        // A field whose selector matches nothing must fall back to `default:`;
        // a field whose selector matches must ignore `default:`.
        let yaml = r"
id: simple
name: Simple
search:
  rows:
    selector: tr.result
  fields:
    title:
      selector: td.name
    genre:
      selector: td.missing
      default: Unknown
    description:
      selector: td.name
      default: Unknown
";
        let html = r#"<table><tr class="result"><td class="name">A</td></tr></table>"#;
        let def = parse_definition(yaml).unwrap();
        let releases =
            extract(&def, html, &BTreeMap::new(), &FilterCtx::fixed_for_tests()).unwrap();
        assert_eq!(releases[0].genre.as_deref(), Some("Unknown"));
        assert_eq!(releases[0].description.as_deref(), Some("A"));
    }

    #[test]
    fn remove_strips_matching_elements_before_reading_text() {
        // A title cell containing a nested `<span class="tag">NEW</span>` with
        // `remove: span.tag` must yield the title without "NEW".
        let yaml = r"
id: simple
name: Simple
search:
  rows:
    selector: tr.result
  fields:
    title:
      selector: td.name
      remove: span.tag
";
        let html = r#"<table><tr class="result"><td class="name">Big Buck Bunny<span class="tag"> NEW</span></td></tr></table>"#;
        let def = parse_definition(yaml).unwrap();
        let releases =
            extract(&def, html, &BTreeMap::new(), &FilterCtx::fixed_for_tests()).unwrap();
        assert_eq!(releases[0].title, "Big Buck Bunny");
    }

    #[test]
    fn remove_strips_elements_nested_several_levels_deep() {
        let yaml = r"
id: simple
name: Simple
search:
  rows:
    selector: tr.result
  fields:
    title:
      selector: td.name
      remove: span.tag
";
        let html = r#"<table><tr class="result"><td class="name">Big Buck<div><em><span class="tag"> NEW</span></em></div> Bunny</td></tr></table>"#;
        let def = parse_definition(yaml).unwrap();
        let releases =
            extract(&def, html, &BTreeMap::new(), &FilterCtx::fixed_for_tests()).unwrap();
        assert_eq!(releases[0].title, "Big Buck Bunny");
    }

    #[test]
    fn remove_strips_multiple_sibling_elements() {
        let yaml = r"
id: simple
name: Simple
search:
  rows:
    selector: tr.result
  fields:
    title:
      selector: td.name
      remove: span.tag
";
        let html = r#"<table><tr class="result"><td class="name"><span class="tag">[X]</span>Big<span class="tag">[Y]</span> Buck<span class="tag">[Z]</span> Bunny</td></tr></table>"#;
        let def = parse_definition(yaml).unwrap();
        let releases =
            extract(&def, html, &BTreeMap::new(), &FilterCtx::fixed_for_tests()).unwrap();
        assert_eq!(releases[0].title, "Big Buck Bunny");
    }

    #[test]
    fn malformed_size_is_none_rather_than_a_wrong_guess() {
        // `1.2.3 GB` must not silently read as `1 GB`.
        assert_eq!(parse_size("1.2.3 GB"), None);
        assert_eq!(parse_size(""), None);
        assert_eq!(parse_size("N/A"), None);
        assert_eq!(parse_size("700 MB"), Some(700 * 1024 * 1024));
    }

    #[test]
    fn a_date_field_with_no_rfc3339_producing_filter_leaves_publish_date_none() {
        // A `date` field with no `dateparse`/`timeparse`/`timeago`/
        // `fuzzytime` filter carries raw, tracker-formatted text that
        // cannot be turned into a `DateTime<Utc>` without guessing a
        // format, so `publish_date` must stay `None` rather than be
        // populated from a guess.
        //
        // `categories` is a separate KNOWN LIMITATION, not asserted
        // behaviour: it needs the `caps.categorymappings` subsystem, which
        // does not exist yet. This test also pins that until it is built.
        let yaml = r"
id: simple
name: Simple
search:
  rows:
    selector: tr.result
  fields:
    title:
      selector: td.name
    date:
      selector: td.date
    category:
      selector: td.cat
";
        let html = r#"<table><tr class="result"><td class="name">A</td><td class="date">2024-01-01</td><td class="cat">2000</td></tr></table>"#;
        let def = parse_definition(yaml).unwrap();
        let releases =
            extract(&def, html, &BTreeMap::new(), &FilterCtx::fixed_for_tests()).unwrap();
        assert_eq!(releases[0].title, "A");
        assert!(
            releases[0].publish_date.is_none(),
            "a date field's raw value, with no RFC 3339-producing filter, must not populate publish_date"
        );
        assert!(
            releases[0].categories.is_empty(),
            "this definition declares no caps.categorymappings, so its raw category value (\"2000\") has nothing to resolve against"
        );
    }

    #[test]
    fn categories_resolve_through_the_definitions_categorymappings() {
        // Two rows, two distinct tracker category ids, both mapped by the
        // definition's own `caps.categorymappings` — the RESULT-level proof
        // that `build_release` actually wires the row's `category` field
        // through `CategoryMap::to_newznab`, not just that the plumbing
        // compiles.
        let yaml = r#"
id: simple
name: Simple
caps:
  categorymappings:
    - {id: "6", cat: Movies/HD, desc: "Movies HD"}
    - {id: "9", cat: TV/SD, desc: "TV SD"}
search:
  rows:
    selector: tr.result
  fields:
    title:
      selector: td.name
    category:
      selector: td.cat
"#;
        let html = r#"<table>
<tr class="result"><td class="name">A</td><td class="cat">6</td></tr>
<tr class="result"><td class="name">B</td><td class="cat">9</td></tr>
<tr class="result"><td class="name">C</td><td class="cat">99</td></tr>
</table>"#;
        let def = parse_definition(yaml).unwrap();
        let releases =
            extract(&def, html, &BTreeMap::new(), &FilterCtx::fixed_for_tests()).unwrap();
        assert_eq!(
            releases[0].categories,
            vec![2040],
            "tracker id 6 -> Movies/HD"
        );
        assert_eq!(releases[1].categories, vec![5030], "tracker id 9 -> TV/SD");
        assert!(
            releases[2].categories.is_empty(),
            "tracker id 99 has no categorymappings row, so it must contribute nothing"
        );
    }

    #[test]
    fn publish_date_is_populated_when_the_date_field_parses_as_rfc3339() {
        // The `date` field's filter chain ends in `dateparse`, which (per
        // Tasks 3-4) emits an RFC 3339 string on success. That final value
        // must land in `Release.publish_date` as a real `DateTime<Utc>`.
        let yaml = r"
id: simple
name: Simple
search:
  rows:
    selector: tr.result
  fields:
    title:
      selector: td.name
    date:
      selector: td.date
      filters:
        - name: dateparse
          args: yyyy-MM-dd
";
        let html = r#"<table><tr class="result"><td class="name">A</td><td class="date">2025-03-09</td></tr></table>"#;
        let def = parse_definition(yaml).unwrap();
        let releases =
            extract(&def, html, &BTreeMap::new(), &FilterCtx::fixed_for_tests()).unwrap();
        assert_eq!(
            releases[0].publish_date,
            Some(Utc.with_ymd_and_hms(2025, 3, 9, 0, 0, 0).unwrap())
        );
    }

    #[test]
    fn fields_evaluate_in_declaration_order_not_alphabetical_order() {
        // Cardigann fields reference earlier fields through `.Result.<name>`,
        // so evaluation must follow DECLARATION order. Sorting by name breaks
        // it: this is the real shape used by 1337x.yml, which declares
        // `title_default` and `title_optional` before `title` — and "title"
        // sorts before both, so an alphabetical pass evaluates the consumer
        // first and every `.Result.*` lookup resolves to the empty string.
        let yaml = r"
id: simple
name: Simple
search:
  rows:
    selector: tr.result
  fields:
    title_default:
      selector: td.fallback
    title_optional:
      selector: td.preferred
    title:
      text: '{{ if .Result.title_optional }}{{ .Result.title_optional }}{{ else }}{{ .Result.title_default }}{{ end }}'
";
        let html = r#"<table>
            <tr class="result"><td class="fallback">Fallback</td><td class="preferred">Preferred</td></tr>
            <tr class="result"><td class="fallback">OnlyFallback</td></tr>
        </table>"#;
        let def = parse_definition(yaml).unwrap();
        let releases =
            extract(&def, html, &BTreeMap::new(), &FilterCtx::fixed_for_tests()).unwrap();
        assert_eq!(releases[0].title, "Preferred");
        assert_eq!(releases[1].title, "OnlyFallback");
    }

    #[test]
    fn andmatch_drops_rows_that_fail_to_match_every_keyword() {
        // End-to-end through the 'rows: loop: a field's filter chain
        // producing `FilterOutcome::DropRow` must skip that row entirely,
        // not just leave the field empty.
        let yaml = r"
id: simple
name: Simple
search:
  rows:
    selector: tr.result
  fields:
    title:
      selector: td.name
      filters:
        - name: andmatch
";
        let html = r#"<table>
<tr class="result"><td class="name">Ubuntu 24.04 Server</td></tr>
<tr class="result"><td class="name">Ubuntu 24.04 Desktop</td></tr>
</table>"#;
        let def = parse_definition(yaml).unwrap();
        let mut ctx = FilterCtx::fixed_for_tests();
        ctx.keywords = vec!["ubuntu".into(), "server".into()];
        let releases = extract(&def, html, &BTreeMap::new(), &ctx).unwrap();
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].title, "Ubuntu 24.04 Server");
    }

    #[test]
    fn a_json_response_definition_is_rejected_even_with_a_css_shaped_row_selector() {
        // The shape that matters: 95 of the corpus's 101 JSON definitions use
        // a bare-identifier row selector like `data`, which parses as a
        // perfectly valid CSS tag selector. Inferring the mode from selector
        // shape catches only 6 of them; the rest would run against a parsed
        // HTML document, match nothing, and return an empty result that looks
        // exactly like a search with no hits.
        let yaml = r"
id: apiish
name: API-ish
search:
  paths:
    - path: api/torrents
      response:
        type: json
  rows:
    selector: data
  fields:
    title:
      selector: name
";
        let def = parse_definition(yaml).unwrap();
        let message = match extract(
            &def,
            r#"{"data":[{"name":"A"}]}"#,
            &BTreeMap::new(),
            &FilterCtx::fixed_for_tests(),
        ) {
            Ok(releases) => format!("accepted, returning {} releases", releases.len()),
            Err(e) => e.to_string(),
        };
        assert!(
            message.contains("JSON"),
            "a JSON definition must be rejected with an error naming the cause, got: {message}"
        );
    }

    #[test]
    fn a_json_field_path_selector_is_a_clear_error_not_a_silent_empty_value() {
        // `selector::compile` recognises Cardigann's JSON field-path grammar
        // (`$`, `$.field`, `items[0].name`, ...), used by definitions whose
        // search response is JSON rather than HTML. This extraction path
        // only understands HTML, so a JSON field-path selector here must be
        // a loud error, never a silently empty value indistinguishable from
        // "matched nothing".
        let yaml = r"
id: simple
name: Simple
search:
  rows:
    selector: tr.result
  fields:
    title:
      selector: $.torrents[0].id
";
        let html = r#"<table><tr class="result"><td class="name">A</td></tr></table>"#;
        let def = parse_definition(yaml).unwrap();
        let err = extract(&def, html, &BTreeMap::new(), &FilterCtx::fixed_for_tests()).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("$.torrents[0].id"),
            "error should name the offending selector: {message}"
        );
    }
}
