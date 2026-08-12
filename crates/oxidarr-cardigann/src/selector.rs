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
    let branches = split_top_level_commas(&normalized)
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

/// Rewrites the jQuery/Sizzle-only `!=` attribute operator (no such
/// operator exists in standard CSS) into the equivalent standard form:
/// `[attr!=value]` becomes `:not([attr=value])`.
fn rewrite_not_equal(s: &str) -> String {
    let mut output = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(bang_idx) = rest.find("!=") {
        let open = rest[..bang_idx].rfind('[');
        let close = rest[bang_idx..].find(']').map(|i| bang_idx + i);
        let Some((open, close)) = open.zip(close) else {
            break;
        };
        output.push_str(&rest[..open]);
        output.push_str(":not([");
        output.push_str(&rest[open + 1..bang_idx]);
        output.push('=');
        output.push_str(&rest[bang_idx + 2..close]);
        output.push_str("])");
        rest = &rest[close + 1..];
    }
    output.push_str(rest);
    output
}

/// Returns true if `s` is a JSON field-path selector (`$`, `$.field`,
/// `$[0].id`, `..field`, `items[0].name`) rather than a CSS selector. These
/// appear in Cardigann definitions whose search response is JSON.
fn is_json_path(s: &str) -> bool {
    if s == "$" {
        return true;
    }
    if let Some(rest) = s.strip_prefix("..") {
        return is_identifier(rest);
    }
    let tail = s.strip_prefix('$').unwrap_or(s);
    if tail.is_empty() || !(tail.contains('.') || tail.contains('[')) {
        return false;
    }
    is_path_tail(tail)
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
    fn json_path_selectors_select_nothing() {
        assert_eq!(ids("$.numFound"), Vec::<String>::new());
        assert_eq!(ids("files[0].name"), Vec::<String>::new());
        assert_eq!(ids("..title"), Vec::<String>::new());
    }
}
