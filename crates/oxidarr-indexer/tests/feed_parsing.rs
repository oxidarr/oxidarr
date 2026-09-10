//! Integration tests for Newznab/Torznab feed parsing, against the
//! hand-written fixtures in `tests/fixtures/` (shared with `form_login.rs`).
#![allow(clippy::unwrap_used, clippy::panic)]

use chrono::{TimeZone, Utc};
use oxidarr_core::Release;
use oxidarr_indexer::{IndexerError, parse_feed};

const TORZNAB_FEED: &[u8] = include_bytes!("fixtures/torznab_feed.xml");
const NEWZNAB_FEED: &[u8] = include_bytes!("fixtures/newznab_feed.xml");
const MALFORMED_FEED: &[u8] = include_bytes!("fixtures/feed_malformed.xml");

#[test]
fn parses_torznab_feed_into_full_and_minimal_releases() {
    let releases = parse_feed(TORZNAB_FEED).unwrap();
    assert_eq!(releases.len(), 2);

    let mut full = Release::default();
    full.title = "Some.Show.S01E01.1080p.WEB-DL".to_string();
    full.size = Some(123_456_789);
    full.seeders = Some(42);
    full.leechers = Some(8);
    full.grabs = Some(7);
    full.details_url = Some("https://example.org/details/1".to_string());
    full.download_url = Some("https://example.org/download/1.torrent".to_string());
    full.magnet_url = Some(
        "magnet:?xt=urn:btih:ABCDEF0123456789ABCDEF0123456789ABCDEF01&dn=Some.Show".to_string(),
    );
    full.info_hash = Some("ABCDEF0123456789ABCDEF0123456789ABCDEF01".to_string());
    full.publish_date = Some(Utc.with_ymd_and_hms(2026, 9, 6, 12, 34, 56).unwrap());
    full.categories = vec![5000, 5040];
    full.download_volume_factor = 0.0;
    full.upload_volume_factor = 2.0;
    full.imdb_id = Some("tt1234567".to_string());
    full.tmdb_id = Some(98765);
    full.tvdb_id = Some(654_321);
    full.files = Some(3);
    full.minimum_seed_time = Some(172_800);
    full.minimum_ratio = Some(1.5);
    assert_eq!(releases[0], full);

    let mut minimal = Release::default();
    minimal.title = "Minimal.Release.2026".to_string();
    minimal.publish_date = Some(Utc.with_ymd_and_hms(2026, 9, 7, 0, 0, 0).unwrap());
    // guid is present ("minimal-guid-002") but does not look like a URL, and
    // there is no `comments` element, so `details_url` stays `None`.
    assert_eq!(releases[1], minimal);
}

#[test]
fn parses_newznab_feed_with_attr_variant_and_size_element() {
    let releases = parse_feed(NEWZNAB_FEED).unwrap();
    assert_eq!(releases.len(), 1);

    let mut expected = Release::default();
    expected.title = "Some.Movie.2026.1080p.BluRay".to_string();
    // The `<size>` element (987654321) overrides the enclosure's
    // `length="111111"` attribute.
    expected.size = Some(987_654_321);
    expected.seeders = Some(0);
    expected.leechers = Some(0);
    expected.grabs = Some(15);
    // No `comments` element; `guid` is a URL, so it is used as `details_url`.
    expected.details_url = Some("https://example.org/details/nzb-1".to_string());
    expected.download_url = Some("https://example.org/download/nzb-1.nzb".to_string());
    expected.publish_date = Some(Utc.with_ymd_and_hms(2026, 9, 8, 8, 0, 0).unwrap());
    expected.categories = vec![2040];
    expected.download_volume_factor = 0.5;
    expected.upload_volume_factor = 3.0;
    expected.imdb_id = Some("1234567".to_string());
    assert_eq!(releases[0], expected);
}

#[test]
fn empty_channel_yields_no_releases() {
    let xml = b"<?xml version=\"1.0\"?><rss version=\"2.0\"><channel><title>Empty</title></channel></rss>";
    assert_eq!(parse_feed(xml).unwrap(), Vec::new());
}

#[test]
fn malformed_xml_is_a_parse_error_naming_the_byte_offset() {
    let err = parse_feed(MALFORMED_FEED).unwrap_err();
    match err {
        IndexerError::Parse(message) => {
            assert!(
                message.contains("byte"),
                "expected the byte offset to be named, got: {message}"
            );
        }
        other => panic!("expected IndexerError::Parse, got {other:?}"),
    }
}

#[test]
fn feed_without_rss_channel_is_a_parse_error() {
    let xml = b"<?xml version=\"1.0\"?><not-a-feed>hello</not-a-feed>";
    let err = parse_feed(xml).unwrap_err();
    match err {
        IndexerError::Parse(message) => {
            assert!(
                message.contains("channel"),
                "expected the missing element to be named, got: {message}"
            );
        }
        other => panic!("expected IndexerError::Parse, got {other:?}"),
    }
}

#[test]
fn leechers_floors_at_zero_when_seeders_exceed_peers() {
    let xml: &[u8] = br#"<?xml version="1.0"?>
<rss version="2.0" xmlns:torznab="http://torznab.com/schemas/2015/feed">
  <channel>
    <item>
      <title>Overseeded.Release</title>
      <guid>overseeded-guid</guid>
      <torznab:attr name="seeders" value="10" />
      <torznab:attr name="peers" value="3" />
    </item>
  </channel>
</rss>"#;
    let releases = parse_feed(xml).unwrap();
    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0].seeders, Some(10));
    // `peers` (3) is smaller than `seeders` (10): a naive `p - s` would
    // underflow/panic on `u32`. The real feed shape has peers count
    // seeders too, so this is an out-of-spec but real-world value some
    // indexers still send.
    assert_eq!(releases[0].leechers, Some(0));
}

#[test]
fn cdata_title_and_guid_are_captured_literally() {
    let xml: &[u8] = br#"<?xml version="1.0"?>
<rss version="2.0">
  <channel>
    <item>
      <title><![CDATA[Some.Show <Director's Cut>]]></title>
      <guid><![CDATA[https://example.org/details/cdata-1]]></guid>
    </item>
  </channel>
</rss>"#;
    let releases = parse_feed(xml).unwrap();
    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0].title, "Some.Show <Director's Cut>");
    // CDATA content is literal (not entity-unescaped), and the brief's
    // `guid`-looks-like-a-URL fallback still applies to it.
    assert_eq!(
        releases[0].details_url,
        Some("https://example.org/details/cdata-1".to_string())
    );
}

#[test]
fn bare_magnet_link_without_magneturl_attr_routes_to_magnet_url() {
    let xml: &[u8] = br#"<?xml version="1.0"?>
<rss version="2.0">
  <channel>
    <item>
      <title>Magnet.Only.Release</title>
      <guid>magnet-only-guid</guid>
      <link>magnet:?xt=urn:btih:0123456789ABCDEF0123456789ABCDEF01234567&amp;dn=Magnet.Only</link>
    </item>
  </channel>
</rss>"#;
    let releases = parse_feed(xml).unwrap();
    assert_eq!(releases.len(), 1);
    assert_eq!(
        releases[0].magnet_url,
        Some(
            "magnet:?xt=urn:btih:0123456789ABCDEF0123456789ABCDEF01234567&dn=Magnet.Only"
                .to_string()
        )
    );
    assert_eq!(releases[0].download_url, None);
}

#[test]
fn size_attr_overrides_enclosure_length_when_no_size_element() {
    let xml: &[u8] = br#"<?xml version="1.0"?>
<rss version="2.0" xmlns:torznab="http://torznab.com/schemas/2015/feed">
  <channel>
    <item>
      <title>Size.Attr.Release</title>
      <guid>size-attr-guid</guid>
      <enclosure url="https://example.org/download/size-attr.torrent" length="1000" />
      <torznab:attr name="size" value="2048" />
    </item>
  </channel>
</rss>"#;
    let releases = parse_feed(xml).unwrap();
    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0].size, Some(2048));
}

#[test]
fn non_numeric_category_attr_is_silently_dropped() {
    let xml: &[u8] = br#"<?xml version="1.0"?>
<rss version="2.0" xmlns:torznab="http://torznab.com/schemas/2015/feed">
  <channel>
    <item>
      <title>Category.Release</title>
      <guid>category-guid</guid>
      <torznab:attr name="category" value="not-a-number" />
      <torznab:attr name="category" value="5000" />
    </item>
  </channel>
</rss>"#;
    let releases = parse_feed(xml).unwrap();
    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0].categories, vec![5000]);
}

#[test]
fn unresolvable_named_entity_degrades_to_literal_text_without_erroring() {
    // `&nbsp;` is a common HTML entity in scraped titles, but it is not one
    // of the 5 XML-predefined entities and there is no DTD to resolve it
    // against. A single bad entity in one item must not zero out the whole
    // feed: it degrades to the literal `&nbsp;` text, and every other item
    // still parses.
    let xml: &[u8] = br#"<?xml version="1.0"?>
<rss version="2.0">
  <channel>
    <item>
      <title>Scraped&nbsp;Title</title>
      <guid>nbsp-guid</guid>
    </item>
    <item>
      <title>Second.Release</title>
      <guid>second-guid</guid>
    </item>
  </channel>
</rss>"#;
    let releases = parse_feed(xml).unwrap();
    assert_eq!(releases.len(), 2);
    assert_eq!(releases[0].title, "Scraped&nbsp;Title");
    assert_eq!(releases[1].title, "Second.Release");
}

#[test]
fn entity_in_title_resolves_ampersand() {
    let xml: &[u8] = br#"<?xml version="1.0"?>
<rss version="2.0">
  <channel>
    <item>
      <title>Fast &amp; Furious</title>
      <guid>fast-furious-guid</guid>
    </item>
  </channel>
</rss>"#;
    let releases = parse_feed(xml).unwrap();
    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0].title, "Fast & Furious");
}

#[test]
fn pretty_printed_whitespace_is_trimmed_once_at_element_close() {
    // Every other fixture/inline feed in this file is single-line-per-
    // element. Real indexers commonly pretty-print their responses, which
    // pads leaf element text with a leading/trailing newline and
    // indentation — this must be trimmed once, from the whole assembled
    // value, without disturbing the internal spacing around an entity
    // reference (`Fast &amp; Furious`, split by quick-xml into
    // `Text`/`GeneralRef`/`Text` fragments around the `&`).
    let xml: &[u8] = br#"<?xml version="1.0"?>
<rss version="2.0">
  <channel>
    <item>
      <title>
        Fast &amp; Furious
      </title>
      <guid>
        https://example.org/details/pretty-1
      </guid>
      <pubDate>
        Sun, 06 Sep 2026 12:34:56 +0000
      </pubDate>
      <size>
        123456789
      </size>
    </item>
  </channel>
</rss>"#;
    let releases = parse_feed(xml).unwrap();
    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0].title, "Fast & Furious");
    assert_eq!(
        releases[0].details_url,
        Some("https://example.org/details/pretty-1".to_string())
    );
    assert_eq!(
        releases[0].publish_date,
        Some(Utc.with_ymd_and_hms(2026, 9, 6, 12, 34, 56).unwrap())
    );
    assert_eq!(releases[0].size, Some(123_456_789));
}
