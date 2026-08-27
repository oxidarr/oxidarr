//! Executes a definition's `search` block against a response body.

use crate::error::CardigannError;
use crate::model::{Case, Definition, Field};
use crate::{filters, selector, template};
use oxidarr_core::Release;
use scraper::{ElementRef, Html, Node};
use std::collections::{BTreeMap, HashSet};

/// Runs a definition's row and field extraction over a response body.
///
/// Every field declared in the definition is evaluated for each row and
/// written into that row's scope as it is produced (under `.Result.<name>`),
/// so a later field's `text:` template can reference an earlier one's
/// value. Note that [`crate::model::Search::fields`] is a `BTreeMap`, so
/// "later" here means alphabetically-later-by-field-name, not the YAML
/// document's own declaration order — the document order is not preserved
/// through that map. This matches every corpus idiom actually observed
/// (fields referencing an internal `_foo`-prefixed helper field defined
/// alongside them), but is worth flagging if a future definition is found
/// to depend on true declaration order.
///
/// # Errors
///
/// Returns [`CardigannError`] if the rows selector, a field's `selector:`,
/// a `case:` key, or a `remove:` selector fails to compile (including when
/// any of them is a JSON field-path selector: this extraction path only
/// understands HTML, so that is treated as an error rather than silently
/// selecting nothing), or if a field's `text:` template or filter chain
/// fails.
pub fn extract(
    def: &Definition,
    body: &str,
    config: &BTreeMap<String, String>,
) -> Result<Vec<Release>, CardigannError> {
    let doc = Html::parse_document(body);
    let rows_selector = compile_html_selector(&def.search.rows.selector)?;

    let mut out = Vec::new();
    let skip = def.search.rows.after.unwrap_or(0);

    for row in rows_selector
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
            let raw = evaluate_field(field, row, &scope)?;
            scope.set_result(name, &raw);
            values.insert(name.clone(), raw);
        }
        out.push(build_release(&values));
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
fn evaluate_field(
    field: &Field,
    row: ElementRef<'_>,
    scope: &template::Scope,
) -> Result<String, CardigannError> {
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
        value = filters::apply(&filter, &value)?;
    }

    if value.is_empty()
        && let Some(default) = &field.default
    {
        value.clone_from(default);
    }

    Ok(value)
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
/// `date`/`categories` are deliberately left unmapped here: `dateparse`,
/// `timeparse`, `timeago` and `fuzzytime` are all still identity no-ops (no
/// clock has been wired into the engine yet), so a `date` field's raw
/// value at this point is unparsed tracker-formatted text, not something
/// that can be turned into a `DateTime<Utc>` without guessing a format.
/// Mapping it in regardless would either panic-free-but-wrong (silently
/// leave `publish_date` as `None`, no different from leaving it unmapped)
/// or require inventing ad hoc parsing that duplicates and likely
/// contradicts whatever the deferred date filters eventually do. Category
/// mapping (`caps.categorymappings`) is a similarly separate, not-yet-built
/// concern. Both are left to a later task.
fn build_release(values: &BTreeMap<String, String>) -> Release {
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
        extract(&def, html, &BTreeMap::new()).unwrap()
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
        let releases = extract(&def, html, &BTreeMap::new()).unwrap();
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
        let releases = extract(&def, html, &BTreeMap::new()).unwrap();
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
        let releases = extract(&def, html, &BTreeMap::new()).unwrap();
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
        let releases = extract(&def, html, &BTreeMap::new()).unwrap();
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
        let releases = extract(&def, html, &BTreeMap::new()).unwrap();
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
        let releases = extract(&def, html, &BTreeMap::new()).unwrap();
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
    fn date_and_categories_are_deliberately_unmapped_in_this_plan() {
        // Pinning a KNOWN LIMITATION, not asserting desired behaviour.
        //
        // `publish_date` needs the date filters (`dateparse`, `timeago`,
        // `fuzzytime`, `timeparse`), which are identity no-ops until the
        // engine is given a clock, and `categories` needs the
        // `caps.categorymappings` subsystem. Both land in the next plan.
        // Until then these two stay empty even when the definition extracts
        // them, so this test exists to make that visible and to fail loudly
        // the moment someone wires either one up without revisiting it.
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
        let releases = extract(&def, html, &BTreeMap::new()).unwrap();
        assert_eq!(releases[0].title, "A");
        assert!(
            releases[0].publish_date.is_none(),
            "publish_date is unmapped until the date filters get a clock"
        );
        assert!(
            releases[0].categories.is_empty(),
            "categories are unmapped until caps.categorymappings is implemented"
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
        let releases = extract(&def, html, &BTreeMap::new()).unwrap();
        assert_eq!(releases[0].title, "Preferred");
        assert_eq!(releases[1].title, "OnlyFallback");
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
        let err = extract(&def, html, &BTreeMap::new()).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("$.torrents[0].id"),
            "error should name the offending selector: {message}"
        );
    }
}
