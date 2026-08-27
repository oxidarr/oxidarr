//! The 21 filter functions used by the Cardigann v11 corpus, ordered in the
//! enum by frequency of use.

use crate::error::CardigannError;
use regex::Regex;

/// A parsed filter invocation.
/// A compiled regex, backed by whichever engine can handle the pattern.
///
/// Cardigann definitions were written against .NET's regex engine, which
/// supports look-around (`(?<=\d)`, `(?![-_. ]?DL)`). Rust's `regex` crate
/// deliberately does not — it guarantees linear time and rejects those
/// constructs. 33 patterns across the corpus use look-around, so restricting
/// the engine to `regex` would leave 42 of 543 definitions with filters that
/// cannot compile at all.
///
/// `regex` is tried first so the common case keeps its linear-time guarantee,
/// and `fancy-regex` (which backtracks) is used only for the patterns `regex`
/// refuses.
#[derive(Debug, Clone)]
pub enum Pattern {
    /// Linear-time engine, used whenever the pattern permits it.
    Fast(Box<Regex>),
    /// Backtracking engine, used only for look-around and backreferences.
    Fancy(Box<fancy_regex::Regex>),
}

impl Pattern {
    /// Compiles `pattern`, preferring the linear-time engine.
    ///
    /// # Errors
    /// Returns an error only when BOTH engines reject the pattern.
    pub fn compile(pattern: &str) -> Result<Self, String> {
        match Regex::new(pattern) {
            Ok(re) => Ok(Self::Fast(Box::new(re))),
            Err(fast_err) => match fancy_regex::Regex::new(pattern) {
                Ok(re) => Ok(Self::Fancy(Box::new(re))),
                Err(fancy_err) => Err(format!("{fast_err} (fancy-regex: {fancy_err})")),
            },
        }
    }

    /// Replaces every match, expanding `$1`-style backreferences.
    #[must_use]
    pub fn replace_all(&self, input: &str, replacement: &str) -> String {
        match self {
            Self::Fast(re) => re.replace_all(input, replacement).into_owned(),
            Self::Fancy(re) => re.replace_all(input, replacement).into_owned(),
        }
    }

    /// Returns the first capture group, or the whole match when there is none.
    #[must_use]
    pub fn first_group(&self, input: &str) -> Option<String> {
        match self {
            Self::Fast(re) => re.captures(input).and_then(|c| {
                c.get(1)
                    .or_else(|| c.get(0))
                    .map(|m| m.as_str().to_string())
            }),
            Self::Fancy(re) => re.captures(input).ok().flatten().and_then(|c| {
                c.get(1)
                    .or_else(|| c.get(0))
                    .map(|m| m.as_str().to_string())
            }),
        }
    }
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Filter {
    /// Regex substitution with capture-group backreferences (`$1`, …).
    ReReplace {
        /// The compiled pattern to match against the input.
        pattern: Pattern,
        /// The replacement text, which may reference capture groups.
        replacement: String,
    },
    /// Literal (non-regex) substring substitution.
    Replace {
        /// The literal substring to search for.
        from: String,
        /// The literal substring to substitute in its place.
        to: String,
    },
    /// Appends a literal suffix to the input.
    Append(String),
    /// Prepends a literal prefix to the input.
    Prepend(String),
    /// Parses the input as a date using the given Go time layout. Deferred
    /// to the engine, which owns the clock.
    DateParse(String),
    /// Parses the input as a time-of-day using the given Go time layout.
    /// Deferred to the engine, which owns the clock.
    TimeParse(String),
    /// Extracts the first capture group (or the whole match) of a regex.
    Regexp(Pattern),
    /// Reads a single query-string parameter from a URL or query string.
    QueryString(String),
    /// A ROW-inclusion filter in real Cardigann: it keeps only result rows
    /// whose text contains every search keyword. That is not a value
    /// transform (`&str -> String`), so `apply`'s current signature cannot
    /// express it at all — implementing it for real requires changing
    /// that signature, not filling in a match arm. See the comment on its
    /// `apply` arm for the corpus impact of leaving it a no-op today.
    AndMatch,
    /// Splits the input on a separator and keeps one indexed part.
    Split {
        /// The separator to split on.
        sep: String,
        /// The index of the part to keep. Negative counts back from the
        /// end (Cardigann/Go convention: `-1` is the last part).
        index: i64,
    },
    /// Trims whitespace, or the given characters, from both ends.
    Trim(Option<String>),
    /// Parses a relative "time ago" phrase. Deferred to the engine, which
    /// owns the clock.
    TimeAgo,
    /// Parses a fuzzy/approximate time phrase. Deferred to the engine,
    /// which owns the clock.
    FuzzyTime,
    /// Keeps only the whitelisted tokens the input has in common with a
    /// comma/space/punctuation-separated whitelist argument (case folded),
    /// in the whitelist's order. Used by the corpus as a genre/category
    /// sanitiser.
    Validate(String),
    /// Decodes HTML entities.
    HtmlDecode,
    /// Strips characters that are not valid in a filename.
    ValidFilename,
    /// Percent-decodes the input.
    UrlDecode,
    /// Percent-encodes the input.
    UrlEncode,
    /// Folds accented Latin characters to their unaccented equivalents.
    Diacritics,
    /// Converts to uppercase.
    ToUpper,
    /// Converts to lowercase.
    ToLower,
}

/// Builds a [`Filter`] from a definition's `{name, args}` pair.
///
/// # Errors
///
/// Returns [`CardigannError::UnknownFilter`] if `name` is not one of the
/// filters this engine implements, or [`CardigannError::Filter`] if the
/// filter is known but its arguments are unusable (for example, an invalid
/// regex pattern).
pub fn parse_filter(name: &str, args: &[String]) -> Result<Filter, CardigannError> {
    let arg = |i: usize| args.get(i).cloned().unwrap_or_default();
    let compile = |p: &str| {
        Pattern::compile(p).map_err(|e| CardigannError::Filter {
            name: name.to_string(),
            reason: format!("invalid regex: {e}"),
        })
    };

    Ok(match name {
        "re_replace" => Filter::ReReplace {
            pattern: compile(&arg(0))?,
            replacement: arg(1),
        },
        "replace" => Filter::Replace {
            from: arg(0),
            to: arg(1),
        },
        "append" => Filter::Append(arg(0)),
        "prepend" => Filter::Prepend(arg(0)),
        "dateparse" => Filter::DateParse(arg(0)),
        "timeparse" => Filter::TimeParse(arg(0)),
        "regexp" => Filter::Regexp(compile(&arg(0))?),
        "querystring" => Filter::QueryString(arg(0)),
        "andmatch" => Filter::AndMatch,
        "split" => Filter::Split {
            sep: arg(0),
            index: arg(1).parse::<i64>().unwrap_or(0),
        },
        "trim" => Filter::Trim(args.first().cloned()),
        "timeago" => Filter::TimeAgo,
        "fuzzytime" => Filter::FuzzyTime,
        "validate" => Filter::Validate(arg(0)),
        "htmldecode" => Filter::HtmlDecode,
        "validfilename" => Filter::ValidFilename,
        "urldecode" => Filter::UrlDecode,
        "urlencode" => Filter::UrlEncode,
        "diacritics" => Filter::Diacritics,
        "toupper" => Filter::ToUpper,
        "tolower" => Filter::ToLower,
        other => {
            return Err(CardigannError::UnknownFilter {
                name: other.to_string(),
            });
        }
    })
}

/// Applies a filter to a value.
///
/// # Errors
///
/// This implementation never fails at apply time — all argument validation
/// happens in [`parse_filter`] — but the signature returns a `Result`
/// because some filters (notably date/time parsing, once the engine grows
/// a clock) may need to fail here in the future.
pub fn apply(filter: &Filter, input: &str) -> Result<String, CardigannError> {
    Ok(match filter {
        Filter::ReReplace {
            pattern,
            replacement,
        } => pattern.replace_all(input, replacement.as_str()),
        Filter::Replace { from, to } => input.replace(from.as_str(), to),
        Filter::Append(s) => format!("{input}{s}"),
        Filter::Prepend(s) => format!("{s}{input}"),
        Filter::Regexp(re) => re.first_group(input).unwrap_or_default(),
        Filter::QueryString(key) => query_param(input, key),
        Filter::Split { sep, index } => {
            let parts: Vec<&str> = input.split(sep.as_str()).collect();
            resolve_split_index(*index, parts.len())
                .map(|i| parts[i])
                .map_or_else(String::new, ToString::to_string)
        }
        Filter::Trim(Some(chars)) => {
            let set: Vec<char> = chars.chars().collect();
            input.trim_matches(|c| set.contains(&c)).to_string()
        }
        Filter::Trim(None) => input.trim().to_string(),
        Filter::ToUpper => input.to_uppercase(),
        Filter::ToLower => input.to_lowercase(),
        Filter::UrlEncode => urlencoding_encode(input),
        Filter::UrlDecode => urlencoding_decode(input),
        Filter::HtmlDecode => html_decode(input),
        Filter::ValidFilename => input
            .chars()
            .filter(|c| !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
            .collect(),
        Filter::Diacritics => fold_diacritics(input),
        Filter::Validate(whitelist) => validate_intersect(input, whitelist),
        // Date/time evaluation is delegated to the engine, which owns the
        // clock: these are recognized (so a definition using them is not
        // rejected as unknown) but not yet evaluated, and pass the input
        // through unchanged until a later plan gives the engine a clock.
        Filter::DateParse(_) | Filter::TimeParse(_) | Filter::TimeAgo | Filter::FuzzyTime => {
            input.to_string()
        }
        // `andmatch` cannot be implemented as a value transform at all
        // (see the `AndMatch` doc comment): today every one of the 59+
        // trackers in the corpus that use it accepts all rows regardless
        // of query relevance, matching a currently-open upstream Prowlarr
        // defect (Prowlarr/Prowlarr#1270) where its own `andmatch` case
        // also parses arguments and then does nothing with them.
        Filter::AndMatch => input.to_string(),
    })
}

fn query_param(input: &str, key: &str) -> String {
    let query = input.split_once('?').map_or(input, |(_, q)| q);
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.to_string())
        .unwrap_or_default()
}

/// Resolves a Cardigann `split` filter's index against the number of
/// parts a `split` produced, mirroring Go/Cardigann's negative-index
/// convention: `-1` is the last part, `-2` the second-to-last, and so on.
/// Returns `None` when the resolved position is out of range in either
/// direction (this is what makes a too-negative or too-large index yield
/// an empty string rather than panicking).
fn resolve_split_index(index: i64, len: usize) -> Option<usize> {
    let len_i64 = i64::try_from(len).unwrap_or(i64::MAX);
    let resolved = if index < 0 { index + len_i64 } else { index };
    usize::try_from(resolved).ok().filter(|i| *i < len)
}

/// The delimiter set Cardigann's `validate` filter splits both its
/// whitelist argument and the input on, matching the real engine's
/// tokenisation (comma, whitespace, slash, brackets, and a few other
/// punctuation marks commonly separating category names).
const VALIDATE_DELIMITERS: [char; 12] =
    [',', ' ', '/', ')', '(', '.', ';', '[', ']', '"', '|', ':'];

/// Splits `s` into lowercase, trimmed, non-empty tokens on
/// [`VALIDATE_DELIMITERS`].
fn validate_tokens(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c| VALIDATE_DELIMITERS.contains(&c))
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// Implements Cardigann's `validate` filter: keeps only the tokens the
/// input has in common with the whitelist, case-folded, deduplicated, and
/// in the whitelist's order — matching Prowlarr's own
/// `validList.Intersect(dataTokens)` followed by a `", "`-joined output.
/// A token present in the input but absent from the whitelist is dropped;
/// a whitelist entry absent from the input is dropped too.
fn validate_intersect(input: &str, whitelist: &str) -> String {
    let allowed = validate_tokens(whitelist);
    let data_tokens = validate_tokens(input);
    let mut seen = std::collections::HashSet::new();
    let mut kept = Vec::new();
    for token in allowed {
        if data_tokens.contains(&token) && seen.insert(token.clone()) {
            kept.push(token);
        }
    }
    kept.join(", ")
}

/// Percent-encodes `input` to match .NET's `WebUtility.UrlEncode` — the
/// primitive Prowlarr's own Cardigann `urlencode` filter calls into
/// (`CardigannBase.cs`'s `data.UrlEncode(_encoding)`, whose
/// `StringExtensions.UrlEncode` calls `WebUtility.UrlEncodeToBytes`).
///
/// This deliberately does **not** follow RFC 3986 (the convention
/// `Uri.EscapeDataString` uses, and what an "obvious" fix might reach
/// for). `dotnet/runtime`'s `WebUtility.cs` defines its safe set as "Url
/// safe chars as defined by RFC 1738.4, minus `+`" — literally
/// `A-Za-z0-9-_.!*()` — and separately encodes a space as `+` (RFC
/// 1738.4, not RFC 3986, is also where `~` and `!*()` being treated
/// differently from RFC 3986's unreserved set comes from: RFC 3986 would
/// leave `~` unescaped and escape `!*()`; .NET does the exact opposite).
/// Do not "fix" this back to RFC 3986's unreserved set — that would flip
/// the space encoding, `~`, and `!*()` all at once, and break the search
/// URLs real trackers expect, since this is the encoding those requests
/// are built against.
fn urlencoding_encode(input: &str) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'*'
            | b'('
            | b')' => {
                out.push(char::from(b));
            }
            b' ' => out.push('+'),
            _ => {
                // `out` has just enough capacity reserved above for the
                // common case; the write itself cannot fail.
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

/// Decodes percent-encoding and `+`-as-space, mirroring
/// [`urlencoding_encode`]'s .NET-matching behavior. Accepts either case
/// in a `%XX` hex pair (`WebUtility.UrlDecode` does too); a malformed or
/// truncated `%` sequence (not followed by two hex digits) is left
/// literal rather than dropped or erroring.
fn urlencoding_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Decodes HTML entities in a single left-to-right pass.
///
/// A chained sequence of `str::replace` calls (one per entity) would feed
/// each replacement's output into the *next* replacement's input: decoding
/// `&amp;lt;` would first turn it into `&lt;`, and a subsequent `&lt;` ->
/// `<` pass would then wrongly decode that as well, yielding `<` instead
/// of the correct `&lt;`. The corpus relies on the correct, non-cascading
/// behavior: `hdspace.yml` calls `htmldecode` twice deliberately, to
/// unwrap exactly one level of double-escaping per call — which only
/// works if a single call resolves each `&...;` occurrence exactly once.
fn html_decode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp + 1..];
        let Some(semi) = tail.find(';') else {
            out.push('&');
            rest = tail;
            continue;
        };
        let name = &tail[..semi];
        if let Some(decoded) = decode_entity(name) {
            out.push(decoded);
            rest = &tail[semi + 1..];
        } else {
            // Not a recognized entity: keep the `&` literally and resume
            // scanning right after it, so its own `;` (and anything past
            // it) is still considered for the next entity.
            out.push('&');
            rest = tail;
        }
    }
    out.push_str(rest);
    out
}

/// Decodes a single HTML entity name (the text between `&` and `;`,
/// exclusive) to its character, or `None` if it is not recognized.
fn decode_entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" | "#39" => Some('\''),
        "nbsp" => Some(' '),
        _ => {
            if let Some(hex) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
                u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
            } else if let Some(dec) = name.strip_prefix('#') {
                dec.parse::<u32>().ok().and_then(char::from_u32)
            } else {
                None
            }
        }
    }
}

/// Folds accented Latin characters to their unaccented base letter.
///
/// This is a table, not a genuine Unicode NFD decomposition (no crate in
/// the workspace provides Unicode normalization tables, and adding one
/// for this alone was judged not worth a new dependency). The table is
/// keyed by lowercase letter only; case is restored by folding the
/// lowercase form of any input character and re-uppercasing the result
/// when the input was uppercase, which lets every accented pair (`Á`/`á`,
/// `Č`/`č`, …) share one entry instead of two.
///
/// Coverage: Latin-1 Supplement and Latin Extended-A (Western and Central
/// European accents), Romanian's comma-below `ș`/`ț`, Turkish's
/// dotted/dotless `İ`/`ı` (handled as a special case below, since their
/// simple-case-fold does not round-trip through ASCII the way the rest of
/// the table's does), and Vietnamese's Latin Extended Additional tone
/// marks. Together this covers every Latin-script `language:` value the
/// corpus declares that plausibly needs diacritic folding: `it-IT`,
/// `cs-CZ`, `pl-PL`, `sl-SI`, `hr-HR`, `sr-SP`, `lt-LT`, `lv-LV`, `tr-TR`,
/// `ro-RO`, and `vi-VN`, plus `hu-HU` (`ő`/`ű`) even though no reviewed
/// corpus entry named it explicitly.
fn fold_diacritics(input: &str) -> String {
    input.chars().map(fold_diacritic_char).collect()
}

fn fold_diacritic_char(c: char) -> char {
    // Turkish dotless/dotted i: `'İ'.to_lowercase()` yields `i̇` (ASCII `i`
    // plus a combining dot above), not a clean `i`, so the generic
    // lowercase-and-restore path below would leave a stray combining mark
    // instead of folding it. Handling both members of the pair directly
    // sidesteps that.
    match c {
        'ı' => return 'i',
        'İ' => return 'I',
        _ => {}
    }

    let is_upper = c.is_uppercase();
    let lower = c.to_lowercase().next().unwrap_or(c);
    let folded = base_latin_letter(lower);
    if is_upper {
        folded.to_ascii_uppercase()
    } else {
        folded
    }
}

fn base_latin_letter(c: char) -> char {
    match c {
        // The second half of each pattern list (after the Latin-1 /
        // Extended-A forms) is Vietnamese's Latin Extended Additional
        // block: a tone mark stacked on a circumflex/breve/horn base,
        // which still collapses to the same plain vowel.
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' | 'ạ' | 'ả' | 'ấ' | 'ầ' | 'ẩ' | 'ẫ'
        | 'ậ' | 'ắ' | 'ằ' | 'ẳ' | 'ẵ' | 'ặ' => 'a',
        'ç' | 'ć' | 'č' | 'ĉ' | 'ċ' => 'c',
        'ď' | 'đ' => 'd',
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' | 'ẹ' | 'ẻ' | 'ẽ' | 'ế' | 'ề' | 'ể'
        | 'ễ' | 'ệ' => 'e',
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => 'g',
        'ĥ' | 'ħ' => 'h',
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'į' | 'ỉ' | 'ị' => 'i',
        'ĵ' => 'j',
        'ķ' => 'k',
        'ĺ' | 'ļ' | 'ľ' | 'ł' => 'l',
        'ñ' | 'ń' | 'ņ' | 'ň' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' | 'ơ' | 'ọ' | 'ỏ' | 'ố' | 'ồ' | 'ổ' | 'ỗ'
        | 'ộ' | 'ớ' | 'ờ' | 'ở' | 'ỡ' | 'ợ' => 'o',
        'ŕ' | 'ř' => 'r',
        'ś' | 'ş' | 'š' | 'ș' | 'ŝ' => 's',
        'ţ' | 'ť' | 'ț' => 't',
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ů' | 'ű' | 'ų' | 'ư' | 'ụ' | 'ủ' | 'ứ' | 'ừ' | 'ử'
        | 'ữ' | 'ự' => 'u',
        'ý' | 'ÿ' | 'ỳ' | 'ỵ' | 'ỷ' | 'ỹ' => 'y',
        'ź' | 'ż' | 'ž' => 'z',
        other => other,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn run(name: &str, args: &[&str], input: &str) -> String {
        let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
        let f = parse_filter(name, &args).unwrap();
        apply(&f, input).unwrap()
    }

    #[test]
    fn re_replace_substitutes_with_capture_groups() {
        assert_eq!(
            run("re_replace", &[r"(\d+)MB", "$1 megabytes"], "700MB"),
            "700 megabytes"
        );
    }

    #[test]
    fn replace_is_literal() {
        assert_eq!(
            run("replace", &["-", " "], "big-buck-bunny"),
            "big buck bunny"
        );
    }

    #[test]
    fn append_and_prepend() {
        assert_eq!(run("append", &[" GB"], "4"), "4 GB");
        assert_eq!(run("prepend", &["Size: "], "4"), "Size: 4");
    }

    #[test]
    fn trim_removes_whitespace_by_default() {
        assert_eq!(run("trim", &[], "  hello  "), "hello");
    }

    #[test]
    fn trim_removes_given_characters() {
        assert_eq!(run("trim", &["|"], "|hello|"), "hello");
    }

    #[test]
    fn case_helpers() {
        assert_eq!(run("toupper", &[], "abc"), "ABC");
        assert_eq!(run("tolower", &[], "ABC"), "abc");
    }

    #[test]
    fn split_takes_indexed_part() {
        assert_eq!(run("split", &["|", "1"], "a|b|c"), "b");
    }

    #[test]
    fn regexp_extracts_first_group() {
        assert_eq!(run("regexp", &[r"(\d+) seeders"], "42 seeders"), "42");
    }

    #[test]
    fn querystring_reads_a_parameter() {
        assert_eq!(run("querystring", &["id"], "/details.php?id=99&x=1"), "99");
    }

    #[test]
    fn urlencode_and_urldecode_roundtrip() {
        // Matches .NET's `WebUtility.UrlEncode`, which is what Prowlarr's
        // own `urlencode` filter calls into: a space becomes `+`, not
        // `%20` (RFC 3986's `Uri.EscapeDataString` convention).
        assert_eq!(run("urlencode", &[], "a b"), "a+b");
        assert_eq!(run("urldecode", &[], "a+b"), "a b");
    }

    #[test]
    fn urlencode_percent_encodes_tilde() {
        // RFC 3986 would leave `~` unescaped; .NET's safe set does not
        // include it, so it must come out as %7E.
        assert_eq!(run("urlencode", &[], "~"), "%7E");
    }

    #[test]
    fn urlencode_leaves_bang_star_and_parens_unescaped() {
        // RFC 3986 would escape these; .NET's RFC-1738.4-minus-plus safe
        // set does not.
        assert_eq!(run("urlencode", &[], "!*()"), "!*()");
    }

    #[test]
    fn urlencode_encodes_multi_byte_characters_as_utf8_percent_pairs() {
        // 'é' is U+00E9, UTF-8 bytes 0xC3 0xA9.
        assert_eq!(run("urlencode", &[], "é"), "%C3%A9");
    }

    #[test]
    fn urlencode_escapes_a_literal_plus_so_it_cannot_be_mistaken_for_a_space() {
        // `+` is not in the safe set, so an actual `+` in the input must
        // itself be percent-encoded — otherwise it would be
        // indistinguishable from an encoded space on the way back.
        assert_eq!(run("urlencode", &[], "+"), "%2B");
    }

    #[test]
    fn urldecode_treats_a_literal_plus_as_a_space() {
        assert_eq!(run("urldecode", &[], "a+b"), "a b");
    }

    #[test]
    fn htmldecode_resolves_entities() {
        assert_eq!(run("htmldecode", &[], "a &amp; b"), "a & b");
    }

    #[test]
    fn validfilename_strips_path_separators() {
        assert_eq!(run("validfilename", &[], "a/b:c"), "abc");
    }

    #[test]
    fn diacritics_are_folded() {
        assert_eq!(run("diacritics", &[], "Amélie"), "Amelie");
    }

    #[test]
    fn diacritics_fold_the_corpus_declared_languages() {
        // cs-CZ (sktorrent.yml): háčeks and the ring above.
        assert_eq!(
            run("diacritics", &[], "Příliš žluťoučký kůň"),
            "Prilis zlutoucky kun"
        );
        // pl-PL: ogonek, stroke, and dot-above.
        assert_eq!(
            run("diacritics", &[], "Zażółć gęślą jaźń"),
            "Zazolc gesla jazn"
        );
        // ro-RO: comma-below s/t, distinct from the cedilla forms.
        assert_eq!(run("diacritics", &[], "Șase ține pași"), "Sase tine pasi");
        // tr-TR: soft g, dotted/dotless i pair (note the case is retained:
        // 'İ' folds to 'I', 'ı' folds to 'i').
        assert_eq!(
            run("diacritics", &[], "İstanbul çöğüş ışık"),
            "Istanbul cogus isik"
        );
        // vi-VN: circumflex/horn base plus a stacked tone mark.
        assert_eq!(
            run("diacritics", &[], "Việt Nam đường phố"),
            "Viet Nam duong pho"
        );
    }

    #[test]
    fn htmldecode_does_not_cascade_through_a_second_decode() {
        // A chained sequence of literal replacements would decode this
        // twice (first `&amp;` -> `&`, producing `&lt;`, then wrongly
        // decoding that `&lt;` too) and yield `<`. A single left-to-right
        // pass resolves only the outer `&amp;`, yielding the literal
        // string `&lt;` — the corpus (hdspace.yml) depends on this,
        // calling `htmldecode` twice deliberately to peel one level of
        // escaping per call.
        assert_eq!(run("htmldecode", &[], "&amp;lt;"), "&lt;");
    }

    #[test]
    fn htmldecode_resolves_numeric_entities() {
        assert_eq!(run("htmldecode", &[], "&#65;"), "A");
        assert_eq!(run("htmldecode", &[], "&#x41;"), "A");
        assert_eq!(run("htmldecode", &[], "&#X41;"), "A");
    }

    #[test]
    fn htmldecode_leaves_unrecognized_ampersands_alone() {
        assert_eq!(run("htmldecode", &[], "A & B; C"), "A & B; C");
        assert_eq!(run("htmldecode", &[], "Tom & Jerry"), "Tom & Jerry");
    }

    #[test]
    fn split_negative_index_counts_from_the_end() {
        assert_eq!(run("split", &["|", "-1"], "a|b|c"), "c");
        assert_eq!(run("split", &["|", "-2"], "a|b|c"), "b");
    }

    #[test]
    fn split_out_of_range_index_is_empty_not_a_panic() {
        // Too negative: even after adding the length back, still negative.
        assert_eq!(run("split", &["|", "-5"], "a|b|c"), "");
        // Too positive: at or beyond the number of parts.
        assert_eq!(run("split", &["|", "10"], "a|b|c"), "");
        assert_eq!(run("split", &["|", "3"], "a|b|c"), "");
    }

    #[test]
    fn validate_keeps_a_whitelisted_token() {
        assert_eq!(
            run("validate", &["Action, Adventure, Comedy"], "Action"),
            "action"
        );
    }

    #[test]
    fn validate_drops_a_token_not_on_the_whitelist() {
        assert_eq!(run("validate", &["Action, Adventure"], "Horror"), "");
    }

    #[test]
    fn validate_trims_whitespace_around_entries() {
        assert_eq!(
            run("validate", &["Action, Adventure"], "  Action  "),
            "action"
        );
    }

    #[test]
    fn validate_keeps_multiple_matches_in_whitelist_order() {
        // The data's own order ("Comedy Action") is not preserved; the
        // output follows the whitelist's order instead, matching
        // Prowlarr's `whitelist.Intersect(dataTokens)` (LINQ `Intersect`
        // yields elements in the first sequence's order).
        assert_eq!(
            run("validate", &["Action, Adventure, Comedy"], "Comedy Action"),
            "action, comedy"
        );
    }

    #[test]
    fn dateparse_timeparse_timeago_fuzzytime_and_andmatch_pass_through_unchanged() {
        // None of these five are evaluated yet: `dateparse`/`timeparse`/
        // `timeago`/`fuzzytime` need a clock the engine does not own
        // until a later plan, and `andmatch` cannot be expressed by
        // `apply`'s `&str -> String` signature at all (it is a
        // row-inclusion filter, not a value transform). Pinning today's
        // no-op behavior here means the day any of them gets a real
        // implementation, this test fails and has to be updated as a
        // deliberate, visible diff — not silently.
        assert_eq!(
            run("dateparse", &["Mon, 02 Jan 2006"], "irrelevant"),
            "irrelevant"
        );
        assert_eq!(run("timeparse", &["15:04:05"], "irrelevant"), "irrelevant");
        assert_eq!(run("timeago", &[], "3 hours ago"), "3 hours ago");
        assert_eq!(run("fuzzytime", &[], "yesterday"), "yesterday");
        assert_eq!(run("andmatch", &[], "some row text"), "some row text");
    }

    #[test]
    fn unknown_filter_is_an_error() {
        assert!(parse_filter("nonexistent", &[]).is_err());
    }
}
