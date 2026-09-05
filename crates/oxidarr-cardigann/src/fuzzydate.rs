//! Shared relative/approximate date-time parser backing Cardigann's
//! `timeago` and `fuzzytime` filters.
//!
//! In real Prowlarr the two filters differ only in the caller's intent —
//! `timeago` for "N units ago" phrases, `fuzzytime` for "today"/"yesterday"
//! and absolute-ish fragments — but both route through the same
//! `DateTimeUtil` fuzzy-parsing logic. This module is that one shared
//! parser; [`crate::filters::apply`]'s `TimeAgo` and `FuzzyTime` arms both
//! call it.

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Utc};

/// Parses `input` as a relative or approximate date-time phrase, evaluated
/// relative to `now`. Returns `None` when `input` matches none of the
/// recognized shapes below (or when the arithmetic they imply would
/// overflow — see [`unit_duration`]); callers pass the original input
/// through unchanged in that case (Prowlarr tolerates junk rows rather
/// than dropping them).
///
/// Case-insensitive; `input` is trimmed before matching. A trailing
/// ` ±HH:MM` UTC offset is stripped first if present (see
/// [`strip_trailing_offset`]); the remainder is matched against, in
/// order:
///
/// - `N unit(s) ago` chains, one or more `<number> <unit>` pairs
///   (whitespace or comma separated, an "and" between pairs is ignored),
///   ending in a trailing `ago` — e.g. `3 hours ago`, `1 day 2 hours ago`.
/// - `today` / `yesterday` / `tomorrow`, optionally followed by (an
///   optional `at` and) a time of day — `HH:MM` in 24-hour form, or a
///   12-hour form with a trailing `am`/`pm` marker. No time defaults to
///   midnight on that day.
/// - A bare time of day (same forms as above), assumed to be today.
/// - `Mon DD YYYY` (month name first, explicit year) and `DD Mon` (day
///   first, year assumed from `now`).
///
/// When an offset was stripped, a `today`/`yesterday`/`tomorrow` day, a
/// bare time of day, or `DD Mon`'s implicit year is resolved against the
/// *offset-local* calendar date, not `now`'s own UTC date — the anchor
/// used to resolve them is `now` shifted by the offset
/// (`now.checked_add_signed(offset)`), since the input text describes a
/// day in that local frame (this matters near local midnight: at UTC
/// 20:00 with a `+08:00` offset, the local date has already rolled over
/// to the next day). The resulting local-frame value is then converted
/// back to true UTC by subtracting the same offset. `N unit(s) ago`
/// chains are duration arithmetic against an absolute instant rather than
/// a calendar-date lookup, so they are resolved against the true `now`
/// and returned unshifted.
pub(crate) fn parse_fuzzy(input: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (body, offset) = strip_trailing_offset(trimmed);
    let lower = body.trim().to_lowercase();
    if lower.is_empty() {
        return None;
    }

    if let Some(parsed) = parse_time_ago(&lower, now) {
        return Some(parsed);
    }

    let anchor = match offset {
        Some(offset) => now.checked_add_signed(offset)?,
        None => now,
    };
    let parsed = parse_named_day(&lower, anchor)
        .or_else(|| parse_bare_time(&lower, anchor))
        .or_else(|| parse_month_name_date(&lower, anchor))?;

    match offset {
        Some(offset) => parsed.checked_sub_signed(offset),
        None => Some(parsed),
    }
}

/// Strips a trailing ` ±HH:MM` UTC offset from `s`, if the last six
/// characters parse as one (see [`parse_offset`]). Returns the remaining
/// text (offset and any separating space removed) and the offset as a
/// [`Duration`] to subtract once the day/time has been resolved in its
/// local frame.
///
/// Corpus evidence: `miobt.yml` appends a literal `" +08:00"` (the
/// tracker's fixed local zone) right after translating `今天`/`昨天` to
/// `Today`/`Yesterday`, producing `"Today 00:35 +08:00"` for `fuzzytime` —
/// with no offset handling, that field never resolves for this real
/// tracker.
fn strip_trailing_offset(s: &str) -> (&str, Option<Duration>) {
    let Some(tail_start) = s.len().checked_sub(6) else {
        return (s, None);
    };
    // `str::get` returns `None` rather than panicking on a non-boundary
    // index, so a multi-byte character straddling the split is simply
    // treated as "no offset" instead of crashing.
    let Some(candidate) = s.get(tail_start..) else {
        return (s, None);
    };
    let Some(offset) = parse_offset(candidate) else {
        return (s, None);
    };
    (s[..tail_start].trim_end(), Some(offset))
}

/// Parses a fixed-width `±HH:MM` offset string (exactly six bytes) into
/// the [`Duration`] it represents, or `None` if it is not one (wrong sign
/// character, missing colon, non-numeric hour/minute, or an hour/minute
/// out of range).
fn parse_offset(candidate: &str) -> Option<Duration> {
    if candidate.len() != 6 {
        return None;
    }
    let bytes = candidate.as_bytes();
    let negative = match bytes[0] {
        b'+' => false,
        b'-' => true,
        _ => return None,
    };
    if bytes[3] != b':' {
        return None;
    }
    let hours: i64 = candidate.get(1..3)?.parse().ok()?;
    let minutes: i64 = candidate.get(4..6)?.parse().ok()?;
    if hours > 23 || minutes > 59 {
        return None;
    }
    let magnitude = Duration::try_hours(hours)?.checked_add(&Duration::try_minutes(minutes)?)?;
    Some(if negative { -magnitude } else { magnitude })
}

/// Parses a `N unit(s) ago` chain (already-lowercased `s`), e.g. `3 hours
/// ago` or `1 day 2 hours ago`.
fn parse_time_ago(s: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let without_ago = s.strip_suffix("ago")?.trim();
    if without_ago.is_empty() {
        return None;
    }

    // Commas are just another separator between chain elements; "and" is
    // filler ("1 day and 2 hours ago") and carries no value of its own.
    let normalized: String = without_ago
        .chars()
        .map(|c| if c == ',' { ' ' } else { c })
        .collect();
    let tokens: Vec<&str> = normalized
        .split_whitespace()
        .filter(|t| *t != "and")
        .collect();
    if tokens.is_empty() || !tokens.len().is_multiple_of(2) {
        return None;
    }

    // Every step below is checked rather than the panicking operator
    // form: `count` is attacker-controlled (it comes straight from the
    // scraped page), and a count large enough to overflow chrono's
    // `TimeDelta` bounds — or a running total that overflows once several
    // chain elements are summed — must fall through to `None` (treated as
    // junk) rather than panic the process.
    let mut total = Duration::zero();
    for pair in tokens.as_chunks::<2>().0 {
        let count: i64 = pair[0].parse().ok()?;
        let duration = unit_duration(pair[1], count)?;
        total = total.checked_add(&duration)?;
    }
    now.checked_sub_signed(total)
}

/// Maps a unit word (singular or plural) to the [`Duration`] it represents
/// for `count` of them, or `None` if `count` (or, for month/year, its
/// scaled day count) overflows what a [`Duration`] can represent. A month
/// is treated as 30 days and a year as 365, matching Prowlarr's own
/// `timeago` conversion rather than a calendar month/year.
fn unit_duration(unit: &str, count: i64) -> Option<Duration> {
    match unit {
        "sec" | "secs" | "second" | "seconds" => Duration::try_seconds(count),
        "min" | "mins" | "minute" | "minutes" => Duration::try_minutes(count),
        "hour" | "hours" | "hr" | "hrs" => Duration::try_hours(count),
        "day" | "days" => Duration::try_days(count),
        "week" | "weeks" => Duration::try_weeks(count),
        "month" | "months" => count.checked_mul(30).and_then(Duration::try_days),
        "year" | "years" => count.checked_mul(365).and_then(Duration::try_days),
        _ => None,
    }
}

/// Parses `today`/`yesterday`/`tomorrow` (already-lowercased `s`),
/// optionally followed by an `at` and/or a time of day. No time given
/// defaults to midnight on that day.
fn parse_named_day(s: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let (date, rest) = if let Some(rest) = s.strip_prefix("yesterday") {
        (now.date_naive() - Duration::days(1), rest)
    } else if let Some(rest) = s.strip_prefix("tomorrow") {
        (now.date_naive() + Duration::days(1), rest)
    } else {
        let rest = s.strip_prefix("today")?;
        (now.date_naive(), rest)
    };

    let rest = rest.trim();
    let rest = rest.strip_prefix("at").map_or(rest, str::trim);

    let time = if rest.is_empty() {
        NaiveTime::from_hms_opt(0, 0, 0)?
    } else {
        parse_time_of_day(rest)?
    };
    Some(Utc.from_utc_datetime(&date.and_time(time)))
}

/// Parses a bare time of day (already-lowercased `s`), assumed to fall on
/// `now`'s date.
fn parse_bare_time(s: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let time = parse_time_of_day(s)?;
    Some(Utc.from_utc_datetime(&now.date_naive().and_time(time)))
}

/// Parses a time-of-day fragment (already-lowercased): 24-hour `HH:MM`, or
/// 12-hour `H:MM` / `HH:MM` with a trailing `am`/`pm` marker, with or
/// without a separating space (`6:00 am`, `12:25am`).
fn parse_time_of_day(s: &str) -> Option<NaiveTime> {
    let s = s.trim();
    if let Ok(t) = NaiveTime::parse_from_str(s, "%H:%M") {
        return Some(t);
    }
    for format in ["%I:%M %P", "%I:%M%P"] {
        if let Ok(t) = NaiveTime::parse_from_str(s, format) {
            return Some(t);
        }
    }
    None
}

/// Parses a month-name date (already-lowercased `s`): `Mon DD YYYY`
/// (explicit year) or `DD Mon` (year assumed from `now`).
fn parse_month_name_date(s: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if let Ok(date) = NaiveDate::parse_from_str(s, "%b %d %Y") {
        return Some(Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0)?));
    }
    let with_year = format!("{s} {}", now.year());
    if let Ok(date) = NaiveDate::parse_from_str(&with_year, "%d %b %Y") {
        return Some(Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0)?));
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 15, 12, 0, 0).unwrap()
    }

    #[test]
    fn parses_a_single_unit_ago() {
        assert_eq!(
            parse_fuzzy("3 hours ago", now()),
            Some(Utc.with_ymd_and_hms(2026, 1, 15, 9, 0, 0).unwrap())
        );
    }

    #[test]
    fn parses_a_chained_ago() {
        assert_eq!(
            parse_fuzzy("1 day 2 hours ago", now()),
            Some(Utc.with_ymd_and_hms(2026, 1, 14, 10, 0, 0).unwrap())
        );
    }

    #[test]
    fn parses_month_and_year_ago_with_the_documented_30_365_day_lengths() {
        assert_eq!(
            parse_fuzzy("1 month ago", now()),
            Some(now() - Duration::days(30))
        );
        assert_eq!(
            parse_fuzzy("1 year ago", now()),
            Some(now() - Duration::days(365))
        );
    }

    #[test]
    fn parses_named_days_with_and_without_a_time() {
        assert_eq!(
            parse_fuzzy("yesterday 08:15", now()),
            Some(Utc.with_ymd_and_hms(2026, 1, 14, 8, 15, 0).unwrap())
        );
        assert_eq!(
            parse_fuzzy("Today at 10:32", now()),
            Some(Utc.with_ymd_and_hms(2026, 1, 15, 10, 32, 0).unwrap())
        );
        assert_eq!(
            parse_fuzzy("tomorrow", now()),
            Some(Utc.with_ymd_and_hms(2026, 1, 16, 0, 0, 0).unwrap())
        );
        assert_eq!(
            parse_fuzzy("today", now()),
            Some(Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap())
        );
    }

    #[test]
    fn parses_a_bare_time_as_today() {
        assert_eq!(
            parse_fuzzy("18:30", now()),
            Some(Utc.with_ymd_and_hms(2026, 1, 15, 18, 30, 0).unwrap())
        );
    }

    #[test]
    fn parses_twelve_hour_times_with_am_pm_markers() {
        // Real corpus shapes (abtorrents.yml, 1337x.yml): a space or no
        // space before a lowercase am/pm marker.
        assert_eq!(
            parse_fuzzy("yesterday 6:00 am", now()),
            Some(Utc.with_ymd_and_hms(2026, 1, 14, 6, 0, 0).unwrap())
        );
        assert_eq!(
            parse_fuzzy("12:25am", now()),
            Some(Utc.with_ymd_and_hms(2026, 1, 15, 0, 25, 0).unwrap())
        );
    }

    #[test]
    fn parses_month_name_dates() {
        assert_eq!(
            parse_fuzzy("Mar 02 2025", now()),
            Some(Utc.with_ymd_and_hms(2025, 3, 2, 0, 0, 0).unwrap())
        );
        assert_eq!(
            parse_fuzzy("02 Mar", now()),
            Some(Utc.with_ymd_and_hms(2026, 3, 2, 0, 0, 0).unwrap())
        );
    }

    #[test]
    fn junk_does_not_parse() {
        assert_eq!(parse_fuzzy("soon™", now()), None);
        assert_eq!(parse_fuzzy("", now()), None);
        assert_eq!(parse_fuzzy("   ", now()), None);
    }

    #[test]
    fn absurdly_large_ago_counts_do_not_panic() {
        // Adversarial input: a count large enough to overflow chrono's
        // TimeDelta bounds must fall through to `None` (junk), not panic.
        assert_eq!(parse_fuzzy("999999999999 days ago", now()), None);
        assert_eq!(parse_fuzzy("9223372036854775807 months ago", now()), None);
    }

    #[test]
    fn strips_a_trailing_utc_offset_and_converts_to_utc() {
        // Corpus shape (miobt.yml): 今天/昨天 are replaced with
        // Today/Yesterday, then `append " +08:00"` (the tracker's fixed
        // local zone) runs immediately before `fuzzytime`, producing
        // exactly this string. The day/time is resolved as if already in
        // that offset's local frame, then converted to UTC by subtracting
        // the offset.
        assert_eq!(
            parse_fuzzy("Today 00:35 +08:00", now()),
            Some(Utc.with_ymd_and_hms(2026, 1, 14, 16, 35, 0).unwrap())
        );
    }

    #[test]
    fn named_day_resolution_uses_the_offset_local_date_not_utc_now_s_date() {
        // Boundary case: at 20:00 UTC, +08:00 local time has already
        // rolled over to the next calendar day (2026-01-16). "Today" in
        // that local frame must resolve against 2026-01-16, not against
        // `now`'s own UTC date (2026-01-15) — otherwise the result is off
        // by a day for roughly a third of every day (any UTC hour from
        // 16:00 through 23:59 for a +08:00 offset).
        let clock_near_local_midnight = Utc.with_ymd_and_hms(2026, 1, 15, 20, 0, 0).unwrap();
        assert_eq!(
            parse_fuzzy("Today 00:35 +08:00", clock_near_local_midnight),
            Some(Utc.with_ymd_and_hms(2026, 1, 15, 16, 35, 0).unwrap())
        );
    }
}
