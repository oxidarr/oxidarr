//! Newznab/Torznab feed parsing into `Release` values.
//!
//! Streamed with [`quick_xml::Reader`] rather than quick-xml's serde
//! support: `torznab:attr`/`newznab:attr` elements repeat with varying
//! `name=`/`value=` pairs, and every indexer picks its own namespace prefix
//! for them. Neither shape maps onto a fixed `#[derive(Deserialize)]`
//! struct, but comparing [`quick_xml::name::QName::local_name`] (which
//! strips whatever prefix precedes the first `:`) against `b"attr"` handles
//! both prefixes identically.

use chrono::{DateTime, Utc};
use oxidarr_core::Release;
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::QName;

use crate::error::IndexerError;

/// Which text-bearing `<item>` child element is currently being captured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextField {
    Title,
    Guid,
    Link,
    Comments,
    PubDate,
    Size,
}

/// Accumulates one `<item>`'s fields as its child elements stream past, in
/// whatever order the feed presents them.
#[derive(Debug, Default)]
struct ItemAcc {
    title: String,
    guid: Option<String>,
    comments: Option<String>,
    link: Option<String>,
    pub_date: Option<String>,
    enclosure_url: Option<String>,
    enclosure_length: Option<u64>,
    size_override: Option<u64>,
    seeders: Option<u32>,
    peers: Option<u32>,
    grabs: Option<u32>,
    info_hash: Option<String>,
    magnet_url: Option<String>,
    categories: Vec<u32>,
    imdb_id: Option<String>,
    tmdb_id: Option<u32>,
    tvdb_id: Option<u32>,
    download_volume_factor: Option<f32>,
    upload_volume_factor: Option<f32>,
    minimum_seed_time: Option<u64>,
    minimum_ratio: Option<f32>,
    files: Option<u32>,
}

impl ItemAcc {
    /// Resolves the accumulated fields into a canonical [`Release`].
    ///
    /// Two decisions are resolved here rather than as elements stream in,
    /// because either input may arrive in either order within an `<item>`:
    ///
    /// - `details_url` prefers the `comments` element (the tracker's own
    ///   "permalink to details" field); when absent, `guid` is used only if
    ///   it looks like a URL (`http://`/`https://`) rather than an opaque
    ///   identifier.
    /// - `download_url` prefers `enclosure`'s `url` attribute over `link`;
    ///   `link` is used only as a fallback, and only when it is not itself
    ///   a `magnet:` URI (which goes to `magnet_url` instead). An explicit
    ///   `magneturl` attr, if present, wins over a `magnet:` link.
    /// - `size` prefers an explicit `size` element/attr over the
    ///   enclosure's `length` attribute.
    fn into_release(self) -> Release {
        let ItemAcc {
            title,
            guid,
            comments,
            link,
            pub_date,
            enclosure_url,
            enclosure_length,
            size_override,
            seeders,
            peers,
            grabs,
            info_hash,
            magnet_url,
            categories,
            imdb_id,
            tmdb_id,
            tvdb_id,
            download_volume_factor,
            upload_volume_factor,
            minimum_seed_time,
            minimum_ratio,
            files,
        } = self;

        let details_url = comments
            .or_else(|| guid.filter(|g| g.starts_with("http://") || g.starts_with("https://")));

        let link_is_magnet = link.as_deref().is_some_and(|l| l.starts_with("magnet:"));
        let magnet_url = if magnet_url.is_some() {
            magnet_url
        } else if link_is_magnet {
            link.clone()
        } else {
            None
        };
        let download_url = if enclosure_url.is_some() {
            enclosure_url
        } else if link_is_magnet {
            None
        } else {
            link
        };

        let leechers = match (seeders, peers) {
            (Some(s), Some(p)) => Some(p.saturating_sub(s)),
            _ => None,
        };

        let publish_date = pub_date
            .as_deref()
            .and_then(|d| DateTime::parse_from_rfc2822(d).ok())
            .map(|dt| dt.with_timezone(&Utc));

        // `Release` is `#[non_exhaustive]`, which forbids struct-literal
        // construction from outside its crate even with a
        // `..Release::default()` base — only field assignment on an
        // existing value is allowed here.
        let mut release = Release::default();
        release.title = title;
        release.size = size_override.or(enclosure_length);
        release.seeders = seeders;
        release.leechers = leechers;
        release.grabs = grabs;
        release.details_url = details_url;
        release.download_url = download_url;
        release.magnet_url = magnet_url;
        release.info_hash = info_hash;
        release.publish_date = publish_date;
        release.categories = categories;
        release.download_volume_factor = download_volume_factor.unwrap_or(1.0);
        release.upload_volume_factor = upload_volume_factor.unwrap_or(1.0);
        release.imdb_id = imdb_id;
        release.tmdb_id = tmdb_id;
        release.tvdb_id = tvdb_id;
        release.files = files;
        release.minimum_seed_time = minimum_seed_time;
        release.minimum_ratio = minimum_ratio;
        release
    }
}

/// Parses a Newznab or Torznab search response into `Release` values.
///
/// Mapping notes (see [`ItemAcc::into_release`] for the resolution rules
/// applied once an `</item>` closes):
///
/// - `pubDate` is parsed as RFC 2822; an unparseable or missing value
///   leaves `publish_date` as `None` rather than erroring the whole feed.
/// - `torznab:attr`/`newznab:attr` `category` values that do not parse as
///   `u32` are dropped rather than erroring; a plain (non-namespaced) RSS
///   `<category>` text element is not mapped to anything, since Newznab's
///   own categories arrive exclusively via the `attr` form.
/// - `imdbid` is stored exactly as the feed writes it (some indexers send
///   `tt1234567`, others bare digits) — this function does not normalise
///   the `tt` prefix either way.
/// - `downloadvolumefactor`/`uploadvolumefactor` default to `1.0` (neutral,
///   matching [`Release::default`]) when the feed omits them.
/// - `description`, `poster`, and `genre` are left `None`: nothing in the
///   Torznab/Newznab spec maps onto them.
///
/// # Errors
///
/// Returns [`IndexerError::Parse`] if `xml` is not valid UTF-8, is not
/// well-formed XML (naming the byte offset `quick_xml` reports), or is
/// well-formed but never opens a `<channel>` element — the one structural
/// requirement every Newznab/Torznab response shares, even one reporting
/// zero results (which instead yields `Ok(vec![])`).
pub fn parse_feed(xml: &[u8]) -> Result<Vec<Release>, IndexerError> {
    let text = std::str::from_utf8(xml)
        .map_err(|err| IndexerError::Parse(format!("feed is not valid UTF-8: {err}")))?;

    // Deliberately *not* `reader.config_mut().trim_text(true)`: quick-xml's
    // own docs warn that global text trimming corrupts text delimited by
    // entity/character references or CDATA sections (it trims each
    // resulting `Event::Text` fragment at its own edges, not the logical
    // run as a whole — e.g. `Fast &amp; Furious` would lose the spaces
    // around `&`, becoming `Fast&Furious`). It is also unnecessary here:
    // whitespace-only text between sibling elements is only ever read
    // while `field` is `None` (nothing is tracking it), so it is already
    // discarded by the `field`-gated `Event::Text`/`CData`/`GeneralRef`
    // arms below without any trimming.
    let mut reader = Reader::from_str(text);

    let mut releases = Vec::new();
    let mut saw_channel = false;
    let mut current: Option<ItemAcc> = None;
    let mut field: Option<TextField> = None;
    // Raw text accumulated for the currently-open `field`, across however
    // many `Text`/`CData`/`GeneralRef` fragments quick-xml splits it into.
    // Committed to `current` — trimmed exactly once, as a whole — only when
    // `field`'s element closes (see the `Event::End` arm and `commit_field`
    // below). Trimming per-fragment instead of once at commit is exactly
    // what corrupted `Fast &amp; Furious` in an earlier revision of this
    // module (quick-xml splits text around each entity reference, so
    // per-fragment trimming eats the whitespace adjacent to it); trimming
    // only the final, fully-assembled value here strips pretty-printed
    // indentation around a leaf element's content without touching
    // whitespace that belongs to that content.
    let mut buffer = String::new();

    loop {
        let event = match reader.read_event() {
            Ok(event) => event,
            Err(err) => {
                let pos = reader.error_position();
                return Err(IndexerError::Parse(format!(
                    "malformed XML at byte {pos}: {err}"
                )));
            }
        };

        match event {
            Event::Eof => break,
            Event::Start(start) => {
                let name = start.name();
                if local_name_is(name, b"channel") {
                    saw_channel = true;
                } else if local_name_is(name, b"item") {
                    current = Some(ItemAcc::default());
                } else if local_name_is(name, b"enclosure") {
                    apply_enclosure(&start, current.as_mut())?;
                } else if local_name_is(name, b"attr") {
                    apply_attr_element(&start, current.as_mut())?;
                } else if current.is_some() {
                    field = text_field_for(name);
                    buffer.clear();
                }
            }
            Event::Empty(start) => {
                let name = start.name();
                if local_name_is(name, b"channel") {
                    saw_channel = true;
                } else if local_name_is(name, b"item") {
                    releases.push(ItemAcc::default().into_release());
                } else if local_name_is(name, b"enclosure") {
                    apply_enclosure(&start, current.as_mut())?;
                } else if local_name_is(name, b"attr") {
                    apply_attr_element(&start, current.as_mut())?;
                }
            }
            Event::Text(text) if field.is_some() && current.is_some() => {
                let decoded = text
                    .decode()
                    .map_err(|err| IndexerError::Parse(format!("decoding text: {err}")))?;
                // Belt-and-braces, not load-bearing: quick-xml surfaces
                // every character/entity reference as its own
                // `Event::GeneralRef` (handled below), splitting the
                // surrounding text around it — so a well-formed
                // `Event::Text` run should never itself contain an
                // escape sequence for this call to resolve. Kept
                // rather than deleted so a `&name;`-shaped substring
                // that somehow *does* reach here (a reader
                // configuration or quick-xml edge case this module
                // hasn't hit) is still unescaped instead of silently
                // passed through raw.
                let unescaped = quick_xml::escape::unescape(&decoded)
                    .map_err(|err| IndexerError::Parse(format!("unescaping text: {err}")))?;
                buffer.push_str(&unescaped);
            }
            // `<![CDATA[...]]>` sections are literal by definition — no
            // entity unescaping applies, unlike `Event::Text`.
            Event::CData(cdata) if field.is_some() && current.is_some() => {
                let decoded = cdata
                    .decode()
                    .map_err(|err| IndexerError::Parse(format!("decoding CDATA: {err}")))?;
                buffer.push_str(&decoded);
            }
            // quick-xml surfaces a character or entity reference (`&amp;`,
            // `&#38;`, …) inside element text as its own event rather than
            // folding it into the surrounding `Event::Text` runs, so it
            // needs the same resolve-then-append treatment here.
            Event::GeneralRef(entity_ref) if field.is_some() && current.is_some() => {
                let resolved = resolve_entity_ref(&entity_ref)?;
                buffer.push_str(&resolved);
            }
            Event::End(end) => {
                if local_name_is(end.name(), b"item") {
                    if let Some(item) = current.take() {
                        releases.push(item.into_release());
                    }
                } else if let Some(target) = field.take() {
                    if let Some(item) = current.as_mut() {
                        commit_field(item, target, &buffer);
                    }
                    buffer.clear();
                }
            }
            _ => {}
        }
    }

    if !saw_channel {
        return Err(IndexerError::Parse(
            "expected a Newznab/Torznab <rss> feed with a <channel> element; found none"
                .to_string(),
        ));
    }

    Ok(releases)
}

/// Resolves a character reference (`&#38;`/`&#x26;`) or predefined XML
/// entity reference (`&amp;`, `&lt;`, `&gt;`, `&apos;`, `&quot;`) to the
/// string it stands for.
///
/// A named reference that is neither of those (`&nbsp;`, `&hellip;`,
/// `&mdash;`, …  — common in titles that started life as scraped HTML)
/// cannot be resolved without a DTD, which feeds in this protocol never
/// declare. Rather than failing the entire feed over one such reference in
/// one item's title, it degrades to the literal `&name;` text — matching
/// how lenient real-world feed consumers behave, and keeping one bad title
/// from discarding every other, already-parsed release.
///
/// # Errors
///
/// Returns [`IndexerError::Parse`] only for a reference quick-xml itself
/// could not decode, or a malformed numeric reference (`resolve_char_ref`
/// failing) — both indicate the input is not well-formed XML, as opposed
/// to a merely unrecognised (but syntactically valid) named reference.
fn resolve_entity_ref(
    entity_ref: &quick_xml::events::BytesRef<'_>,
) -> Result<String, IndexerError> {
    if let Some(ch) = entity_ref
        .resolve_char_ref()
        .map_err(|err| IndexerError::Parse(format!("invalid character reference: {err}")))?
    {
        return Ok(ch.to_string());
    }

    let name = entity_ref
        .decode()
        .map_err(|err| IndexerError::Parse(format!("decoding entity reference: {err}")))?;
    Ok(quick_xml::escape::resolve_predefined_entity(&name)
        .map_or_else(|| format!("&{name};"), str::to_string))
}

fn local_name_is(name: QName<'_>, target: &[u8]) -> bool {
    name.local_name().as_ref() == target
}

fn text_field_for(name: QName<'_>) -> Option<TextField> {
    match name.local_name().as_ref() {
        b"title" => Some(TextField::Title),
        b"guid" => Some(TextField::Guid),
        b"link" => Some(TextField::Link),
        b"comments" => Some(TextField::Comments),
        b"pubDate" => Some(TextField::PubDate),
        b"size" => Some(TextField::Size),
        _ => None,
    }
}

/// Commits a `field`'s fully-assembled raw text to `item` when that
/// field's element closes, trimming it exactly once as a whole (see the
/// `buffer` comment in [`parse_feed`] for why not per-fragment).
///
/// An empty (or all-whitespace) value leaves the `Option` fields at their
/// default `None` rather than becoming `Some("")` — a `<guid></guid>`
/// with nothing in it is "not provided", the same as no `<guid>` at all.
fn commit_field(item: &mut ItemAcc, field: TextField, raw: &str) {
    let value = raw.trim();
    match field {
        TextField::Title => item.title = value.to_string(),
        TextField::Guid => set_non_empty(&mut item.guid, value),
        TextField::Link => set_non_empty(&mut item.link, value),
        TextField::Comments => set_non_empty(&mut item.comments, value),
        TextField::PubDate => set_non_empty(&mut item.pub_date, value),
        TextField::Size => item.size_override = value.parse().ok(),
    }
}

fn set_non_empty(slot: &mut Option<String>, value: &str) {
    if !value.is_empty() {
        *slot = Some(value.to_string());
    }
}

fn apply_enclosure(start: &BytesStart, item: Option<&mut ItemAcc>) -> Result<(), IndexerError> {
    let Some(item) = item else {
        return Ok(());
    };
    item.enclosure_url = attr_value(start, b"url")?;
    item.enclosure_length = attr_value(start, b"length")?.and_then(|v| v.parse().ok());
    Ok(())
}

fn apply_attr_element(start: &BytesStart, item: Option<&mut ItemAcc>) -> Result<(), IndexerError> {
    let Some(item) = item else {
        return Ok(());
    };
    let name = attr_value(start, b"name")?;
    let value = attr_value(start, b"value")?;
    if let (Some(name), Some(value)) = (name, value) {
        apply_torznab_attr(item, &name, &value);
    }
    Ok(())
}

fn apply_torznab_attr(item: &mut ItemAcc, name: &str, value: &str) {
    match name {
        "seeders" => item.seeders = value.parse().ok(),
        "peers" => item.peers = value.parse().ok(),
        "grabs" => item.grabs = value.parse().ok(),
        "infohash" => item.info_hash = Some(value.to_string()),
        "magneturl" => item.magnet_url = Some(value.to_string()),
        "category" => {
            if let Ok(id) = value.parse::<u32>() {
                item.categories.push(id);
            }
        }
        "imdbid" => item.imdb_id = Some(value.to_string()),
        "downloadvolumefactor" => item.download_volume_factor = value.parse().ok(),
        "uploadvolumefactor" => item.upload_volume_factor = value.parse().ok(),
        "minimumseedtime" => item.minimum_seed_time = value.parse().ok(),
        "minimumratio" => item.minimum_ratio = value.parse().ok(),
        "files" => item.files = value.parse().ok(),
        "tvdbid" => item.tvdb_id = value.parse().ok(),
        "tmdbid" => item.tmdb_id = value.parse().ok(),
        "size" => item.size_override = value.parse().ok(),
        _ => {}
    }
}

/// Reads a single attribute's unescaped value off a start/empty tag.
fn attr_value(start: &BytesStart, name: &[u8]) -> Result<Option<String>, IndexerError> {
    for attr in start.attributes() {
        let attr = attr.map_err(|err| IndexerError::Parse(format!("invalid attribute: {err}")))?;
        if attr.key.local_name().as_ref() == name {
            let value = attr
                .normalized_value(quick_xml::XmlVersion::Explicit1_0)
                .map_err(|err| IndexerError::Parse(format!("invalid attribute value: {err}")))?;
            return Ok(Some(value.into_owned()));
        }
    }
    Ok(None)
}
