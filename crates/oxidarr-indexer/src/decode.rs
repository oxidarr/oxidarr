//! Decodes a tracker page body per its definition's declared `encoding`.

use encoding_rs::Encoding;

/// Decodes `bytes` per `encoding_label`, resolved via
/// [`encoding_rs::Encoding::for_label`] (WHATWG label matching — ASCII
/// case-insensitive, leading/trailing ASCII whitespace trimmed). An
/// unresolvable label, including an empty string, falls back to UTF-8: a
/// definition's declared `encoding` is untrusted corpus input like
/// everything else this engine parses, and a wrong or unrecognised label
/// should degrade to the same lossy UTF-8 decoding this replaces, never
/// become a hard error. A byte-order mark present in `bytes` overrides
/// `encoding_label` entirely (via `Encoding::decode`'s BOM sniffing): a page
/// that ships its own BOM is stating its actual encoding first-hand, which
/// outranks a definition label that may simply be stale.
///
/// # Corpus survey
/// `grep -rh '^encoding:' .definitions/v11 | sort | uniq -c` (task 5,
/// 2026-09-11) surfaced: `UTF-8`, `iso-8859-1`/`ISO-8859-1`, `ISO-8859-2`,
/// `tis-620`, `windows-1250`, `windows-1251`, `windows-1252`,
/// `windows-1255`, `windows-1256`, `windows-874`. `all_corpus_labels_resolve`
/// below pins that every one of these actually resolves via `for_label`,
/// rather than silently falling back lossy for a label this engine has
/// never checked.
pub(crate) fn decode_body(bytes: &[u8], encoding_label: &str) -> String {
    let encoding = Encoding::for_label(encoding_label.as_bytes()).unwrap_or(encoding_rs::UTF_8);
    let (decoded, _, _) = encoding.decode(bytes);
    decoded.into_owned()
}

#[cfg(test)]
mod tests {
    use super::decode_body;

    #[test]
    fn windows_1251_label_decodes_real_bytes_correctly() {
        // "Тест" encoded in windows-1251.
        let bytes = [0xD2, 0xE5, 0xF1, 0xF2];
        assert_eq!(decode_body(&bytes, "windows-1251"), "Тест");
    }

    #[test]
    fn utf8_label_passes_valid_utf8_through_unchanged() {
        let bytes = "Hello, world".as_bytes();
        assert_eq!(decode_body(bytes, "UTF-8"), "Hello, world");
    }

    #[test]
    fn unknown_label_falls_back_to_utf8_lossy() {
        let bytes = "plain ascii text".as_bytes();
        assert_eq!(
            decode_body(bytes, "not-a-real-encoding"),
            "plain ascii text"
        );
    }

    #[test]
    fn empty_label_falls_back_to_utf8_lossy() {
        let bytes = "plain ascii text".as_bytes();
        assert_eq!(decode_body(bytes, ""), "plain ascii text");
    }

    #[test]
    fn a_byte_order_mark_overrides_a_wrong_declared_label() {
        // Regression pin, not new behaviour: `decode_body` already calls
        // `Encoding::decode` (BOM-sniffing), not
        // `decode_without_bom_handling`, so a BOM present in `bytes` always
        // wins over `encoding_label` — see this function's doc comment.
        // UTF-8 BOM (EF BB BF) followed by "Тест" as UTF-8 bytes, decoded
        // with the (wrong) label "windows-1251": the BOM must still win,
        // yielding the correct UTF-8 text rather than mis-decoding the
        // UTF-8 bytes as windows-1251.
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice("Тест".as_bytes());
        assert_eq!(decode_body(&bytes, "windows-1251"), "Тест");
    }

    #[test]
    fn all_corpus_labels_resolve_via_for_label() {
        // Distinct `encoding:` values across the v11 corpus, from the
        // survey command in this module's doc comment above (task 5,
        // 2026-09-11). Pinned here so a future definition whose encoding
        // label this engine has never exercised fails loudly (this test
        // breaks) instead of silently decoding lossy.
        let corpus_labels = [
            "UTF-8",
            "iso-8859-1",
            "ISO-8859-1",
            "ISO-8859-2",
            "tis-620",
            "windows-1250",
            "windows-1251",
            "windows-1252",
            "windows-1255",
            "windows-1256",
            "windows-874",
        ];
        for label in corpus_labels {
            assert!(
                encoding_rs::Encoding::for_label(label.as_bytes()).is_some(),
                "corpus encoding label {label:?} does not resolve via Encoding::for_label"
            );
        }
    }
}
