//! Rewrites jQuery-flavoured Cardigann selectors into standard CSS plus
//! text predicates.
//!
//! The `selectors` crate implements no `:contains()` pseudo-class, and over
//! half of the definition corpus depends on it. Each occurrence is lifted
//! out of the CSS string — however deeply it is nested inside `:has(...)`
//! and/or `:not(...)` — and evaluated as a predicate over matched elements
//! instead. `:has()` and `:not()` themselves are left untouched whenever
//! their argument contains no `:contains()`, since `scraper` supports both
//! natively.
//!
//! Some Cardigann definitions search a JSON API rather than an HTML page.
//! For those, the `selector:` field is not CSS at all but a small
//! dot/bracket field-path grammar (`$`, `$.field`, `..field`,
//! `files[0].name`). [`compile`] recognises that grammar and produces a
//! [`CompiledSelector`] that simply selects nothing against an HTML
//! document — JSON field extraction is handled by a separate code path.

use crate::error::CardigannError;
use scraper::{ElementRef, Selector};
use std::collections::HashSet;

/// A text condition lifted out of a `:contains()` clause.
#[derive(Debug)]
enum TextPredicate {
    /// The current element's own (flattened) text must, unless `negate`,
    /// contain `text`.
    Contains { text: String, negate: bool },
    /// Some descendant matching `css` must, unless `negate`, satisfy every
    /// predicate in `nested`.
    Descendant {
        css: Selector,
        nested: Vec<TextPredicate>,
        negate: bool,
    },
}

impl TextPredicate {
    fn negated(self) -> Self {
        match self {
            TextPredicate::Contains { text, negate } => TextPredicate::Contains {
                text,
                negate: !negate,
            },
            TextPredicate::Descendant {
                css,
                nested,
                negate,
            } => TextPredicate::Descendant {
                css,
                nested,
                negate: !negate,
            },
        }
    }

    fn matches(&self, el: ElementRef<'_>) -> bool {
        match self {
            TextPredicate::Contains { text, negate } => {
                element_text(el).contains(text.as_str()) != *negate
            }
            TextPredicate::Descendant {
                css,
                nested,
                negate,
            } => {
                let found = el.select(css).any(|d| nested.iter().all(|p| p.matches(d)));
                found != *negate
            }
        }
    }
}

fn element_text(el: ElementRef<'_>) -> String {
    el.text().collect::<String>()
}

/// One branch of a (possibly comma-separated) selector list: standard CSS
/// plus the text predicates that must also hold.
#[derive(Debug)]
struct Branch {
    css: Selector,
    predicates: Vec<TextPredicate>,
}

/// What a compiled selector actually does when asked to select elements.
#[derive(Debug)]
enum SelectorKind {
    /// Ordinary CSS (rewritten from jQuery-flavoured Cardigann CSS).
    Css(Vec<Branch>),
    /// A JSON field-path selector (`$`, `..field`, `items[0].name`, ...),
    /// used when the search response is JSON rather than HTML. It is not
    /// CSS and never matches anything in an HTML document; JSON field
    /// extraction happens elsewhere.
    JsonPath,
}

#[derive(Debug)]
pub struct CompiledSelector {
    kind: SelectorKind,
}

impl CompiledSelector {
    /// True when this selector is a JSON field-path selector (see
    /// [`is_json_path`]) rather than CSS. An HTML extraction path must check
    /// this before calling [`select`](Self::select): a `JsonPath` selector
    /// always selects nothing against an HTML document, and treating that
    /// silence as "the selector legitimately matched zero elements" would
    /// hide a definition/engine mismatch instead of surfacing it.
    #[must_use]
    pub fn is_json_path(&self) -> bool {
        matches!(self.kind, SelectorKind::JsonPath)
    }

    /// Selects descendants of `root` matching the CSS, then applies
    /// predicates. Elements are de-duplicated and returned in document
    /// order.
    #[must_use]
    pub fn select<'a>(&self, root: ElementRef<'a>) -> Vec<ElementRef<'a>> {
        let branches = match &self.kind {
            SelectorKind::Css(branches) => branches,
            SelectorKind::JsonPath => return Vec::new(),
        };

        let mut seen = HashSet::new();
        let mut matches = Vec::new();
        for branch in branches {
            for el in root.select(&branch.css) {
                if branch.predicates.iter().all(|p| p.matches(el)) && seen.insert(el.id()) {
                    matches.push(el);
                }
            }
        }
        matches.sort_by_key(|el| el.id());
        matches
    }
}

/// Splits a selector into standard CSS plus lifted text predicates.
///
/// # Errors
///
/// Returns [`CardigannError::Selector`] if, after rewriting `:contains()`,
/// `:has()`, `:not()` and the jQuery-only `!=` attribute operator into
/// standard CSS, the result still fails to parse. Returns
/// [`CardigannError::UnbalancedContains`] if a `:contains(`, `:has(` or
/// `:not(` clause is never closed.
pub fn compile(raw: &str) -> Result<CompiledSelector, CardigannError> {
    let trimmed = raw.trim();
    if is_json_path(trimmed) {
        return Ok(CompiledSelector {
            kind: SelectorKind::JsonPath,
        });
    }

    let normalized = rewrite_not_equal(raw);
    let normalized = normalize_attribute_selectors(&normalized);
    // A leading `$` that isn't a JSON root path (handled above) has no
    // meaning in CSS — it can't be part of an attribute's `$=` operator,
    // since nothing precedes it — and shows up in a couple of definitions
    // as a stray leftover. Drop it.
    let normalized = normalized.strip_prefix('$').unwrap_or(&normalized);
    let branches = split_top_level_commas(normalized)
        .into_iter()
        .map(|part| compile_branch(part, raw))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(CompiledSelector {
        kind: SelectorKind::Css(branches),
    })
}

/// Compiles a single (comma-free) selector branch.
fn compile_branch(part: &str, raw: &str) -> Result<Branch, CardigannError> {
    let (css_text, predicates) = rewrite(part, raw)?;
    let css_text = css_text.trim();
    let css_text = if css_text.is_empty() { "*" } else { css_text };

    let css = Selector::parse(css_text).map_err(|e| CardigannError::Selector {
        selector: raw.to_string(),
        reason: format!("{e:?}"),
    })?;

    Ok(Branch { css, predicates })
}

/// The pseudo-class calls that get special handling: `:contains()` is
/// removed entirely and lifted into a predicate; `:has()` and `:not()` are
/// recursed into so any `:contains()` nested inside them is found too.
#[derive(Clone, Copy)]
enum Marker {
    Contains,
    Has,
    Not,
}

impl Marker {
    const fn token(self) -> &'static str {
        match self {
            Marker::Contains => ":contains(",
            Marker::Has => ":has(",
            Marker::Not => ":not(",
        }
    }
}

const MARKERS: [Marker; 3] = [Marker::Contains, Marker::Has, Marker::Not];

/// Finds the earliest occurrence, in `s`, of any marker token.
fn find_earliest(s: &str) -> Option<(usize, Marker)> {
    MARKERS
        .into_iter()
        .filter_map(|m| s.find(m.token()).map(|idx| (idx, m)))
        .min_by_key(|(idx, _)| *idx)
}

/// Recursively strips `:contains(...)` out of `s` — including occurrences
/// nested inside `:has(...)` and `:not(...)` — leaving standard CSS behind
/// plus the predicates that were lifted out. `:has()`/`:not()` clauses
/// whose argument contains no `:contains()` are left exactly as they were,
/// since `scraper` supports them natively.
///
/// `raw` is the original, whole selector string, kept only for error
/// messages.
fn rewrite(s: &str, raw: &str) -> Result<(String, Vec<TextPredicate>), CardigannError> {
    let mut output = String::with_capacity(s.len());
    let mut predicates = Vec::new();
    let mut rest = s;

    while let Some((idx, marker)) = find_earliest(rest) {
        output.push_str(&rest[..idx]);
        let token = marker.token();
        let after = &rest[idx + token.len()..];
        let close =
            find_matching_paren(after).ok_or_else(|| CardigannError::UnbalancedContains {
                selector: raw.to_string(),
            })?;
        let arg = &after[..close];

        match marker {
            Marker::Contains => {
                predicates.push(TextPredicate::Contains {
                    text: unquote(arg.trim()),
                    negate: false,
                });
            }
            Marker::Has => {
                let (inner_css, inner_predicates) = rewrite(arg, raw)?;
                let inner_css = inner_css.trim();
                // The CSS spec forbids `:has()` inside another `:has()`'s
                // relative selector list, and `scraper` enforces that — so
                // a `:has()` that still contains a nested `:has(` can't be
                // re-embedded natively even when there was no `:contains()`
                // to lift. Promote it to a `Descendant` predicate instead,
                // which parses the nested selector fresh (not nested inside
                // another `:has()`) and so is unaffected by the rule.
                let needs_lift = !inner_predicates.is_empty() || inner_css.contains(":has(");
                if needs_lift {
                    // The lifted predicate already requires a matching
                    // descendant to exist, so the native `:has()` would be
                    // redundant.
                    let scope = if inner_css.is_empty() { "*" } else { inner_css };
                    let scope_css =
                        Selector::parse(scope).map_err(|e| CardigannError::Selector {
                            selector: raw.to_string(),
                            reason: format!("{e:?}"),
                        })?;
                    predicates.push(TextPredicate::Descendant {
                        css: scope_css,
                        nested: inner_predicates,
                        negate: false,
                    });
                } else if !inner_css.is_empty() {
                    // No `:contains()` or nested `:has()` inside: keep
                    // `:has(...)` untouched, unless it is empty — some
                    // definitions contain a literal `:has()`, which is
                    // invalid CSS and is simply dropped.
                    output.push_str(":has(");
                    output.push_str(inner_css);
                    output.push(')');
                }
            }
            Marker::Not => {
                let (inner_css, inner_predicates) = rewrite(arg, raw)?;
                let inner_css = inner_css.trim();
                if inner_predicates.is_empty() {
                    // Purely structural negation: keep `:not(...)` untouched,
                    // unless it is empty (see the `:has()` case above).
                    if !inner_css.is_empty() {
                        output.push_str(":not(");
                        output.push_str(inner_css);
                        output.push(')');
                    }
                } else {
                    // `:not(:contains(...))` or `:not(:has(...:contains(...)))`:
                    // negate the lifted predicates. If any structural part
                    // of the `:not()` argument survived, keep it too.
                    if !inner_css.is_empty() {
                        output.push_str(":not(");
                        output.push_str(inner_css);
                        output.push(')');
                    }
                    for predicate in inner_predicates {
                        predicates.push(predicate.negated());
                    }
                }
            }
        }

        rest = &after[close + 1..];
    }
    output.push_str(rest);

    Ok((output, predicates))
}

/// Returns the byte index of the `)` matching the already-consumed `(`,
/// skipping parens that fall inside a quoted string. A backslash inside a
/// quoted string escapes the next character (so `\"` does not end the
/// string), matching CSS string-literal syntax.
fn find_matching_paren(s: &str) -> Option<usize> {
    let mut depth = 1usize;
    let mut quote = None;
    let mut chars = s.char_indices();
    while let Some((i, c)) = chars.next() {
        if let Some(q) = quote {
            if c == '\\' {
                chars.next();
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Splits `s` on commas that are not nested inside parentheses and not
/// inside a quoted string.
fn split_top_level_commas(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut quote = None;
    let mut start = 0usize;
    let mut chars = s.char_indices();
    while let Some((i, c)) = chars.next() {
        if let Some(q) = quote {
            if c == '\\' {
                chars.next();
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&s[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

/// Strips one layer of matching surrounding quotes, if present, then
/// decodes CSS escape sequences in the remaining text.
fn unquote(s: &str) -> String {
    let bytes = s.as_bytes();
    let inner = if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' || first == b'\'') && first == last {
            &s[1..s.len() - 1]
        } else {
            s
        }
    } else {
        s
    };
    css_unescape(inner)
}

/// Decodes CSS escape sequences: a backslash followed by 1-6 hex digits
/// (and one optional trailing whitespace character) becomes the
/// corresponding Unicode scalar value; a backslash followed by anything
/// else becomes that character literally.
fn css_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let mut hex = String::new();
        while hex.len() < 6 {
            match chars.peek() {
                Some(h) if h.is_ascii_hexdigit() => {
                    hex.push(*h);
                    chars.next();
                }
                _ => break,
            }
        }
        if hex.is_empty() {
            if let Some(next) = chars.next() {
                out.push(next);
            }
            continue;
        }
        if matches!(chars.peek(), Some(w) if w.is_whitespace()) {
            chars.next();
        }
        match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
            Some(ch) => out.push(ch),
            None => out.push_str(&hex),
        }
    }
    out
}

/// Returns the byte index of the first unquoted occurrence of `needle` in
/// `s`. Text inside a quoted string (single or double, with backslash
/// escapes, matching CSS string-literal syntax) is treated as opaque, so a
/// `!=`, `[`, or `]` that happens to appear inside e.g. a
/// `:contains("...")` argument is never mistaken for real selector syntax
/// belonging to an attribute selector that comes after it.
fn find_unquoted(s: &str, needle: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    let mut chars = s.char_indices();
    while let Some((i, c)) = chars.next() {
        // A backslash escapes the next character unconditionally — both
        // while inside a quoted string (where it's a genuine escaped
        // quote that must not end the string) and outside one (where a
        // couple of definitions write a spurious `\"`/`\'` as if a quote
        // needed the same escaping in every context; that pair must not
        // be mistaken for the start of a real quoted string either, or
        // the *next* real quote is misread as the opener and the string
        // never closes).
        if c == '\\' {
            chars.next();
            continue;
        }
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        if c == '"' || c == '\'' {
            quote = Some(c);
            continue;
        }
        if s[i..].starts_with(needle) {
            return Some(i);
        }
    }
    None
}

/// Rewrites the jQuery/Sizzle-only `!=` attribute operator (no such
/// operator exists in standard CSS) into the equivalent standard form:
/// `[attr!=value]` becomes `:not([attr=value])`.
///
/// Scans for `[...]` bodies one at a time — using [`find_unquoted`] for
/// both the brackets and the `!=` inside them — rather than searching for
/// `!=` globally and then guessing which brackets it belongs to. A global
/// search would mis-scope a `!=` that happens to appear inside a quoted
/// `:contains("...")` argument, attributing it to an unrelated `[...]`
/// elsewhere in the selector.
fn rewrite_not_equal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        let Some(open) = find_unquoted(rest, "[") else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..open]);
        let after_open = &rest[open + 1..];
        let Some(close) = find_unquoted(after_open, "]") else {
            out.push('[');
            out.push_str(after_open);
            return out;
        };
        let body = &after_open[..close];
        if let Some(bang) = find_unquoted(body, "!=") {
            out.push_str(":not([");
            out.push_str(&body[..bang]);
            out.push('=');
            out.push_str(&body[bang + 2..]);
            out.push_str("])");
        } else {
            out.push('[');
            out.push_str(body);
            out.push(']');
        }
        rest = &after_open[close + 1..];
    }
}

/// Normalizes every `[...]` attribute-selector body in `s` (see
/// [`normalize_attribute_value`]), scoped the same quote-aware way as
/// [`rewrite_not_equal`].
fn normalize_attribute_selectors(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        let Some(open) = find_unquoted(rest, "[") else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..=open]);
        rest = &rest[open + 1..];
        let Some(close) = find_unquoted(rest, "]") else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&normalize_attribute_value(&rest[..close]));
        out.push(']');
        rest = &rest[close + 1..];
    }
}

/// Normalizes a single `attr`, `attr=value`, `attr^=value`, ...
/// attribute-selector body, fixing two malformations observed in the
/// corpus:
///
/// - A value that *starts* with `\"` or `\'`. A value can never validly
///   begin with an escaped quote (there is nothing before it to escape),
///   so this only happens when a definition was written as if the whole
///   selector needed double-quoted-YAML-style escaping even though nothing
///   ever opens a real quote — e.g. `[src$=\"/x.gif\"]` instead of
///   `[src$="/x.gif"]`. Every backslash-quote pair in the body is spurious
///   in that case and is flattened to a plain quote.
/// - An unquoted value that starts with a digit, e.g. `[border=1]`. A bare
///   CSS identifier can't start with a digit, so `Selector::parse` rejects
///   it; `[border="1"]` is what was meant.
fn normalize_attribute_value(inner: &str) -> String {
    let Some(op_end) = inner.find('=') else {
        return inner.to_string();
    };
    let value = &inner[op_end + 1..];

    if value.starts_with("\\\"") || value.starts_with("\\'") {
        return inner.replace("\\\"", "\"").replace("\\'", "'");
    }
    if value.starts_with(|c: char| c.is_ascii_digit()) {
        return format!("{}\"{value}\"", &inner[..=op_end]);
    }
    inner.to_string()
}

/// Returns true if `s` is a JSON field-path selector (`$`, `$.field`,
/// `$[0].id`, `..field`, `items[0].name`) rather than a CSS selector. These
/// appear in Cardigann definitions whose search response is JSON.
///
/// Without a `$` or `..` prefix, only a numeric array index (`[0]`) is an
/// unambiguous signal: a bare `field.subfield` shape is indistinguishable
/// from an ordinary CSS class selector (`tr.result`, `td.name`, ...), which
/// is by far the most common selector shape in the HTML-response corpus.
/// Treating every dotted identifier as JSON — as an earlier version of this
/// function did — misclassified virtually every row/field selector of that
/// form and made every one of them silently select nothing.
fn is_json_path(s: &str) -> bool {
    if s == "$" {
        return true;
    }
    if let Some(rest) = s.strip_prefix("..") {
        return is_identifier(rest);
    }
    if let Some(tail) = s.strip_prefix('$') {
        return !tail.is_empty() && is_path_tail(tail);
    }
    s.contains('[') && is_path_tail(s)
}

fn is_identifier(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// A path tail is a sequence of `.identifier` and `[digits]` segments,
/// optionally starting with a bare identifier.
fn is_path_tail(mut s: &str) -> bool {
    loop {
        if s.is_empty() {
            return true;
        }
        if let Some(rest) = s.strip_prefix('.') {
            s = rest;
            continue;
        }
        if let Some(rest) = s.strip_prefix('[') {
            let Some(close) = rest.find(']') else {
                return false;
            };
            let (index, remainder) = rest.split_at(close);
            if index.is_empty() || !index.chars().all(|c| c.is_ascii_digit()) {
                return false;
            }
            s = &remainder[1..];
            continue;
        }
        let ident_len = s
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .count();
        if ident_len == 0 {
            return false;
        }
        s = &s[ident_len..];
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use scraper::Html;

    const HTML: &str = r#"<table>
        <tr id="a"><td class="c1"><a href="/torrent/1">Big Buck Bunny</a></td></tr>
        <tr id="b"><td class="c1"><a href="/torrent/2">Sintel</a></td></tr>
    </table>"#;

    fn ids(raw: &str) -> Vec<String> {
        let doc = Html::parse_document(HTML);
        let root = doc.root_element();
        compile(raw)
            .unwrap()
            .select(root)
            .iter()
            .map(|e| e.value().id().unwrap_or_default().to_string())
            .collect()
    }

    #[test]
    fn plain_css_is_unchanged() {
        assert_eq!(ids("tr"), vec!["a", "b"]);
    }

    #[test]
    fn self_contains_filters_by_text() {
        assert_eq!(ids("tr:contains(Bunny)"), vec!["a"]);
    }

    #[test]
    fn contains_nested_in_has() {
        assert_eq!(ids("tr:has(td:contains(Sintel))"), vec!["b"]);
    }

    #[test]
    fn quoted_contains_argument() {
        assert_eq!(ids(r#"tr:contains("Big Buck")"#), vec!["a"]);
    }

    #[test]
    fn has_without_contains_still_works() {
        assert_eq!(ids(r#"tr:has(a[href^="/torrent/"])"#), vec!["a", "b"]);
    }

    #[test]
    fn invalid_css_is_an_error() {
        assert!(compile("tr:::bogus(").is_err());
    }

    #[test]
    fn not_contains_negates_self() {
        assert_eq!(ids(r#"tr:not(:contains("Sintel"))"#), vec!["a"]);
    }

    #[test]
    fn not_has_contains_negates_descendant() {
        assert_eq!(ids(r#"tr:not(:has(td:contains("Sintel")))"#), vec!["a"]);
    }

    #[test]
    fn bare_contains_matches_universal_selector() {
        // A bare `:contains()` (no preceding type/class) behaves like
        // jQuery's own `:contains()`: it matches any element — including
        // ancestors — whose flattened text contains the string. `tr#b` is
        // one of the matches; `tr#a` never is.
        let matched = ids(r#":contains("Sintel")"#);
        assert!(matched.contains(&"b".to_string()));
        assert!(!matched.contains(&"a".to_string()));
    }

    #[test]
    fn comma_list_unions_branches() {
        assert_eq!(
            ids(r#"tr:contains("Bunny"), tr:contains("Sintel")"#),
            vec!["a", "b"]
        );
    }

    #[test]
    fn is_json_path_flags_json_field_paths_but_not_css() {
        assert!(compile("$.numFound").unwrap().is_json_path());
        assert!(compile("files[0].name").unwrap().is_json_path());
        assert!(!compile("tr.result").unwrap().is_json_path());
    }

    #[test]
    fn a_bare_class_selector_is_not_mistaken_for_a_json_path() {
        // `tr.result` is the single most common row selector shape in the
        // HTML-response corpus. Its `identifier.identifier` shape is
        // syntactically identical to a JSON dotted path with no `$` or `..`
        // prefix, so it must not be classified as JSON — that would make
        // every such selector silently (or, once callers check
        // `is_json_path`, loudly) fail to match anything.
        let doc =
            Html::parse_document(r#"<table><tr id="a" class="result"><td>x</td></tr></table>"#);
        let root = doc.root_element();
        let matched = compile("tr.result").unwrap().select(root);
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].value().attr("id"), Some("a"));
    }

    #[test]
    fn json_path_selectors_select_nothing() {
        assert_eq!(ids("$.numFound"), Vec::<String>::new());
        assert_eq!(ids("files[0].name"), Vec::<String>::new());
        assert_eq!(ids("..title"), Vec::<String>::new());
    }

    #[test]
    fn empty_contains_argument_matches_everything() {
        // A `:contains()` with no argument (left behind when a template
        // placeholder is stripped out before the value is known at render
        // time) must match every element of the given type, since an empty
        // string is contained in any text.
        let doc = Html::parse_document(HTML);
        let root = doc.root_element();
        let matched = compile("td:contains()").unwrap().select(root);
        assert_eq!(matched.len(), 2);
        assert!(matched.iter().all(|el| el.value().name() == "td"));
    }

    #[test]
    fn rewrite_not_equal_turns_bang_equals_into_not() {
        assert_eq!(
            rewrite_not_equal(r#"table[id!="torrent_ajanlo"] > tbody > tr[id]"#),
            r#"table:not([id="torrent_ajanlo"]) > tbody > tr[id]"#
        );
    }

    #[test]
    fn rewrite_not_equal_ignores_bang_equals_inside_quotes() {
        // A `!=` that happens to appear inside a quoted string (here, a
        // `:contains()` argument) must not be mistaken for the jQuery `!=`
        // attribute operator, and must not get attributed to an unrelated
        // `[...]` that comes later in the same selector.
        assert_eq!(
            rewrite_not_equal(r#"div:contains("x!=y")[title!="z"]"#),
            r#"div:contains("x!=y"):not([title="z"])"#
        );
    }

    #[test]
    fn bang_equals_inside_contains_does_not_corrupt_a_later_attribute() {
        // End-to-end regression test for the trap case above: compiling
        // and actually selecting against real HTML, not just checking
        // that the string parses.
        let html = r#"
            <div id="x1" title="z">has x!=y in its text</div>
            <div id="x2" title="w">has x!=y in its text</div>
        "#;
        let doc = Html::parse_document(html);
        let root = doc.root_element();
        let matched = compile(r#"div:contains("x!=y")[title!="z"]"#)
            .unwrap()
            .select(root);
        let ids: Vec<&str> = matched
            .iter()
            .map(|el| el.value().attr("id").unwrap_or_default())
            .collect();
        assert_eq!(ids, vec!["x2"]);
    }

    #[test]
    fn normalize_attribute_value_quotes_bare_numeric_value() {
        assert_eq!(normalize_attribute_value("border=1"), r#"border="1""#);
        // A value that isn't purely numeric, or is already quoted, is left
        // alone.
        assert_eq!(normalize_attribute_value("href^=index"), "href^=index");
        assert_eq!(normalize_attribute_value(r#"border="1""#), r#"border="1""#);
    }

    #[test]
    fn normalize_attribute_value_flattens_escaped_quote_only_value() {
        assert_eq!(
            normalize_attribute_value(r#"src$=\"/x.gif\""#),
            r#"src$="/x.gif""#
        );
    }

    #[test]
    fn css_unescape_decodes_hex_escape() {
        // `\00a0` is the CSS escape for U+00A0 NO-BREAK SPACE.
        assert_eq!(css_unescape(r"\00a0GB"), "\u{a0}GB");
        // A trailing whitespace character after the hex digits is part of
        // the escape sequence itself and is consumed, not kept literally.
        assert_eq!(css_unescape(r"\41 BC"), "ABC");
        // A backslash followed by a non-hex character is a literal escape,
        // not a hex decode.
        assert_eq!(css_unescape(r#"\"quoted\""#), r#""quoted""#);
    }

    #[test]
    fn unquote_applies_css_unescape_to_a_quoted_argument() {
        assert_eq!(unquote(r#""\00a0GB""#), "\u{a0}GB");
    }
}
