//! Per-definition tracker-category to Newznab-category mapping.
//!
//! A definition's `caps.categorymappings` (see [`crate::model::CategoryMapping`])
//! pairs the tracker's own category id with a Newznab category NAME, taken
//! verbatim from Jackett's `TorznabCatType` (e.g. `"Movies/HD"`, `"TV/SD"`,
//! bare block names like `"Movies"`). [`CategoryMap`] resolves those names
//! against [`crate::categories::all`] once, at construction time, so the
//! rest of the engine — extraction, request building, and the Torznab caps
//! response — only ever deals in ids.
//!
//! Name resolution is a single exact-string lookup against
//! [`crate::categories::all`]. Every top-level block in that table (e.g.
//! `"Movies"`, id `2000`) is itself an entry carrying its own bare name, so
//! a bare `cat: Movies` already resolves through the same lookup as a
//! subcategory like `cat: Movies/HD` — no separate parent-name fallback is
//! needed. Verified against the full v11 corpus (see `tests/corpus.rs`):
//! every one of the 18,262 `categorymappings.cat` values across all 548
//! definitions is one of these 69 distinct names, and every one of those 69
//! is a literal name in [`crate::categories::all`]. (18,262 is the raw
//! grep count of flow-style `categorymappings` entries in the corpus text;
//! `tests/corpus.rs`'s gate parses 18,248 of them through [`parse_definition`]
//! — the 14-row gap is entirely commented-out example rows such as
//! `animebybelka.yml`'s `#    - {id: 1, cat: TV/Anime, ...}`, which a raw
//! `grep` still matches but YAML correctly discards.)
//!
//! `caps.categories` — the tracker's own extra category id/label pairs,
//! e.g. a UI hint list distinct from the Newznab mapping — is a different
//! table entirely and is never consulted here: the same corpus survey found
//! no `categorymappings.cat` value that names one of those tracker-defined
//! categories instead of a standard Newznab one.
//!
//! # `to_tracker` only expands parent-to-child, never child-to-parent
//!
//! Querying a parent block id (e.g. `5000` for `TV`) matches every tracker
//! mapped to one of its children (`5030`, `5040`, …) — but querying a
//! child id (e.g. `5030` for `TV/SD`) does NOT match a tracker mapped only
//! to the bare parent block (`5000`). This mirrors Jackett's own reference
//! behaviour exactly, not an oversight: Jackett's
//! `TorznabCapabilitiesCategories.ExpandTorznabQueryCategories` (in
//! `TorznabCapabilitiesCategories.cs`) only adds a query category's
//! children to the expanded set; adding the *parent* of a child query
//! category is gated behind a `mapChildrenCatsToParent` flag that defaults
//! to `false`, and `CardigannIndexer.cs`'s `PerformQuery` — the only call
//! site that matters for this crate, at its single
//! `MapTorznabCapsToTrackers(query)` call — never opts in. (Verified
//! against `Jackett/Jackett` commit `efda16b4b167008dff3716e9da73c1c7c17db1ff`;
//! `mapChildrenCatsToParent: true` is used by a handful of hand-coded,
//! non-Cardigann indexers such as `BitHDTV.cs`, which are out of scope for
//! this crate.) So a tracker categorized only at the block level is
//! correctly absent from a subcategory search — that is what real Jackett
//! does for every Cardigann-driven indexer, and this module matches it.

use crate::categories;
use crate::model::Definition;

/// Maps a definition's tracker category ids onto the standard Newznab
/// category table, and back.
///
/// Built once per definition via [`CategoryMap::from_definition`].
#[derive(Debug, Clone)]
pub struct CategoryMap {
    entries: Vec<Entry>,
    defaults: Vec<String>,
}

/// One resolved `categorymappings` row: a tracker category id paired with
/// the Newznab id its `cat` name resolved to.
#[derive(Debug, Clone)]
struct Entry {
    tracker_id: String,
    newznab_id: u32,
}

impl CategoryMap {
    /// Builds a category map from a definition's `caps.categorymappings`.
    ///
    /// A row whose `cat` does not resolve to a known Newznab category name
    /// contributes no [`Entry`] (proven absent across the full v11 corpus
    /// by `tests/corpus.rs`'s gate; kept defensive here — skipped rather
    /// than panicking — so a future upstream addition this module doesn't
    /// yet know about degrades to "unmapped" instead of crashing the
    /// engine). Its `default` flag is still honoured even when its `cat`
    /// doesn't resolve, since `defaults()` is about which tracker id to use
    /// absent a category filter, not about the mapping itself.
    #[must_use]
    pub fn from_definition(def: &Definition) -> Self {
        let mut entries = Vec::with_capacity(def.caps.categorymappings.len());
        let mut defaults = Vec::new();
        for mapping in &def.caps.categorymappings {
            if let Some(category) = categories::all().iter().find(|c| c.name == mapping.cat) {
                entries.push(Entry {
                    tracker_id: mapping.id.clone(),
                    newznab_id: category.id,
                });
            }
            if mapping.default && !defaults.contains(&mapping.id) {
                defaults.push(mapping.id.clone());
            }
        }
        Self { entries, defaults }
    }

    /// Newznab ids for a tracker category id (usually 1; empty when
    /// unmapped). Declaration order, deduplicated.
    #[must_use]
    pub fn to_newznab(&self, tracker_id: &str) -> Vec<u32> {
        let mut ids = Vec::new();
        for entry in &self.entries {
            if entry.tracker_id == tracker_id && !ids.contains(&entry.newznab_id) {
                ids.push(entry.newznab_id);
            }
        }
        ids
    }

    /// Tracker ids whose mapping matches any of the given Newznab ids,
    /// EXPANDING parents: asking for a top-level id (e.g. `5000`) also
    /// matches trackers mapped to any of its subcategories (`5030`,
    /// `5040`, …). The reverse does NOT hold — asking for a subcategory id
    /// does not match a tracker mapped only to its parent block; see the
    /// module-level docs for why that matches Jackett's own reference
    /// behaviour. Declaration order, deduplicated.
    #[must_use]
    pub fn to_tracker(&self, newznab: &[u32]) -> Vec<String> {
        let mut ids = Vec::new();
        for entry in &self.entries {
            let matches = newznab.iter().any(|&wanted| {
                wanted == entry.newznab_id
                    || categories::parent_of(entry.newznab_id) == Some(wanted)
            });
            if matches && !ids.contains(&entry.tracker_id) {
                ids.push(entry.tracker_id.clone());
            }
        }
        ids
    }

    /// Tracker ids flagged `default: true`, used when a search names no
    /// category.
    #[must_use]
    pub fn defaults(&self) -> &[String] {
        &self.defaults
    }

    /// Every distinct Newznab id this definition maps to, sorted ascending.
    #[must_use]
    pub fn newznab_ids(&self) -> Vec<u32> {
        let mut ids: Vec<u32> = self.entries.iter().map(|e| e.newznab_id).collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::model::parse_definition;

    const SAMPLE: &str = r#"
id: example
name: Example
caps:
  categorymappings:
    - {id: "6", cat: Movies/HD, desc: "Movies HD", default: true}
    - {id: "9", cat: TV/SD, desc: "TV SD"}
    - {id: "12", cat: Movies/HD, desc: "Movies HD again"}
search:
  rows:
    selector: item
  fields:
    title:
      selector: a
"#;

    fn sample_map() -> CategoryMap {
        let def = parse_definition(SAMPLE).unwrap();
        CategoryMap::from_definition(&def)
    }

    #[test]
    fn to_newznab_resolves_a_mapped_tracker_id() {
        let map = sample_map();
        assert_eq!(map.to_newznab("6"), vec![2040]);
    }

    #[test]
    fn to_newznab_is_empty_for_an_unmapped_tracker_id() {
        let map = sample_map();
        assert_eq!(map.to_newznab("99"), Vec::<u32>::new());
    }

    #[test]
    fn to_tracker_finds_every_tracker_id_mapped_to_a_newznab_id() {
        let map = sample_map();
        assert_eq!(
            map.to_tracker(&[2040]),
            vec!["6".to_string(), "12".to_string()]
        );
    }

    #[test]
    fn to_tracker_expands_a_parent_id_to_its_children() {
        let map = sample_map();
        let tracker_ids = map.to_tracker(&[2000]);
        assert!(tracker_ids.contains(&"6".to_string()));
        assert!(tracker_ids.contains(&"12".to_string()));
    }

    #[test]
    fn defaults_lists_tracker_ids_flagged_default() {
        let map = sample_map();
        assert_eq!(map.defaults(), &["6".to_string()]);
    }

    #[test]
    fn newznab_ids_is_sorted_and_distinct() {
        let map = sample_map();
        assert_eq!(map.newznab_ids(), vec![2040, 5030]);
    }

    /// A tracker category mapped only to the bare `TV` block (`5000`), not
    /// to any subcategory.
    const BLOCK_ONLY_SAMPLE: &str = r#"
id: example
name: Example
caps:
  categorymappings:
    - {id: "7", cat: TV, desc: "TV"}
search:
  rows:
    selector: item
  fields:
    title:
      selector: a
"#;

    fn block_only_map() -> CategoryMap {
        let def = parse_definition(BLOCK_ONLY_SAMPLE).unwrap();
        CategoryMap::from_definition(&def)
    }

    #[test]
    fn to_tracker_matches_the_block_query_itself() {
        let map = block_only_map();
        assert_eq!(map.to_tracker(&[5000]), vec!["7".to_string()]);
    }

    #[test]
    fn to_tracker_does_not_expand_a_subcategory_query_to_a_block_only_mapping() {
        // Reviewed and verified against Jackett's own reference behaviour
        // (see the module docs' "`to_tracker` only expands parent-to-child"
        // section): `ExpandTorznabQueryCategories` never adds a child
        // query category's parent unless `mapChildrenCatsToParent` is set,
        // and Cardigann's own `PerformQuery` never sets it. A tracker
        // mapped only to bare `TV` (5000) is therefore correctly absent
        // from a `TV/SD` (5030) query.
        let map = block_only_map();
        assert_eq!(map.to_tracker(&[5030]), Vec::<String>::new());
    }

    #[test]
    fn to_tracker_does_not_match_an_unrelated_top_level_block() {
        // Pins the non-match: a Movies subcategory query must not match a
        // TV-mapped entry.
        let map = block_only_map();
        assert_eq!(map.to_tracker(&[2040]), Vec::<String>::new());
    }
}
