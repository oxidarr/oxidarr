//! The 21 filter functions used by the Cardigann v11 corpus, ordered in the
//! enum by frequency of use.

use crate::error::CardigannError;
use crate::fuzzydate::parse_fuzzy;
use crate::netlayout::net_layout_to_chrono;
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use regex::Regex;

/// Context threaded through filter evaluation: the values a real Cardigann
/// engine reads from its environment rather than from the row being
/// processed — the wall clock (for `dateparse`/`timeparse`/`timeago`/
/// `fuzzytime`) and the search's keywords (for `andmatch`'s row-inclusion
/// check; see [`apply`]'s match arms).
#[derive(Debug, Clone)]
pub struct FilterCtx {
    /// The moment a filter evaluation should treat as "now".
    pub now: DateTime<Utc>,
    /// The search's keywords, in whatever order the search was issued with.
    pub keywords: Vec<String>,
}

impl FilterCtx {
    /// A fixed context for tests: `2026-01-15T12:00:00Z`, no keywords.
    ///
    /// Fixed rather than `Utc::now()` so tests of date/time filters (once
    /// those filters are implemented) are deterministic instead of flaking
    /// around midnight or across CI runs on different days.
    #[must_use]
    pub fn fixed_for_tests() -> Self {
        let now = Utc
            .with_ymd_and_hms(2026, 1, 15, 12, 0, 0)
            .single()
            .unwrap_or(DateTime::<Utc>::MIN_UTC);
        Self {
            now,
            keywords: Vec::new(),
        }
    }
}

/// The result of applying a single filter to a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterOutcome {
    /// The filter produced a (possibly transformed) value.
    Value(String),
    /// The filter says to drop the row this value belongs to entirely.
    /// `andmatch` is the only filter that emits this, for rows that fail to
    /// match every search keyword; the engine's filter-chain loop treats it
    /// as "skip this row".
    DropRow,
}

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
    ///
    /// # Errors
    /// Returns an error when the backtracking engine exceeds its limit. Only
    /// [`Pattern::Fancy`] can fail: `fancy_regex`'s own `replace_all` unwraps
    /// that error internally, so `try_replacen` is used instead — a
    /// tracker-controlled page must not be able to panic the process.
    pub fn replace_all(&self, input: &str, replacement: &str) -> Result<String, String> {
        match self {
            Self::Fast(re) => Ok(re.replace_all(input, replacement).into_owned()),
            Self::Fancy(re) => re
                .try_replacen(input, 0, replacement)
                .map(std::borrow::Cow::into_owned)
                .map_err(|e| e.to_string()),
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
    /// Parses the input as a date using the given .NET custom date-format
    /// layout. Deferred to the engine, which owns the clock.
    DateParse(String),
    /// Parses the input as a time-of-day using the given .NET custom
    /// date-format layout. Deferred to the engine, which owns the clock.
    TimeParse(String),
    /// Extracts the first capture group (or the whole match) of a regex.
    Regexp(Pattern),
    /// Reads a single query-string parameter from a URL or query string.
    QueryString(String),
    /// A ROW-inclusion filter: keeps only rows whose text contains every
    /// one of the search's keywords ([`FilterCtx::keywords`]), case
    /// insensitively; a row missing any keyword is dropped via
    /// [`FilterOutcome::DropRow`], not just filtered to an empty value.
    ///
    /// The optional argument is Cardigann's `andmatch` character limit
    /// (`args: 50` in `torrentlt.yml`, `deildu.yml`, `backups.yml`): per
    /// Jackett's `TorznabQuery.MatchQueryStringAND` (the real engine behind
    /// `CardigannIndexer.cs`'s `andmatch` case — Prowlarr copied the case
    /// label but never wired the limit through, an open defect tracked as
    /// Prowlarr/Prowlarr#1270), the limit truncates the search's keywords
    /// (joined with a space) to their first N characters BEFORE splitting
    /// back into words — it bounds *which* keywords must match, not *where*
    /// in the row's text a keyword must appear. `None` when the argument is
    /// absent or fails to parse as a non-negative integer.
    AndMatch(Option<usize>),
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
    /// Parses a relative "time ago" phrase (`3 hours ago`) against the
    /// clock in [`FilterCtx`]. Shares its parser with `FuzzyTime`.
    TimeAgo,
    /// Parses a fuzzy/approximate time phrase (`yesterday`, `Today at
    /// 10:32`, …) against the clock in [`FilterCtx`]. Shares its parser
    /// with `TimeAgo`.
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
        "andmatch" => Filter::AndMatch(args.first().and_then(|a| a.parse::<usize>().ok())),
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
/// `ctx` carries the clock and search keywords a real Cardigann engine
/// consults outside the row itself; see [`FilterCtx`]. `dateparse`/
/// `timeparse`/`timeago`/`fuzzytime` all read `ctx.now`; `andmatch` reads
/// `ctx.keywords` and, unlike every other filter here, may answer with
/// [`FilterOutcome::DropRow`] instead of a transformed value — see
/// [`and_match`].
///
/// # Errors
///
/// This implementation never fails at apply time — all argument validation
/// happens in [`parse_filter`], and unparseable `dateparse`/`timeparse`
/// input passes through unchanged rather than erroring (Prowlarr tolerates
/// junk rows; dropping them is not this filter's job) — but the signature
/// returns a `Result` because a future filter may need to fail here.
pub fn apply(
    filter: &Filter,
    input: &str,
    ctx: &FilterCtx,
) -> Result<FilterOutcome, CardigannError> {
    Ok(FilterOutcome::Value(match filter {
        // Answers with `FilterOutcome::DropRow` rather than a value, so it
        // returns out of this match (and this function) directly instead
        // of falling through to the `Value(...)` wrapper below.
        Filter::AndMatch(limit) => return Ok(and_match(input, ctx, *limit)),
        Filter::ReReplace {
            pattern,
            replacement,
        } => pattern
            .replace_all(input, replacement.as_str())
            .map_err(|reason| CardigannError::Filter {
                name: "re_replace".to_string(),
                reason,
            })?,
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
        // `dateparse` and `timeparse` share one .NET-layout parser: the
        // input passes through unchanged when the layout does not
        // translate or nothing matches it (Prowlarr tolerates junk rows;
        // dropping them is not this filter's job).
        Filter::DateParse(layout) | Filter::TimeParse(layout) => {
            parse_net_datetime(layout, input, ctx)
                .map_or_else(|| input.to_string(), |dt| dt.to_rfc3339())
        }
        // `timeago`/`fuzzytime` share one fuzzy relative-date parser (see
        // `fuzzydate::parse_fuzzy`): the input passes through unchanged
        // when it matches none of the recognized shapes (Prowlarr
        // tolerates junk rows; dropping them is not this filter's job).
        Filter::TimeAgo | Filter::FuzzyTime => {
            parse_fuzzy(input, ctx.now).map_or_else(|| input.to_string(), |dt| dt.to_rfc3339())
        }
    }))
}

/// Implements Cardigann's `andmatch`: keeps the row only if every one of
/// `ctx.keywords` appears case-insensitively somewhere in `input`; an empty
/// keyword list (no active search, e.g. an RSS feed poll) always keeps the
/// row, matching Prowlarr/Jackett's own `!searchCriteria.IsRssSearch` guard
/// on the equivalent general-purpose filter.
///
/// `limit`, when present, truncates the keywords (joined with a single
/// space, in `ctx.keywords`'s order) to their first `limit` characters
/// before re-splitting them on non-word runs — see the `AndMatch` doc
/// comment for why this mirrors Jackett's `MatchQueryStringAND` rather than
/// bounding where in `input` a keyword must appear. A `limit` of `0`, or one
/// that truncates away every keyword, leaves nothing left to require, so
/// the row is kept — matching .NET LINQ's `Enumerable.All` returning `true`
/// on an empty sequence.
fn and_match(input: &str, ctx: &FilterCtx, limit: Option<usize>) -> FilterOutcome {
    if ctx.keywords.is_empty() {
        return FilterOutcome::Value(input.to_string());
    }

    let joined = ctx.keywords.join(" ");
    let scope = match limit {
        Some(n) => joined.chars().take(n).collect::<String>(),
        None => joined,
    };

    let input_lower = input.to_lowercase();
    let all_present = scope
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|part| !part.is_empty())
        .all(|part| input_lower.contains(&part.to_lowercase()));

    if all_present {
        FilterOutcome::Value(input.to_string())
    } else {
        FilterOutcome::DropRow
    }
}

/// Parses `input` against a .NET custom date-format `layout`
/// (`dateparse`/`timeparse`'s single argument), returning the parsed
/// instant normalised to UTC, or `None` if the layout does not translate
/// or `input` does not match it in any of the shapes tried below.
///
/// The layout is translated once via [`net_layout_to_chrono`] and its
/// fractional-second sequence normalised (see [`normalize_fractional_seconds`])
/// before any parse is attempted. When the translated format carries an
/// offset spec (`%:z`/`%z`), [`DateTime::parse_from_str`] is tried first;
/// a corpus layout advertising an offset (`zzz`) but a scraped value that
/// omits it is common enough that a failure there is retried with the
/// offset token stripped from the format, falling back to
/// [`parse_naive`] (which assumes UTC). A format without an offset spec
/// goes straight to [`parse_naive`].
fn parse_net_datetime(layout: &str, input: &str, ctx: &FilterCtx) -> Option<DateTime<Utc>> {
    let format = net_layout_to_chrono(layout).ok()?;
    let format = normalize_fractional_seconds(&format);

    if format_has_offset(&format) {
        if let Ok(dt) = DateTime::parse_from_str(input, &format) {
            return Some(dt.with_timezone(&Utc));
        }
        let stripped = strip_trailing_offset(&format);
        return parse_naive(&stripped, input, ctx);
    }
    parse_naive(&format, input, ctx)
}

/// Whether a translated chrono format carries an offset directive
/// (`%:z`, from .NET's `zzz`/`K`, or `%z`, from `zz`).
fn format_has_offset(format: &str) -> bool {
    format.contains("%:z") || format.contains("%z")
}

/// Strips a trailing offset directive (with or without a preceding
/// space) from a translated chrono format, for the offset-missing retry
/// in [`parse_net_datetime`]. Corpus layouts place the offset token at
/// the end (`"... zzz"`), so only a trailing match is stripped; a format
/// with no matching suffix is returned unchanged.
fn strip_trailing_offset(format: &str) -> String {
    for suffix in [" %:z", "%:z", " %z", "%z"] {
        if let Some(stripped) = format.strip_suffix(suffix) {
            return stripped.to_string();
        }
    }
    format.to_string()
}

/// Parses `input` against an offset-free chrono `format`, assuming UTC.
/// Tries, in order: a full datetime, a date only (midnight UTC), and a
/// time only (`timeparse`'s whole reason for existing is a layout with no
/// date component — `ctx.now`'s date fills that gap). Returns `None` if
/// none of the three match.
fn parse_naive(format: &str, input: &str, ctx: &FilterCtx) -> Option<DateTime<Utc>> {
    if let Ok(ndt) = NaiveDateTime::parse_from_str(input, format) {
        return Some(Utc.from_utc_datetime(&ndt));
    }
    if let Ok(date) = NaiveDate::parse_from_str(input, format) {
        let ndt = date.and_hms_opt(0, 0, 0)?;
        return Some(Utc.from_utc_datetime(&ndt));
    }
    if let Ok(time) = NaiveTime::parse_from_str(input, format) {
        let ndt = ctx.now.date_naive().and_time(time);
        return Some(Utc.from_utc_datetime(&ndt));
    }
    None
}

/// Collapses `.%.3f` (a literal dot immediately followed by chrono's
/// `%.3f` fractional-second directive, which itself renders/expects a
/// leading dot of its own) down to `%.3f`.
///
/// [`net_layout_to_chrono`] translates .NET's `fff` token to `%.3f`
/// verbatim, so a corpus layout written `ss.fff` (a literal dot, then
/// `fff`) becomes `%S.%.3f` — a format expecting two dots in the input
/// where a real value (`00.123`) has only one, so it would never parse
/// without this normalisation.
fn normalize_fractional_seconds(format: &str) -> String {
    format.replace(".%.3f", "%.3f")
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
        run_ctx(name, args, input, &FilterCtx::fixed_for_tests())
    }

    fn run_ctx(name: &str, args: &[&str], input: &str, ctx: &FilterCtx) -> String {
        let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
        let f = parse_filter(name, &args).unwrap();
        match apply(&f, input, ctx).unwrap() {
            FilterOutcome::Value(v) => v,
            FilterOutcome::DropRow => {
                unreachable!("no filter in this test suite emits DropRow")
            }
        }
    }

    fn run_outcome(name: &str, args: &[&str], input: &str, ctx: &FilterCtx) -> FilterOutcome {
        let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
        let f = parse_filter(name, &args).unwrap();
        apply(&f, input, ctx).unwrap()
    }

    #[test]
    fn apply_takes_context_and_wraps_plain_values() {
        let ctx = FilterCtx::fixed_for_tests();
        let out = apply(&parse_filter("toupper", &[]).unwrap(), "abc", &ctx).unwrap();
        assert_eq!(out, FilterOutcome::Value("ABC".into()));
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
    fn andmatch_drops_rows_missing_a_keyword() {
        let mut ctx = FilterCtx::fixed_for_tests();
        ctx.keywords = vec!["ubuntu".into(), "server".into()];
        assert_eq!(
            run_outcome("andmatch", &[], "Ubuntu 24.04 Desktop", &ctx),
            FilterOutcome::DropRow
        );
        assert_eq!(
            run_outcome("andmatch", &[], "Ubuntu 24.04 Server", &ctx),
            FilterOutcome::Value("Ubuntu 24.04 Server".into())
        );
    }

    #[test]
    fn andmatch_with_no_keywords_keeps_everything() {
        let ctx = FilterCtx::fixed_for_tests();
        assert_eq!(
            run_outcome("andmatch", &[], "anything", &ctx),
            FilterOutcome::Value("anything".into())
        );
    }

    #[test]
    fn andmatch_matches_case_insensitively() {
        let mut ctx = FilterCtx::fixed_for_tests();
        ctx.keywords = vec!["UBUNTU".into()];
        assert_eq!(
            run_outcome("andmatch", &[], "ubuntu 24.04 server", &ctx),
            FilterOutcome::Value("ubuntu 24.04 server".into())
        );
    }

    #[test]
    fn andmatch_character_limit_arg_bounds_which_keywords_must_match() {
        // Mirrors Jackett's `TorznabQuery.MatchQueryStringAND(title, limit)`
        // (the real engine behind Cardigann's `andmatch`, wired up through
        // `query.ImdbID`-style guards in `CardigannIndexer.cs`): `limit`
        // truncates the space-joined keyword string to its first N
        // characters BEFORE splitting it back into words, so it bounds
        // which keywords are required, not where in the input a keyword
        // must appear. Here only "ubu" (the first 3 characters of "ubuntu
        // server") is required, so a row missing "server" still matches.
        let mut ctx = FilterCtx::fixed_for_tests();
        ctx.keywords = vec!["ubuntu".into(), "server".into()];
        assert_eq!(
            run_outcome("andmatch", &["3"], "Ubuntu 24.04 Desktop", &ctx),
            FilterOutcome::Value("Ubuntu 24.04 Desktop".into())
        );
    }

    #[test]
    fn andmatch_unparseable_arg_falls_back_to_no_limit() {
        let mut ctx = FilterCtx::fixed_for_tests();
        ctx.keywords = vec!["ubuntu".into(), "server".into()];
        assert_eq!(
            run_outcome("andmatch", &["not-a-number"], "Ubuntu 24.04 Desktop", &ctx),
            FilterOutcome::DropRow
        );
    }

    #[test]
    fn timeago_subtracts_from_now() {
        let ctx = FilterCtx::fixed_for_tests();
        assert_eq!(
            run_ctx("timeago", &[], "3 hours ago", &ctx),
            "2026-01-15T09:00:00+00:00"
        );
        assert_eq!(
            run_ctx("timeago", &[], "1 day 2 hours ago", &ctx),
            "2026-01-14T10:00:00+00:00"
        );
    }

    #[test]
    fn fuzzytime_handles_named_days() {
        let ctx = FilterCtx::fixed_for_tests();
        assert_eq!(
            run_ctx("fuzzytime", &[], "yesterday 08:15", &ctx),
            "2026-01-14T08:15:00+00:00"
        );
        assert_eq!(
            run_ctx("fuzzytime", &[], "Today at 10:32", &ctx),
            "2026-01-15T10:32:00+00:00"
        );
    }

    #[test]
    fn timeago_with_an_overflowing_count_passes_through_without_panicking() {
        // Adversarial input: a count large enough to overflow chrono's
        // TimeDelta bounds must be treated as junk (pass through
        // unchanged), not panic the process.
        let ctx = FilterCtx::fixed_for_tests();
        assert_eq!(
            run_ctx("timeago", &[], "999999999999 days ago", &ctx),
            "999999999999 days ago"
        );
        assert_eq!(
            run_ctx("timeago", &[], "9223372036854775807 months ago", &ctx),
            "9223372036854775807 months ago"
        );
    }

    #[test]
    fn fuzzytime_resolves_a_trailing_offset_like_miobt_does() {
        // Corpus shape (miobt.yml): after translating 今天/昨天 to
        // Today/Yesterday, `append " +08:00"` runs immediately before
        // `fuzzytime`, so the real input carries the tracker's fixed
        // local offset as a trailing suffix.
        let ctx = FilterCtx::fixed_for_tests();
        assert_eq!(
            run_ctx("fuzzytime", &[], "Today 00:35 +08:00", &ctx),
            "2026-01-14T16:35:00+00:00"
        );
    }

    #[test]
    fn fuzzytime_passes_junk_through() {
        let ctx = FilterCtx::fixed_for_tests();
        assert_eq!(run_ctx("fuzzytime", &[], "soon™", &ctx), "soon™");
    }

    #[test]
    fn dateparse_emits_rfc3339() {
        let ctx = FilterCtx::fixed_for_tests();
        assert_eq!(
            run_ctx(
                "dateparse",
                &["yyyy-MM-dd HH:mm:ss"],
                "2025-03-09 18:30:00",
                &ctx
            ),
            "2025-03-09T18:30:00+00:00"
        );
    }

    #[test]
    fn dateparse_with_offset_keeps_the_offset() {
        let ctx = FilterCtx::fixed_for_tests();
        assert_eq!(
            run_ctx(
                "dateparse",
                &["yyyy-MM-dd HH:mm:ss zzz"],
                "2025-03-09 18:30:00 +02:00",
                &ctx
            ),
            "2025-03-09T16:30:00+00:00"
        );
    }

    #[test]
    fn timeparse_fills_missing_date_from_ctx_now() {
        let ctx = FilterCtx::fixed_for_tests(); // 2026-01-15
        assert_eq!(
            run_ctx("timeparse", &["HH:mm"], "18:30", &ctx),
            "2026-01-15T18:30:00+00:00"
        );
    }

    #[test]
    fn unparseable_date_passes_through() {
        let ctx = FilterCtx::fixed_for_tests();
        assert_eq!(run_ctx("dateparse", &["yyyy-MM-dd"], "n/a", &ctx), "n/a");
    }

    #[test]
    fn dateparse_layout_advertising_an_offset_still_parses_a_value_without_one() {
        // Corpus layouts often end in `zzz`, but the scraped value
        // frequently omits the offset entirely. When the offset-aware
        // parse fails, the offset token is stripped from the format and
        // the value is retried naive, assumed UTC.
        let ctx = FilterCtx::fixed_for_tests();
        assert_eq!(
            run_ctx(
                "dateparse",
                &["yyyy-MM-dd HH:mm:ss zzz"],
                "2025-03-09 18:30:00",
                &ctx
            ),
            "2025-03-09T18:30:00+00:00"
        );
    }

    #[test]
    fn timeparse_fractional_second_layout_parses_a_single_dot() {
        // KNOWN ISSUE: `net_layout_to_chrono` maps `fff` to `%.3f`, which
        // itself renders/expects a leading dot. A corpus layout written
        // `ss.fff` (a literal dot, then `fff`) would otherwise translate
        // to a doubled dot (`%S.%.3f`) that never matches a real value's
        // single dot — it must be collapsed before reaching chrono.
        let ctx = FilterCtx::fixed_for_tests();
        assert_eq!(
            run_ctx("timeparse", &["HH:mm:ss.fff"], "18:30:00.123", &ctx),
            "2026-01-15T18:30:00.123+00:00"
        );
    }

    #[test]
    fn unknown_filter_is_an_error() {
        assert!(parse_filter("nonexistent", &[]).is_err());
    }
}
