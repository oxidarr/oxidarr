//! The 21 filter functions used by the Cardigann v11 corpus, ordered in the
//! enum by frequency of use.

use crate::error::CardigannError;
use regex::Regex;

/// A parsed filter invocation.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Filter {
    /// Regex substitution with capture-group backreferences (`$1`, …).
    ReReplace {
        /// The compiled pattern to match against the input.
        pattern: Box<Regex>,
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
    Regexp(Box<Regex>),
    /// Reads a single query-string parameter from a URL or query string.
    QueryString(String),
    /// Marker filter used by Cardigann to combine adjacent conditions;
    /// carries no transformation of its own.
    AndMatch,
    /// Splits the input on a separator and keeps one indexed part.
    Split {
        /// The separator to split on.
        sep: String,
        /// The zero-based index of the part to keep.
        index: usize,
    },
    /// Trims whitespace, or the given characters, from both ends.
    Trim(Option<String>),
    /// Parses a relative "time ago" phrase. Deferred to the engine.
    TimeAgo,
    /// Parses a fuzzy/approximate time phrase. Deferred to the engine.
    FuzzyTime,
    /// Validates the input against a Cardigann-defined rule. Deferred to
    /// the engine.
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
        Regex::new(p)
            .map(Box::new)
            .map_err(|e| CardigannError::Filter {
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
            index: arg(1).parse().unwrap_or(0),
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
        } => pattern
            .replace_all(input, replacement.as_str())
            .into_owned(),
        Filter::Replace { from, to } => input.replace(from.as_str(), to),
        Filter::Append(s) => format!("{input}{s}"),
        Filter::Prepend(s) => format!("{s}{input}"),
        Filter::Regexp(re) => re
            .captures(input)
            .and_then(|c| c.get(1).or_else(|| c.get(0)))
            .map_or_else(String::new, |m| m.as_str().to_string()),
        Filter::QueryString(key) => query_param(input, key),
        Filter::Split { sep, index } => input
            .split(sep.as_str())
            .nth(*index)
            .unwrap_or_default()
            .to_string(),
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
        // Date/time and validation handling is delegated to the engine,
        // which owns the clock; `andmatch` is a marker with no
        // transformation of its own. Both pass the input through
        // unchanged here.
        Filter::DateParse(_)
        | Filter::TimeParse(_)
        | Filter::TimeAgo
        | Filter::FuzzyTime
        | Filter::Validate(_)
        | Filter::AndMatch => input.to_string(),
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

fn urlencoding_encode(input: &str) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(b));
            }
            _ => {
                // `out` has just enough capacity reserved above; the write
                // cannot fail.
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

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
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn html_decode(input: &str) -> String {
    input
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

fn fold_diacritics(input: &str) -> String {
    input
        .chars()
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'ö' | 'õ' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            'Á' | 'À' | 'Â' | 'Ä' | 'Ã' | 'Å' => 'A',
            'É' | 'È' | 'Ê' | 'Ë' => 'E',
            'Í' | 'Ì' | 'Î' | 'Ï' => 'I',
            'Ó' | 'Ò' | 'Ô' | 'Ö' | 'Õ' => 'O',
            'Ú' | 'Ù' | 'Û' | 'Ü' => 'U',
            'Ç' => 'C',
            'Ñ' => 'N',
            other => other,
        })
        .collect()
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
        assert_eq!(run("urlencode", &[], "a b"), "a%20b");
        assert_eq!(run("urldecode", &[], "a%20b"), "a b");
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
    fn unknown_filter_is_an_error() {
        assert!(parse_filter("nonexistent", &[]).is_err());
    }
}
