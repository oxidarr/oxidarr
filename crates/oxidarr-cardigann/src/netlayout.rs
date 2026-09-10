//! Translation of .NET custom date-format layouts to chrono format strings.
//!
//! Cardigann's `dateparse`/`timeparse` filters (see [`crate::filters::Filter`])
//! carry a layout written against .NET's custom date and time format
//! strings (e.g. `"yyyy-MM-dd HH:mm:ss zzz"`). An earlier version of this
//! module assumed these layouts were Go reference-time strings
//! (`"2006-01-02 15:04:05"`); corpus verification found every
//! dateparse/timeparse argument in the real `.definitions/v11` corpus is
//! .NET-dialect, not Go, so that assumption was wrong and this module
//! targets .NET instead. chrono has no `strftime` support for either
//! by-example dialect, so a layout must be translated into a
//! `strftime`-style format string before chrono can use it. This module
//! owns exactly that translation, so [`crate::filters`]'s
//! `dateparse`/`timeparse` implementation can call it without knowing
//! anything about .NET's layout vocabulary.

use crate::error::CardigannError;

/// One entry in [`TOKENS`]: a .NET custom format letter run and the chrono
/// `strftime` directive it becomes.
type TokenMapping = (&'static str, &'static str);

/// .NET custom date/time format tokens mapped to chrono directives.
///
/// Every .NET token is a run of one repeated letter (`yyyy`, `MMM`, `HH`,
/// ...); different run lengths of the *same* letter mean different things
/// (`yyyy` a four-digit year, `yy` a two-digit one), and a run length not
/// listed here is not a valid token at all — .NET itself rejects it, and
/// so does [`net_layout_to_chrono`]. Because the scanner (see that
/// function) looks up a whole run at once rather than stripping known
/// prefixes off it, the order of entries below does not affect
/// correctness, only readability; they are listed in the same order the
/// task brief gives them in.
const TOKENS: &[TokenMapping] = &[
    ("yyyy", "%Y"),  // four-digit year
    ("yy", "%y"),    // two-digit year
    ("MMMM", "%B"),  // full month name
    ("MMM", "%b"),   // abbreviated month name
    ("MM", "%m"),    // zero-padded month
    ("M", "%-m"),    // unpadded month
    ("dddd", "%A"),  // full weekday name
    ("ddd", "%a"),   // abbreviated weekday name
    ("dd", "%d"),    // zero-padded day
    ("d", "%-d"),    // unpadded day
    ("HH", "%H"),    // zero-padded 24-hour
    ("H", "%-H"),    // unpadded 24-hour
    ("hh", "%I"),    // zero-padded 12-hour
    ("h", "%-I"),    // unpadded 12-hour
    ("mm", "%M"),    // zero-padded minute
    ("m", "%-M"),    // unpadded minute
    ("ss", "%S"),    // zero-padded second
    ("s", "%-S"),    // unpadded second
    ("tt", "%p"),    // AM/PM designator
    ("fff", "%.3f"), // fixed-width fractional second
    ("zzz", "%:z"),  // hour-and-minute numeric offset with colon
    ("zz", "%z"),    // numeric offset without colon
    ("K", "%:z"),    // "kind" (UTC "Z" or a numeric offset with colon)
];

/// Translates a .NET custom date-format layout (as used by Cardigann's
/// `dateparse`/`timeparse` filter arguments) into a chrono `strftime`
/// format string.
///
/// Scans `layout` left to right in a single pass:
/// - `'...'` is a quoted literal: everything between the quotes passes
///   through unchanged (except `%`-escaping, see below), and is never
///   looked up as a token. An unterminated quote (no closing `'`) takes
///   the rest of the layout as its literal content rather than erroring —
///   lenient rather than adding an error case the brief does not call for.
/// - `\x` escapes the single character `x`, emitting it literally without
///   attempting a token lookup. A trailing `\` with nothing after it is
///   emitted as a literal backslash.
/// - A run of the same ASCII letter repeated one or more times is looked
///   up as a whole against [`TOKENS`]. This is what lets `"yyyy-QQ-dd"` be
///   rejected rather than silently reading `Q` as some other, unrelated
///   token: unlike a simple longest-prefix scan, a run that does not
///   exactly match a known token is always an error, never partially
///   matched.
/// - Anything else (digits, punctuation, whitespace) passes through
///   unchanged, except that a literal `%` is escaped to `%%` so it
///   survives being embedded in a `strftime` string.
///
/// # Errors
/// Returns [`CardigannError::Definition`] naming the run when a maximal
/// run of the same repeated ASCII letter does not exactly match a token in
/// [`TOKENS`] (for example `"QQ"`, or `"ttt"` — .NET's AM/PM designator is
/// only ever one or two `t`s) — .NET layouts are built entirely from a
/// fixed vocabulary of letter runs, so an unrecognized one is always a
/// mistake in the definition, not a value this function should pass
/// through.
///
/// `pub` rather than `pub(crate)` only so `lib.rs` can re-export it
/// (`#[doc(hidden)]`) for `tests/corpus.rs`'s translation gate — the
/// `netlayout` module itself stays private, so this is unreachable from
/// outside the crate except through that one re-export.
pub fn net_layout_to_chrono(layout: &str) -> Result<String, CardigannError> {
    let mut out = String::with_capacity(layout.len());
    let mut rest = layout;

    while !rest.is_empty() {
        let mut chars = rest.chars();
        // `rest` is non-empty (the `while` guard), so the first char always
        // exists.
        let c = chars.next().unwrap_or_default();

        if c == '\'' {
            let tail = chars.as_str();
            if let Some(end) = tail.find('\'') {
                push_literal(&mut out, &tail[..end]);
                rest = &tail[end + 1..];
            } else {
                // Unterminated quote: no closing `'` anywhere in the rest
                // of the layout. Rather than adding an error case the
                // brief does not call for, take everything left as this
                // literal's content.
                push_literal(&mut out, tail);
                rest = "";
            }
            continue;
        }

        if c == '\\' {
            let tail = chars.as_str();
            let mut escaped_chars = tail.chars();
            match escaped_chars.next() {
                Some(escaped) => push_literal_char(&mut out, escaped),
                None => push_literal_char(&mut out, '\\'),
            }
            rest = escaped_chars.as_str();
            continue;
        }

        if c.is_ascii_alphabetic() {
            let run_len = rest.chars().take_while(|&ch| ch == c).count();
            let (run, tail) = rest.split_at(run_len);
            let Some((_, directive)) = TOKENS.iter().find(|(token, _)| *token == run) else {
                return Err(CardigannError::Definition {
                    reason: format!("unrecognized letter run {run:?} in time layout {layout:?}"),
                });
            };
            out.push_str(directive);
            rest = tail;
            continue;
        }

        push_literal_char(&mut out, c);
        rest = chars.as_str();
    }

    Ok(out)
}

/// Appends `literal` to `out` verbatim, except that each `%` is escaped to
/// `%%` so it survives being embedded in a `strftime` string.
fn push_literal(out: &mut String, literal: &str) {
    for c in literal.chars() {
        push_literal_char(out, c);
    }
}

/// Appends a single literal character to `out`, escaping `%` to `%%`.
fn push_literal_char(out: &mut String, c: char) {
    if c == '%' {
        out.push_str("%%");
    } else {
        out.push(c);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn translates_common_corpus_layouts() {
        assert_eq!(
            net_layout_to_chrono("yyyy-MM-dd HH:mm:ss zzz").unwrap(),
            "%Y-%m-%d %H:%M:%S %:z"
        );
        assert_eq!(
            net_layout_to_chrono("ddd, dd MMM yyyy HH:mm:ss zzz").unwrap(),
            "%a, %d %b %Y %H:%M:%S %:z"
        );
        assert_eq!(
            net_layout_to_chrono("MMM d yyyy hh:mm tt").unwrap(),
            "%b %-d %Y %I:%M %p"
        );
        assert_eq!(net_layout_to_chrono("dd/MM/yyyy").unwrap(), "%d/%m/%Y");
    }

    #[test]
    fn unknown_token_is_an_error_naming_it() {
        let err = net_layout_to_chrono("yyyy-QQ-dd").unwrap_err();
        assert!(err.to_string().contains("QQ"));
    }

    #[test]
    fn unpadded_and_weekday_month_names_are_distinguished_from_padded_siblings() {
        // Exercises the single-letter family (day/hour24/hour12/minute/
        // second) and the full weekday/month names, none of which the two
        // required tests above touch.
        assert_eq!(
            net_layout_to_chrono("dddd, MMMM d H:h:m:s").unwrap(),
            "%A, %B %-d %-H:%-I:%-M:%-S"
        );
    }

    #[test]
    fn two_digit_year_and_meridiem_tokens() {
        assert_eq!(
            net_layout_to_chrono("MM/dd/yy hh:mm tt").unwrap(),
            "%m/%d/%y %I:%M %p"
        );
    }

    #[test]
    fn fractional_second_and_timezone_family() {
        assert_eq!(
            net_layout_to_chrono("HH:mm:ss.fff").unwrap(),
            "%H:%M:%S.%.3f"
        );
        // "zz" (2 z's) must not be misread as a partial match of "zzz" (3
        // z's), and "K" resolves independently of both.
        assert_eq!(net_layout_to_chrono("zzz zz K").unwrap(), "%:z %z %:z");
    }

    #[test]
    fn quoted_literal_passes_through_unchanged() {
        // The canonical ISO-8601-with-literal-T .NET format.
        assert_eq!(
            net_layout_to_chrono("yyyy-MM-dd'T'HH:mm:ss").unwrap(),
            "%Y-%m-%dT%H:%M:%S"
        );
    }

    #[test]
    fn unterminated_quote_takes_the_rest_of_the_layout_as_literal() {
        assert_eq!(net_layout_to_chrono("yyyy'oops").unwrap(), "%Yoops");
    }

    #[test]
    fn escaped_letter_is_literal_not_a_token() {
        // Without the escape, "h" would be read as the unpadded 12-hour
        // token; escaped, it is just the letter h.
        assert_eq!(net_layout_to_chrono(r"HH\hmm").unwrap(), "%Hh%M");
    }

    #[test]
    fn trailing_backslash_with_nothing_to_escape_is_a_literal_backslash() {
        assert_eq!(net_layout_to_chrono(r"dd\").unwrap(), "%d\\");
    }

    #[test]
    fn literal_percent_is_escaped() {
        assert_eq!(net_layout_to_chrono("100%").unwrap(), "100%%");
    }

    #[test]
    fn percent_inside_a_quoted_literal_is_still_escaped() {
        assert_eq!(net_layout_to_chrono("'100%'").unwrap(), "100%%");
    }

    #[test]
    fn non_letter_separators_pass_through_verbatim() {
        // Bare letters are not exempt from token lookup (a lone unquoted
        // "a" would itself be an unrecognized letter run — see the test
        // above), so this deliberately covers only punctuation and
        // whitespace, which is what "non-letter separator" means here.
        assert_eq!(net_layout_to_chrono(", - / : ").unwrap(), ", - / : ");
    }

    #[test]
    fn unrecognized_run_length_of_a_known_letter_is_an_error() {
        // "tt" is valid but "ttt" is not — .NET's AM/PM designator is only
        // ever one or two characters.
        let err = net_layout_to_chrono("ttt").unwrap_err();
        assert!(err.to_string().contains("ttt"));
    }
}
