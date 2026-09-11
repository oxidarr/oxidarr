//! Pure Torznab/Newznab XML rendering.
//!
//! No I/O, no `axum` — these functions take already-resolved domain values
//! ([`Definition`], [`CategoryMap`], [`Release`]) and return a `String` of
//! well-formed XML. The three shapes rendered here (`t=caps`, a search
//! results feed, and an error response) are Prowlarr's own, since Prowlarr's
//! output — not the original Newznab/Torznab spec text, which never nailed
//! down an authoritative XSD — is what Sonarr's Torznab client actually
//! parses. Every shape below is cross-checked against `Prowlarr/Prowlarr` at
//! commit `693c7c3b0e8ec6e9dd792c01e5fa1091260b3be1`:
//!
//! - **caps**: `IndexerCapabilities.GetXDocument()`
//!   (`src/NzbDrone.Core/Indexers/IndexerCapabilities.cs`) — `<server>`,
//!   `<limits>`, `<searching>`'s per-mode `available`/`supportedParams`
//!   attributes, and `<categories>/<category>/<subcat>` (id+name only, no
//!   `description`). A real capture of that shape lives at
//!   `src/NzbDrone.Core.Test/Files/Indexers/Torznab/torznab_animetosho_caps.xml`.
//! - **results**: `NewznabResults.ToXml()`
//!   (`src/NzbDrone.Core/IndexerSearch/NewznabResults.cs`) for the
//!   `<rss version="2.0" xmlns:torznab="...">` envelope and per-item element
//!   order, and `schemas/torznab.xsd` for the attribute names themselves —
//!   whose comment on `peers` (`<!-- seeders + leechers -->`) is the source
//!   for that derivation. A real feed of this shape lives at
//!   `src/NzbDrone.Core.Test/Files/Indexers/Torznab/torznab_morethantv.xml`.
//! - **error**: `NewznabController.CreateErrorXML()`
//!   (`src/Prowlarr.Api.V1/Indexers/NewznabController.cs`) — a bare
//!   `<error code="..." description="..."/>`, e.g. `CreateErrorXML(202, "No
//!   such function (t)")`.
//!
//! `<limits>` has no source in a Cardigann [`Definition`] (nothing in
//! `caps:` declares a per-request result cap), so it is rendered as the
//! fixed `default="100" max="100"` Prowlarr itself falls back to
//! (`IndexerCapabilities`'s constructor defaults both to `100`) rather than
//! invented from nothing.
//!
//! # Rendering can't actually fail
//!
//! [`quick_xml::Writer`] writes to a `Vec<u8>` here, whose [`std::io::Write`]
//! impl never returns `Err` — but its methods are still typed
//! `io::Result<()>`, and this module holds to the same no-`unwrap`/`expect`/
//! `panic` rule as the rest of the crate even for an error that cannot occur
//! in practice. [`render`] threads that `Result` through and degrades to a
//! literal `<error code="900" description="render failure"/>` on the
//! (unreachable) `Err` path rather than unwrapping it.

use std::collections::BTreeMap;
use std::io::{self, Write};

use chrono::{DateTime, Utc};
use oxidarr_cardigann::categories;
use oxidarr_cardigann::catmap::CategoryMap;
use oxidarr_cardigann::model::Definition;
use oxidarr_core::Release;
use quick_xml::Writer;
use quick_xml::events::{BytesDecl, BytesText, Event};

/// Fixed `<limits>` advertised for every indexer; see the module docs for
/// why this cannot be derived from a [`Definition`].
const DEFAULT_LIMIT: &str = "100";

/// The Torznab XML namespace, per `schemas/torznab.xsd` and every real
/// feed's `xmlns:torznab` attribute.
const TORZNAB_NS: &str = "http://torznab.com/schemas/2015/feed";

/// Rendered in place of any (unreachable in practice) write failure; see the
/// module docs' "Rendering can't actually fail" section.
const RENDER_FAILURE_XML: &str =
    r#"<?xml version="1.0" encoding="UTF-8"?><error code="900" description="render failure"/>"#;

/// Renders a `t=caps` response: this definition's declared search modes and
/// mapped categories.
///
/// `search`, `tv-search`, and `movie-search` are always rendered, in that
/// order, one per key `def.caps.modes` may declare (see [`crate::torznab`]'s
/// module docs for the `IndexerCapabilities` this mirrors). A mode absent
/// from `modes` is rendered `available="no"` with the same `q`-only
/// `supportedParams` fallback Prowlarr itself uses for an unavailable mode.
///
/// Categories come from `map.newznab_ids()`, grouped under each id's
/// top-level parent block (via [`categories::parent_of`]); an id that is
/// itself a top-level block is rendered with no `<subcat>` children.
#[must_use]
pub fn render_caps(def: &Definition, map: &CategoryMap) -> String {
    render(|writer| write_caps(writer, def, map))
}

/// Renders a Torznab search-results feed for `releases`, titled `title`.
///
/// See the module docs for the element shape and which `Release` fields
/// become which `torznab:attr`. A `None`/empty field is omitted entirely
/// rather than rendered empty, except `download_volume_factor` and
/// `upload_volume_factor`, which are plain `f32`s (defaulting to `1.0`, per
/// [`Release::default`]) and are therefore always present. `grabs` is
/// rendered as its own `torznab:attr`, positioned right after `peers` —
/// matching the attribute order real Torznab feeds carry it in (see
/// `oxidarr-indexer`'s `tests/fixtures/torznab_feed.xml`) — when
/// [`Release::grabs`] is `Some`.
#[must_use]
pub fn render_results(releases: &[Release], title: &str) -> String {
    render(|writer| write_results(writer, releases, title))
}

/// Renders a bare `<error code="..." description="..."/>` response, per
/// `NewznabController.CreateErrorXML` (see the module docs).
#[must_use]
pub fn render_error(code: u16, description: &str) -> String {
    render(|writer| {
        writer
            .create_element("error")
            .with_attribute(("code", code.to_string().as_str()))
            .with_attribute(("description", description))
            .write_empty()?;
        Ok(())
    })
}

/// Runs `write` over a fresh [`Writer`], preceded by the XML declaration
/// every real Torznab/Newznab response carries, and returns the result as a
/// `String` — or [`RENDER_FAILURE_XML`] on the write failing (unreachable
/// for a `Vec<u8>` sink; see the module docs).
fn render(write: impl FnOnce(&mut Writer<Vec<u8>>) -> io::Result<()>) -> String {
    let mut writer = Writer::new(Vec::new());
    let declared = writer
        .write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))
        .and_then(|()| write(&mut writer));
    match declared {
        Ok(()) => String::from_utf8(writer.into_inner())
            .unwrap_or_else(|_| RENDER_FAILURE_XML.to_string()),
        Err(_) => RENDER_FAILURE_XML.to_string(),
    }
}

fn write_caps<W: Write>(
    writer: &mut Writer<W>,
    def: &Definition,
    map: &CategoryMap,
) -> io::Result<()> {
    writer
        .create_element("caps")
        .write_inner_content(|writer| {
            writer
                .create_element("server")
                .with_attribute(("title", def.name.as_str()))
                .write_empty()?;
            writer
                .create_element("limits")
                .with_attribute(("default", DEFAULT_LIMIT))
                .with_attribute(("max", DEFAULT_LIMIT))
                .write_empty()?;
            write_searching(writer, def)?;
            write_categories(writer, map)?;
            Ok(())
        })?;
    Ok(())
}

fn write_searching<W: Write>(writer: &mut Writer<W>, def: &Definition) -> io::Result<()> {
    writer
        .create_element("searching")
        .write_inner_content(|writer| {
            write_search_mode(writer, "search", def)?;
            write_search_mode(writer, "tv-search", def)?;
            write_search_mode(writer, "movie-search", def)?;
            Ok(())
        })?;
    Ok(())
}

fn write_search_mode<W: Write>(
    writer: &mut Writer<W>,
    mode: &str,
    def: &Definition,
) -> io::Result<()> {
    let params = def.caps.modes.get(mode);
    let available = if params.is_some() { "yes" } else { "no" };
    let supported = params
        .map(|values| values.join(","))
        .filter(|joined| !joined.is_empty())
        .unwrap_or_else(|| "q".to_string());
    writer
        .create_element(mode)
        .with_attribute(("available", available))
        .with_attribute(("supportedParams", supported.as_str()))
        .write_empty()?;
    Ok(())
}

fn write_categories<W: Write>(writer: &mut Writer<W>, map: &CategoryMap) -> io::Result<()> {
    let mut groups: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for id in map.newznab_ids() {
        let parent = categories::parent_of(id).unwrap_or(id);
        let children = groups.entry(parent).or_default();
        if id != parent {
            children.push(id);
        }
    }

    writer
        .create_element("categories")
        .write_inner_content(|writer| {
            for (parent_id, children) in &groups {
                let Some(parent_cat) = categories::by_id(*parent_id) else {
                    continue;
                };
                if children.is_empty() {
                    writer
                        .create_element("category")
                        .with_attribute(("id", parent_id.to_string().as_str()))
                        .with_attribute(("name", parent_cat.name))
                        .write_empty()?;
                } else {
                    writer
                        .create_element("category")
                        .with_attribute(("id", parent_id.to_string().as_str()))
                        .with_attribute(("name", parent_cat.name))
                        .write_inner_content(|writer| {
                            for child_id in children {
                                if let Some(child_cat) = categories::by_id(*child_id) {
                                    writer
                                        .create_element("subcat")
                                        .with_attribute(("id", child_id.to_string().as_str()))
                                        .with_attribute(("name", child_cat.name))
                                        .write_empty()?;
                                }
                            }
                            Ok(())
                        })?;
                }
            }
            Ok(())
        })?;
    Ok(())
}

fn write_results<W: Write>(
    writer: &mut Writer<W>,
    releases: &[Release],
    title: &str,
) -> io::Result<()> {
    writer
        .create_element("rss")
        .with_attribute(("version", "2.0"))
        .with_attribute(("xmlns:torznab", TORZNAB_NS))
        .write_inner_content(|writer| {
            writer
                .create_element("channel")
                .write_inner_content(|writer| {
                    writer
                        .create_element("title")
                        .write_text_content(BytesText::new(title))?;
                    for release in releases {
                        write_item(writer, release)?;
                    }
                    Ok(())
                })?;
            Ok(())
        })?;
    Ok(())
}

fn write_item<W: Write>(writer: &mut Writer<W>, release: &Release) -> io::Result<()> {
    // Used only for `<link>`/`<guid>` fallback: `<enclosure>` must NOT fall
    // back to `magnet_url` here, even though a magnet-only release still
    // needs a `<link>`. `parse_feed` treats any `enclosure` `url` attribute
    // as authoritative `download_url` (it prefers `enclosure_url` over
    // `link` precisely because `link` may be a magnet), so writing a magnet
    // into `<enclosure url="...">` would make a magnet-only release come
    // back with `download_url` wrongly set to the magnet URI instead of
    // staying `None`. See `write_enclosure`'s call site below, which is
    // gated on `release.download_url` alone.
    let download_or_magnet = release
        .download_url
        .as_deref()
        .or(release.magnet_url.as_deref());
    let guid = release
        .details_url
        .clone()
        .unwrap_or_else(|| format!("urn:release:{}", release.title));

    writer
        .create_element("item")
        .write_inner_content(|writer| {
            writer
                .create_element("title")
                .write_text_content(BytesText::new(&release.title))?;
            writer
                .create_element("guid")
                .write_text_content(BytesText::new(&guid))?;
            if let Some(details_url) = &release.details_url {
                writer
                    .create_element("comments")
                    .write_text_content(BytesText::new(details_url))?;
            }
            if let Some(link) = download_or_magnet {
                writer
                    .create_element("link")
                    .write_text_content(BytesText::new(link))?;
            }
            if let Some(publish_date) = release.publish_date {
                write_pub_date(writer, publish_date)?;
            }
            if let Some(size) = release.size {
                writer
                    .create_element("size")
                    .write_text_content(BytesText::new(size.to_string().as_str()))?;
            }
            if let Some(url) = release.download_url.as_deref() {
                write_enclosure(writer, url, release.size)?;
            }
            write_attr_opt(writer, "seeders", release.seeders.map(|v| v.to_string()))?;
            if let (Some(seeders), Some(leechers)) = (release.seeders, release.leechers) {
                write_attr(
                    writer,
                    "peers",
                    &seeders.saturating_add(leechers).to_string(),
                )?;
            }
            write_attr_opt(writer, "grabs", release.grabs.map(|v| v.to_string()))?;
            write_attr_opt(writer, "infohash", release.info_hash.clone())?;
            write_attr_opt(writer, "magneturl", release.magnet_url.clone())?;
            for category in &release.categories {
                write_attr(writer, "category", &category.to_string())?;
            }
            write_attr(
                writer,
                "downloadvolumefactor",
                &release.download_volume_factor.to_string(),
            )?;
            write_attr(
                writer,
                "uploadvolumefactor",
                &release.upload_volume_factor.to_string(),
            )?;
            Ok(())
        })?;
    Ok(())
}

fn write_pub_date<W: Write>(writer: &mut Writer<W>, publish_date: DateTime<Utc>) -> io::Result<()> {
    let text = publish_date.to_rfc2822();
    writer
        .create_element("pubDate")
        .write_text_content(BytesText::new(text.as_str()))?;
    Ok(())
}

fn write_enclosure<W: Write>(
    writer: &mut Writer<W>,
    url: &str,
    size: Option<u64>,
) -> io::Result<()> {
    let length = size.map(|s| s.to_string());
    let mut element = writer
        .create_element("enclosure")
        .with_attribute(("url", url));
    if let Some(length) = &length {
        element = element.with_attribute(("length", length.as_str()));
    }
    element
        .with_attribute(("type", "application/x-bittorrent"))
        .write_empty()?;
    Ok(())
}

fn write_attr<W: Write>(writer: &mut Writer<W>, name: &str, value: &str) -> io::Result<()> {
    writer
        .create_element("torznab:attr")
        .with_attribute(("name", name))
        .with_attribute(("value", value))
        .write_empty()?;
    Ok(())
}

fn write_attr_opt<W: Write>(
    writer: &mut Writer<W>,
    name: &str,
    value: Option<String>,
) -> io::Result<()> {
    if let Some(value) = value {
        write_attr(writer, name, &value)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use chrono::TimeZone;
    use oxidarr_cardigann::model::parse_definition;

    /// Mirrors `oxidarr_cardigann::catmap`'s own corpus sample: two
    /// `categorymappings` rows resolving to `Movies/HD` (2040) and `TV/SD`
    /// (5030), plus a `search`/`tv-search` mode declaration.
    const DEFINITION_YAML: &str = r#"
id: example
name: Example Tracker
caps:
  categorymappings:
    - {id: "6", cat: Movies/HD, desc: "Movies HD"}
    - {id: "9", cat: TV/SD, desc: "TV SD"}
  modes:
    search: [q]
    tv-search: [q, season, ep]
search:
  rows:
    selector: item
  fields:
    title:
      selector: a
"#;

    fn sample_definition() -> Definition {
        parse_definition(DEFINITION_YAML).unwrap()
    }

    /// A single `categorymappings` row mapped to a bare top-level block
    /// (`Audio`, id `3000`) with no subcategory mapped under it — the
    /// `children.is_empty()` branch of `write_categories`.
    const BLOCK_ONLY_DEFINITION_YAML: &str = r#"
id: example
name: Example Tracker
caps:
  categorymappings:
    - {id: "3", cat: Audio, desc: "Audio"}
  modes:
    search: [q]
search:
  rows:
    selector: item
  fields:
    title:
      selector: a
"#;

    fn block_only_definition() -> Definition {
        parse_definition(BLOCK_ONLY_DEFINITION_YAML).unwrap()
    }

    #[test]
    fn render_caps_matches_the_exact_prowlarr_shape() {
        let def = sample_definition();
        let map = CategoryMap::from_definition(&def);

        let xml = render_caps(&def, &map);

        assert_eq!(
            xml,
            concat!(
                r#"<?xml version="1.0" encoding="UTF-8"?>"#,
                r#"<caps><server title="Example Tracker"/>"#,
                r#"<limits default="100" max="100"/>"#,
                r#"<searching>"#,
                r#"<search available="yes" supportedParams="q"/>"#,
                r#"<tv-search available="yes" supportedParams="q,season,ep"/>"#,
                r#"<movie-search available="no" supportedParams="q"/>"#,
                r#"</searching>"#,
                r#"<categories>"#,
                r#"<category id="2000" name="Movies"><subcat id="2040" name="Movies/HD"/></category>"#,
                r#"<category id="5000" name="TV"><subcat id="5030" name="TV/SD"/></category>"#,
                r#"</categories></caps>"#,
            )
        );
    }

    #[test]
    fn render_caps_renders_a_block_only_category_with_no_subcat() {
        let def = block_only_definition();
        let map = CategoryMap::from_definition(&def);

        let xml = render_caps(&def, &map);

        assert_eq!(
            xml,
            concat!(
                r#"<?xml version="1.0" encoding="UTF-8"?>"#,
                r#"<caps><server title="Example Tracker"/>"#,
                r#"<limits default="100" max="100"/>"#,
                r#"<searching>"#,
                r#"<search available="yes" supportedParams="q"/>"#,
                r#"<tv-search available="no" supportedParams="q"/>"#,
                r#"<movie-search available="no" supportedParams="q"/>"#,
                r#"</searching>"#,
                r#"<categories>"#,
                r#"<category id="3000" name="Audio"/>"#,
                r#"</categories></caps>"#,
            )
        );
    }

    fn rich_release() -> Release {
        let mut release = Release::default();
        release.title = r#"Fast & Furious <2026> "X""#.to_string();
        release.size = Some(1_500_000_000);
        release.seeders = Some(120);
        release.leechers = Some(30);
        release.grabs = Some(15);
        release.details_url = Some("https://example.org/details/1".to_string());
        release.download_url = Some("https://example.org/download/1.torrent".to_string());
        release.magnet_url = Some("magnet:?xt=urn:btih:ABCDEF1234567890".to_string());
        release.info_hash = Some("ABCDEF1234567890".to_string());
        release.publish_date = Some(Utc.with_ymd_and_hms(2024, 1, 2, 3, 4, 5).unwrap());
        release.categories = vec![2040, 5030];
        release.download_volume_factor = 0.5;
        release.upload_volume_factor = 2.0;
        release
    }

    fn minimal_release() -> Release {
        let mut release = Release::default();
        release.title = "Minimal.Release".to_string();
        release.download_url = Some("https://example.org/download/2.torrent".to_string());
        release
    }

    /// A magnet-only release: no `download_url` at all, only `magnet_url`.
    /// Regression fixture for the bug where `<enclosure>` was built from
    /// `download_url.or(magnet_url)` — `parse_feed` treats any
    /// `enclosure`'s `url` attribute as authoritative `download_url`, so a
    /// magnet-only release would come back with `download_url` wrongly set
    /// to the magnet URI instead of staying `None`.
    fn magnet_only_release() -> Release {
        let mut release = Release::default();
        release.title = "Magnet.Only.Release".to_string();
        release.magnet_url = Some("magnet:?xt=urn:btih:FEDCBA0987654321".to_string());
        release
    }

    #[test]
    fn render_results_matches_the_exact_prowlarr_shape() {
        let releases = vec![rich_release(), minimal_release()];

        let xml = render_results(&releases, "Example Feed");

        assert_eq!(
            xml,
            concat!(
                r#"<?xml version="1.0" encoding="UTF-8"?>"#,
                r#"<rss version="2.0" xmlns:torznab="http://torznab.com/schemas/2015/feed">"#,
                r#"<channel><title>Example Feed</title>"#,
                r#"<item>"#,
                r#"<title>Fast &amp; Furious &lt;2026&gt; &quot;X&quot;</title>"#,
                r#"<guid>https://example.org/details/1</guid>"#,
                r#"<comments>https://example.org/details/1</comments>"#,
                r#"<link>https://example.org/download/1.torrent</link>"#,
                r#"<pubDate>Tue, 2 Jan 2024 03:04:05 +0000</pubDate>"#,
                r#"<size>1500000000</size>"#,
                r#"<enclosure url="https://example.org/download/1.torrent" length="1500000000" type="application/x-bittorrent"/>"#,
                r#"<torznab:attr name="seeders" value="120"/>"#,
                r#"<torznab:attr name="peers" value="150"/>"#,
                r#"<torznab:attr name="grabs" value="15"/>"#,
                r#"<torznab:attr name="infohash" value="ABCDEF1234567890"/>"#,
                r#"<torznab:attr name="magneturl" value="magnet:?xt=urn:btih:ABCDEF1234567890"/>"#,
                r#"<torznab:attr name="category" value="2040"/>"#,
                r#"<torznab:attr name="category" value="5030"/>"#,
                r#"<torznab:attr name="downloadvolumefactor" value="0.5"/>"#,
                r#"<torznab:attr name="uploadvolumefactor" value="2"/>"#,
                r#"</item>"#,
                r#"<item>"#,
                r#"<title>Minimal.Release</title>"#,
                r#"<guid>urn:release:Minimal.Release</guid>"#,
                r#"<link>https://example.org/download/2.torrent</link>"#,
                r#"<enclosure url="https://example.org/download/2.torrent" type="application/x-bittorrent"/>"#,
                r#"<torznab:attr name="downloadvolumefactor" value="1"/>"#,
                r#"<torznab:attr name="uploadvolumefactor" value="1"/>"#,
                r#"</item>"#,
                r#"</channel></rss>"#,
            )
        );
    }

    /// The strongest correctness gate available for this module: render two
    /// releases, parse the rendered feed back through
    /// `oxidarr_indexer::parse_feed`, and require the parsed `Release`s to
    /// equal the originals field-for-field via `Release`'s own `PartialEq`.
    ///
    /// This only holds because every field either round-trips exactly
    /// (`leechers` is not itself rendered — `peers` is rendered as
    /// `seeders + leechers` per `schemas/torznab.xsd`'s comment on that
    /// attribute, and `parse_feed` inverts it as `peers - seeders`, landing
    /// back on the original `leechers` — see `rss.rs`'s `into_release`) or
    /// is left at `Release::default()` on both sides: `imdb_id`, `tmdb_id`,
    /// `tvdb_id`, `minimum_seed_time`, `minimum_ratio`, and `files` are real
    /// `Release` fields this renderer does not emit at all (outside this
    /// task's scope per the brief) — `grabs` used to be one of these too,
    /// until it was added in Task 10 per that task's own deferred ruling —
    /// and `description`,
    /// `poster`, `genre` are fields `parse_feed` itself never populates from
    /// any Torznab/Newznab element (see its own doc comment) — so neither
    /// side of the round trip ever sets them to anything but `None`.
    #[test]
    fn render_results_round_trips_through_parse_feed() {
        let rich = rich_release();
        let minimal = minimal_release();
        let magnet_only = magnet_only_release();
        let xml = render_results(
            &[rich.clone(), minimal.clone(), magnet_only.clone()],
            "Example Feed",
        );

        let parsed = oxidarr_indexer::parse_feed(xml.as_bytes()).unwrap();

        assert_eq!(parsed, vec![rich, minimal, magnet_only]);
    }

    #[test]
    fn render_error_matches_the_exact_prowlarr_shape() {
        // Prowlarr's own `NewznabController` raises exactly this code and
        // message when a request omits the mandatory `t` parameter.
        let xml = render_error(200, "Missing parameter (t)");

        assert_eq!(
            xml,
            r#"<?xml version="1.0" encoding="UTF-8"?><error code="200" description="Missing parameter (t)"/>"#
        );
    }

    #[test]
    fn render_falls_back_to_the_failure_xml_when_writing_errors() {
        let xml = render(|_writer| Err(io::Error::other("simulated write failure")));

        assert_eq!(xml, RENDER_FAILURE_XML);
    }
}
