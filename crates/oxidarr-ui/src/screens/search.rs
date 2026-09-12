//! The search screen: a query/indexer/category form over
//! `GET /api/v1/search`, and a results table — the last of this crate's four
//! screens, and the simplest: no add/edit flow, no `ScreenState` (compare
//! `crate::screens::applications`/`crate::screens::indexers`, both of which
//! need one). [`SearchFormView`] and [`SearchResultsView`] are this
//! screen's own presentational halves — plain props in, no context, no
//! fetching, trivially SSR-snapshotted (`tests/snapshots.rs`); [`Search`]
//! is the routed container that fetches the indexer list, holds the form's
//! own draft state and the last search's own results, and feeds both views.
//!
//! # Indexer "multi-select" is a checkbox list, not a `<select multiple>`
//!
//! The requirement is an "indexer multi-select". A native `<select
//! multiple>` needs reading a change event's own `selectedOptions`
//! collection back out — `dioxus`'s own form event does not expose that
//! directly, unlike a checkbox's plain `checked` bool. A checkbox per
//! indexer (this screen's own [`SearchFormView`]) gives the same "pick any
//! subset" semantics with the same per-item `onchange` convention every
//! other screen's own toggles already use (see
//! `crate::screens::indexers::IndexerListView`'s own enable checkbox), at
//! no cost to the actual requirement: an "any subset of indexers" selector,
//! not a specific HTML element.
//!
//! # No selection means "search every enabled indexer"
//!
//! [`build_search_params`]'s own `indexer_ids` is passed straight through
//! from [`Search`]'s own `selected_indexer_ids` — an empty selection is not
//! special-cased into "select everything" client-side, because it already
//! is: `oxidarr_prowl::api::search`'s own "Indexer selection" section
//! documents that an empty `indexerIds` list means every *enabled* indexer,
//! and a non-empty one means only the named ids. This screen's own checkbox
//! list starts with nothing checked, which is exactly that same "search
//! everything enabled" default.
//!
//! # Results are rendered in server order — never re-sorted here
//!
//! `oxidarr_prowl::api::search`'s own "Result shape and ordering" section:
//! the server already sorts the concatenated release list by `seeders`
//! descending (releases with no reported `seeders` sorted last, ties kept
//! in their original relative order). [`SearchResultsView`] renders
//! `results` in the order it receives them, with no `sort`/`sort_by` of its
//! own — re-sorting client-side would silently disagree with the one
//! ordering contract the server already promises, for no benefit.
//!
//! # A 200 naming fewer indexers than requested is not detectable here
//!
//! That same module's own "Partial-failure semantics" section: a `200`
//! response is exactly "every successful indexer's releases, concatenated"
//! — it carries no list of which selected indexers actually ran, only the
//! releases that came back. If three indexers were selected and one failed,
//! this client receives a `200` with the other two indexers' releases and
//! has no way to tell, from that response alone, that a third indexer was
//! even selected, let alone that it failed. Only a *total* failure (every
//! selected indexer failed) is visible here, as the `400`
//! [`crate::api::UiError::Problem`] [`handle_error`] already routes to the
//! banner — a partial failure is a
//! silent gap in this screen's own feedback, not a bug to work around: there
//! is nothing in the wire response this client could use to close it.
//!
//! # Avoiding a `Signal` read guard held across `.await`
//!
//! [`Search`]'s own `on_search` handler clones the shared `ApiClient` out of
//! its `Signal` in a synchronous statement before the surrounding
//! `async move` block's first `.await` — see
//! `crate::screens::indexers`'s own doc comment ("Avoiding a `Signal` read
//! guard held across `.await`") for exactly why `api.read().clone().search(..).await`
//! in one statement is unsound here. This screen sends no resource payload
//! of its own (it only ever reads `GET /api/v1/search`), so the sibling
//! "outbound `sync_error`" rule those two screens' own doc comments carry
//! does not apply to anything built here.

use chrono::{DateTime, Utc};
use dioxus::prelude::*;

use crate::app::{Session, SharedApi, handle_error};
use crate::dto::{IndexerResource, ReleaseResource, SearchParams};

/// The binary-unit ladder [`humanize_size`] climbs — `KiB`/`MiB`/`GiB`/`TiB`,
/// matching the units a release's own size is actually measured in (disk
/// space), not the decimal (`KB`/`MB`/...) convention some tools use for the
/// same bytes.
const SIZE_UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];

/// Renders `bytes` as a human-readable size: `None` is `"—"` (no size
/// known); under 1024 bytes renders as a plain integer (`"1023 B"`, no
/// decimal — a byte count under one KiB is already exact and small, a
/// decimal point would add noise, not precision); 1024 and over climbs
/// [`SIZE_UNITS`] by repeatedly dividing by 1024.0 while the value is still
/// at least 1024, rendering the final value with exactly one decimal place
/// (`"1.0 KiB"`, `"1.5 KiB"`, `"1.0 MiB"` for exactly `1 << 20` — the
/// division stops climbing once the value drops back under 1024, so a value
/// that lands exactly on a unit boundary always shows in the *next* unit up,
/// never as a four-digit count in the unit below it). Caps at `TiB` — no
/// release this client will ever render is realistically measured in
/// petabytes, and an unbounded ladder would need a fifth committed test case
/// for no practical benefit.
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    reason = "bytes is a release's own download size; converting to f64 for a one-decimal \
              human-readable string is this function's whole job, and no release is ever close \
              to the ~2^53 byte mark where that conversion would actually lose precision"
)]
pub fn humanize_size(bytes: Option<u64>) -> String {
    let Some(bytes) = bytes else {
        return "—".to_string();
    };
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit_index = 0;
    while value >= 1024.0 && unit_index < SIZE_UNITS.len() - 1 {
        value /= 1024.0;
        unit_index += 1;
    }
    format!("{value:.1} {}", SIZE_UNITS[unit_index])
}

/// One count-plus-unit pair `n` ago, singular when `n == 1` (`"1 hour"`, not
/// `"1 hours"`). Shared by every rung of [`humanize_age`]'s own ladder below
/// "just now".
fn pluralize(n: i64, unit: &str) -> String {
    if n == 1 {
        format!("1 {unit}")
    } else {
        format!("{n} {unit}s")
    }
}

/// Renders how long ago `publish` was, relative to `now` — `now` is always
/// injected by the caller (this screen's own [`Search`] container passes
/// its own freshly read `Utc::now()`), never read from inside this function,
/// so every case below is exercised by a plain, deterministic native test
/// with no wall-clock dependency at all.
///
/// `None` (`publishDate` absent on the wire — the magnet-only release
/// fixture this screen's own snapshot pins) renders as `"—"`, the same
/// "nothing known" marker [`humanize_size`] uses. A `publish` that is at or
/// after `now` (`publish` in the future — clock skew between this client and
/// whichever indexer reported it, not expected in practice but not
/// impossible either) clamps to zero rather than rendering a negative
/// count, so it renders the same as a genuinely fresh release: `"just now"`.
///
/// The ladder above that: minutes under an hour, hours under a day, days
/// under 30, months (flat 30-day buckets — a real calendar month has no
/// fixed length, and this screen has no use for one) under 365 days, years
/// beyond that. Every rung but "just now" pluralizes via [`pluralize`].
#[must_use]
pub fn humanize_age(publish: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    let Some(publish) = publish else {
        return "—".to_string();
    };
    let seconds = (now - publish).num_seconds().max(0);
    if seconds < 60 {
        return "just now".to_string();
    }
    if seconds < 3600 {
        return pluralize(seconds / 60, "minute");
    }
    if seconds < 86_400 {
        return pluralize(seconds / 3600, "hour");
    }
    if seconds < 30 * 86_400 {
        return pluralize(seconds / 86_400, "day");
    }
    if seconds < 365 * 86_400 {
        return pluralize(seconds / (30 * 86_400), "month");
    }
    pluralize(seconds / (365 * 86_400), "year")
}

/// Parses a `publishDate` wire string (RFC 3339 — see
/// `crate::dto::ReleaseResource::publish_date`'s own doc comment) into the
/// [`DateTime<Utc>`] [`humanize_age`] wants. `None` on a missing field
/// (already `#[serde(default)]`-handled by [`ReleaseResource`] itself) and
/// on a string that fails to parse alike — a release whose feed sent an
/// unparsable date is exactly as "no date known" as one that sent none at
/// all; there is no honest halfway rendering for a corrupt date this client
/// did not send.
#[must_use]
pub fn parse_publish_date(publish_date: Option<&str>) -> Option<DateTime<Utc>> {
    let raw = publish_date?;
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Renders an optional release count (`seeders`/`leechers`) — `"—"` for
/// `None`, the plain number otherwise. Both columns share this one
/// rendering rather than each inlining its own `match`, since a null count
/// means the exact same thing in either column: the indexer's own feed did
/// not report one (see the `release_seeders_null.json` wire fixture this
/// screen's own results snapshot pins).
#[must_use]
pub fn format_optional_count(value: Option<u32>) -> String {
    match value {
        Some(n) => n.to_string(),
        None => "—".to_string(),
    }
}

/// Parses this screen's own categories input (a comma-separated list of
/// newznab category ids, typed freehand — see [`SearchFormView`]'s own hint
/// text) into the `Vec<u32>` [`SearchParams::categories`] wants. Mirrors
/// `oxidarr_prowl::api::search::parse_search_params`'s own leniency for the
/// same parameter: each comma-separated part is trimmed and parsed, and a
/// part that does not parse as a plain non-negative integer (a stray
/// letter, a trailing empty part after a trailing comma, ...) is dropped
/// rather than failing the whole input — a typo in one id should not block
/// searching on the other, valid ids already typed.
#[must_use]
pub fn parse_category_ids(input: &str) -> Vec<u32> {
    input
        .split(',')
        .filter_map(|part| part.trim().parse::<u32>().ok())
        .collect()
}

/// Builds this screen's own outbound [`SearchParams`] from the form's raw
/// draft state: `query` is trimmed, becoming `None` (rather than
/// `Some(String::new())`) when blank so [`SearchParams::to_query_string`]
/// omits the parameter entirely, matching how a client that never typed a
/// query looks identical, on the wire, to one that typed and then cleared
/// it. `indexer_ids` is carried through as-is — already a plain `Vec<i32>`
/// of ids [`Search`]'s own checkbox toggling collected, needing no further
/// parsing. `categories` is [`parse_category_ids`]'s own result.
#[must_use]
pub fn build_search_params(
    query: &str,
    indexer_ids: &[i32],
    categories_input: &str,
) -> SearchParams {
    let trimmed = query.trim();
    SearchParams {
        query: if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        },
        indexer_ids: indexer_ids.to_vec(),
        categories: parse_category_ids(categories_input),
    }
}

/// The search form's presentational half — see this module's own doc
/// comment ("Indexer \"multi-select\" is a checkbox list") for why
/// `indexers` renders as a checkbox per entry rather than a native
/// `<select multiple>`.
#[component]
pub fn SearchFormView(
    query: String,
    indexers: Vec<IndexerResource>,
    selected_indexer_ids: Vec<i32>,
    categories: String,
    on_query_change: EventHandler<String>,
    on_indexer_toggle: EventHandler<i32>,
    on_categories_change: EventHandler<String>,
    on_search: EventHandler<()>,
) -> Element {
    rsx! {
        div { id: "search-form",
            h1 { "Search" }
            form {
                onsubmit: move |event| {
                    event.prevent_default();
                    on_search.call(());
                },
                label {
                    "Query"
                    input {
                        r#type: "text",
                        value: "{query}",
                        oninput: move |event| on_query_change.call(event.value()),
                    }
                }
                fieldset {
                    legend { "Indexers" }
                    for indexer in &indexers {
                        {
                            let id = indexer.id;
                            let checked = selected_indexer_ids.contains(&id);
                            rsx! {
                                label { key: "{id}",
                                    input {
                                        r#type: "checkbox",
                                        checked,
                                        onchange: move |_| on_indexer_toggle.call(id),
                                    }
                                    "{indexer.name}"
                                }
                            }
                        }
                    }
                }
                label {
                    "Categories"
                    input {
                        r#type: "text",
                        value: "{categories}",
                        oninput: move |event| on_categories_change.call(event.value()),
                    }
                    p { class: "hint",
                        "Comma-separated newznab category ids, e.g. 5000,5030 for TV SD/HD"
                    }
                }
                button { r#type: "submit", "Search" }
            }
        }
    }
}

/// One release row's own link buttons: `info` (`infoUrl` on the wire, a
/// details page), then `download` (`downloadUrl`, a `.torrent`/`.nzb`), then
/// `magnet` (`magnetUrl`) — present-if-set, in that order, matching
/// [`ReleaseResource`]'s own field order (named without each one's shared
/// `_url` suffix here only because `#[component]`'s own generated props
/// struct does not forward this function's attributes, so
/// `clippy::struct_field_names` cannot be silenced with an `#[allow]` the
/// way a plain function's could — the wire's own field names are still
/// `infoUrl`/`downloadUrl`/`magnetUrl`, unchanged). The magnet-only release
/// this screen's own results snapshot pins (no `infoUrl`, no `downloadUrl`)
/// renders only the magnet link; the null-seeders fixture (`infoUrl` and
/// `downloadUrl`, no `magnetUrl`) renders the other two and no magnet link.
#[component]
fn ReleaseLinksView(
    info: Option<String>,
    download: Option<String>,
    magnet: Option<String>,
) -> Element {
    rsx! {
        if let Some(url) = info {
            a { href: "{url}", target: "_blank", rel: "noopener noreferrer", "Info" }
        }
        if let Some(url) = download {
            a { href: "{url}", target: "_blank", rel: "noopener noreferrer", "Download" }
        }
        if let Some(url) = magnet {
            a { href: "{url}", "Magnet" }
        }
    }
}

/// The results table — see this module's own doc comment ("Results are
/// rendered in server order") for why `results` is never sorted here.
/// `results.is_empty()` (a genuine `200` with zero releases, as opposed to
/// "no search run yet" — [`Search`]'s own container tells those two apart
/// and only renders this view for the former) renders a plain empty-state
/// message instead of a table with no rows.
#[component]
pub fn SearchResultsView(results: Vec<ReleaseResource>, now: DateTime<Utc>) -> Element {
    if results.is_empty() {
        return rsx! {
            div { id: "search-results",
                p { "No results found." }
            }
        };
    }
    rsx! {
        div { id: "search-results",
            table {
                thead {
                    tr {
                        th { "Title" }
                        th { "Indexer" }
                        th { "Size" }
                        th { "Seeders" }
                        th { "Leechers" }
                        th { "Age" }
                        th { "Links" }
                    }
                }
                tbody {
                    for release in results {
                        {
                            let age = humanize_age(
                                parse_publish_date(release.publish_date.as_deref()),
                                now,
                            );
                            rsx! {
                                tr { key: "{release.guid:?}-{release.title}",
                                    td { "{release.title}" }
                                    td { "{release.indexer}" }
                                    td { "{humanize_size(Some(release.size))}" }
                                    td { "{format_optional_count(release.seeders)}" }
                                    td { "{format_optional_count(release.leechers)}" }
                                    td { "{age}" }
                                    td {
                                        ReleaseLinksView {
                                            info: release.info_url.clone(),
                                            download: release.download_url.clone(),
                                            magnet: release.magnet_url.clone(),
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The routed, data-fetching container — see this module's own doc
/// comment. Not exercised by any native test (same reason as
/// `crate::screens::applications::Applications`'s own non-test half): every
/// `*View` component above carries this screen's real test coverage.
#[component]
pub fn Search() -> Element {
    let api = use_context::<SharedApi>();
    let mut session = use_context::<Session>();

    let mut query = use_signal(String::new);
    let mut selected_indexer_ids = use_signal(Vec::<i32>::new);
    let mut categories = use_signal(String::new);
    let mut results = use_signal(|| None::<Vec<ReleaseResource>>);

    // See this module's own doc comment ("Avoiding a `Signal` read guard
    // held across `.await`") for why the client is cloned out of the
    // `Signal` in this synchronous closure, before the `async move` block
    // even exists.
    let indexers = use_resource(move || {
        let client = api.read().clone();
        async move { client.indexers().await }
    });

    let indexer_list = match &*indexers.read() {
        None => return rsx! { p { "Loading…" } },
        Some(Ok(list)) => list.clone(),
        Some(Err(err)) => {
            handle_error(err.clone(), &mut session);
            return rsx! {};
        }
    };

    rsx! {
        SearchFormView {
            query: query.read().clone(),
            indexers: indexer_list,
            selected_indexer_ids: selected_indexer_ids.read().clone(),
            categories: categories.read().clone(),
            on_query_change: move |value| query.set(value),
            on_indexer_toggle: move |id: i32| {
                let mut ids = selected_indexer_ids.write();
                if let Some(pos) = ids.iter().position(|existing| *existing == id) {
                    ids.remove(pos);
                } else {
                    ids.push(id);
                    ids.sort_unstable();
                }
            },
            on_categories_change: move |value| categories.set(value),
            on_search: move |()| {
                let params = build_search_params(
                    &query.read(),
                    &selected_indexer_ids.read(),
                    &categories.read(),
                );
                let client = api.read().clone();
                spawn(async move {
                    let mut session = session;
                    match client.search(&params).await {
                        Ok(list) => results.set(Some(list)),
                        Err(err) => handle_error(err, &mut session),
                    }
                });
            },
        }
        if let Some(list) = results.read().clone() {
            SearchResultsView { results: list, now: Utc::now() }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn humanize_size_with_no_value_renders_an_em_dash() {
        assert_eq!(humanize_size(None), "—");
    }

    #[test]
    fn humanize_size_renders_zero_bytes_as_a_plain_count() {
        assert_eq!(humanize_size(Some(0)), "0 B");
    }

    #[test]
    fn humanize_size_stays_in_bytes_just_under_one_kib() {
        assert_eq!(humanize_size(Some(1023)), "1023 B");
    }

    #[test]
    fn humanize_size_renders_exactly_one_kib() {
        assert_eq!(humanize_size(Some(1024)), "1.0 KiB");
    }

    #[test]
    fn humanize_size_renders_one_and_a_half_kib() {
        assert_eq!(humanize_size(Some(1536)), "1.5 KiB");
    }

    #[test]
    fn humanize_size_renders_exactly_one_mib_not_1024_kib() {
        assert_eq!(humanize_size(Some(1 << 20)), "1.0 MiB");
    }

    #[test]
    fn humanize_size_renders_gib_and_tib() {
        assert_eq!(humanize_size(Some(1 << 30)), "1.0 GiB");
        assert_eq!(humanize_size(Some(1_u64 << 40)), "1.0 TiB");
    }

    #[test]
    fn humanize_size_caps_at_tib_for_a_larger_value() {
        assert_eq!(humanize_size(Some(1_u64 << 50)), "1024.0 TiB");
    }

    fn dt(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339)
            .expect("valid RFC 3339 fixture timestamp")
            .with_timezone(&Utc)
    }

    #[test]
    fn humanize_age_with_no_publish_date_renders_an_em_dash() {
        assert_eq!(humanize_age(None, dt("2026-01-01T00:00:00Z")), "—");
    }

    #[test]
    fn humanize_age_renders_just_now_under_a_minute() {
        let now = dt("2026-01-01T00:00:30Z");
        let publish = dt("2026-01-01T00:00:00Z");
        assert_eq!(humanize_age(Some(publish), now), "just now");
    }

    #[test]
    fn humanize_age_clamps_a_future_publish_date_to_just_now() {
        let now = dt("2026-01-01T00:00:00Z");
        let publish = dt("2026-01-01T00:05:00Z");
        assert_eq!(humanize_age(Some(publish), now), "just now");
    }

    #[test]
    fn humanize_age_renders_minutes_singular_and_plural() {
        let now = dt("2026-01-01T01:00:00Z");
        assert_eq!(
            humanize_age(Some(dt("2026-01-01T00:59:00Z")), now),
            "1 minute"
        );
        assert_eq!(
            humanize_age(Some(dt("2026-01-01T00:55:00Z")), now),
            "5 minutes"
        );
    }

    #[test]
    fn humanize_age_renders_hours_singular_and_plural() {
        let now = dt("2026-01-02T00:00:00Z");
        assert_eq!(
            humanize_age(Some(dt("2026-01-01T23:00:00Z")), now),
            "1 hour"
        );
        assert_eq!(
            humanize_age(Some(dt("2026-01-01T21:00:00Z")), now),
            "3 hours"
        );
    }

    #[test]
    fn humanize_age_renders_days_singular_and_plural() {
        let now = dt("2026-01-10T00:00:00Z");
        assert_eq!(humanize_age(Some(dt("2026-01-09T00:00:00Z")), now), "1 day");
        assert_eq!(
            humanize_age(Some(dt("2026-01-08T00:00:00Z")), now),
            "2 days"
        );
    }

    #[test]
    fn humanize_age_renders_months_under_a_year() {
        let now = dt("2026-06-01T00:00:00Z");
        assert_eq!(
            humanize_age(Some(dt("2026-04-01T00:00:00Z")), now),
            "2 months"
        );
    }

    #[test]
    fn humanize_age_renders_years_beyond_a_year() {
        let now = dt("2026-01-01T00:00:00Z");
        assert_eq!(
            humanize_age(Some(dt("2024-01-01T00:00:00Z")), now),
            "2 years"
        );
    }

    #[test]
    fn parse_publish_date_with_no_field_returns_none() {
        assert_eq!(parse_publish_date(None), None);
    }

    #[test]
    fn parse_publish_date_parses_a_z_suffixed_rfc3339_string() {
        assert_eq!(
            parse_publish_date(Some("2026-01-15T10:30:00Z")),
            Some(dt("2026-01-15T10:30:00Z"))
        );
    }

    #[test]
    fn parse_publish_date_with_an_unparsable_string_returns_none() {
        assert_eq!(parse_publish_date(Some("not a date")), None);
    }

    #[test]
    fn format_optional_count_renders_a_present_value() {
        assert_eq!(format_optional_count(Some(3)), "3");
    }

    #[test]
    fn format_optional_count_renders_an_em_dash_for_none() {
        assert_eq!(format_optional_count(None), "—");
    }

    #[test]
    fn parse_category_ids_with_a_blank_input_returns_empty() {
        assert_eq!(parse_category_ids(""), Vec::<u32>::new());
    }

    #[test]
    fn parse_category_ids_splits_and_trims_a_comma_separated_list() {
        assert_eq!(parse_category_ids("5000, 5030"), vec![5000, 5030]);
    }

    #[test]
    fn parse_category_ids_drops_a_part_that_does_not_parse() {
        assert_eq!(parse_category_ids("abc,5000"), vec![5000]);
    }

    #[test]
    fn parse_category_ids_drops_a_trailing_empty_part() {
        assert_eq!(parse_category_ids("5000,"), vec![5000]);
    }

    #[test]
    fn build_search_params_with_a_blank_query_omits_it() {
        let params = build_search_params("   ", &[], "");
        assert_eq!(params.query, None);
    }

    #[test]
    fn build_search_params_trims_a_non_blank_query() {
        let params = build_search_params("  ubuntu  ", &[], "");
        assert_eq!(params.query, Some("ubuntu".to_string()));
    }

    #[test]
    fn build_search_params_carries_indexer_ids_through_unchanged() {
        let params = build_search_params("", &[1, 2, 3], "");
        assert_eq!(params.indexer_ids, vec![1, 2, 3]);
    }

    #[test]
    fn build_search_params_parses_the_categories_input() {
        let params = build_search_params("", &[], "5000,5030");
        assert_eq!(params.categories, vec![5000, 5030]);
    }
}
