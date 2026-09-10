//! The standard Newznab category table.
//!
//! Every id/name/parent triple below is copied verbatim from Jackett's
//! `TorznabCatType` (`src/Jackett.Common/Models/TorznabCatType.cs` at
//! commit `efda16b4b167008dff3716e9da73c1c7c17db1ff` — the `AllCats` array
//! for ids/names, and the `SubCategories` groupings built in its static
//! constructor for parentage); this module does not invent categories or
//! names. The task brief that spawned this module expected a
//! `TorznabCatType.generated.cs`; no such file exists in the current
//! `Jackett/Jackett` repository (confirmed via a full tree listing at the
//! same commit), so the real, non-generated `TorznabCatType.cs` was used
//! instead — see the task report for how that was verified.

/// One standard Newznab category.
///
/// A top-level block (e.g. `TV`) has `parent: None` and an id that is a
/// multiple of 1000; a subcategory (e.g. `TV/Anime`) has `parent` set to
/// its block's id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewznabCategory {
    /// The Newznab category id, e.g. `5070` for `TV/Anime`.
    pub id: u32,
    /// The category's Torznab name, e.g. `"TV/Anime"`.
    pub name: &'static str,
    /// The id of this category's top-level block, or `None` if this
    /// category is itself a top-level block.
    pub parent: Option<u32>,
}

/// Every standard category and subcategory, ordered ascending by id. Kept
/// sorted so [`by_id`] can binary-search it; `all_is_sorted_ascending_by_id`
/// below guards that invariant.
const CATEGORIES: &[NewznabCategory] = &[
    NewznabCategory {
        id: 1000,
        name: "Console",
        parent: None,
    },
    NewznabCategory {
        id: 1010,
        name: "Console/NDS",
        parent: Some(1000),
    },
    NewznabCategory {
        id: 1020,
        name: "Console/PSP",
        parent: Some(1000),
    },
    NewznabCategory {
        id: 1030,
        name: "Console/Wii",
        parent: Some(1000),
    },
    NewznabCategory {
        id: 1040,
        name: "Console/XBox",
        parent: Some(1000),
    },
    NewznabCategory {
        id: 1050,
        name: "Console/XBox 360",
        parent: Some(1000),
    },
    NewznabCategory {
        id: 1060,
        name: "Console/Wiiware",
        parent: Some(1000),
    },
    NewznabCategory {
        id: 1070,
        name: "Console/XBox 360 DLC",
        parent: Some(1000),
    },
    NewznabCategory {
        id: 1080,
        name: "Console/PS3",
        parent: Some(1000),
    },
    NewznabCategory {
        id: 1090,
        name: "Console/Other",
        parent: Some(1000),
    },
    NewznabCategory {
        id: 1110,
        name: "Console/3DS",
        parent: Some(1000),
    },
    NewznabCategory {
        id: 1120,
        name: "Console/PS Vita",
        parent: Some(1000),
    },
    NewznabCategory {
        id: 1130,
        name: "Console/WiiU",
        parent: Some(1000),
    },
    NewznabCategory {
        id: 1140,
        name: "Console/XBox One",
        parent: Some(1000),
    },
    NewznabCategory {
        id: 1180,
        name: "Console/PS4",
        parent: Some(1000),
    },
    NewznabCategory {
        id: 2000,
        name: "Movies",
        parent: None,
    },
    NewznabCategory {
        id: 2010,
        name: "Movies/Foreign",
        parent: Some(2000),
    },
    NewznabCategory {
        id: 2020,
        name: "Movies/Other",
        parent: Some(2000),
    },
    NewznabCategory {
        id: 2030,
        name: "Movies/SD",
        parent: Some(2000),
    },
    NewznabCategory {
        id: 2040,
        name: "Movies/HD",
        parent: Some(2000),
    },
    NewznabCategory {
        id: 2045,
        name: "Movies/UHD",
        parent: Some(2000),
    },
    NewznabCategory {
        id: 2050,
        name: "Movies/BluRay",
        parent: Some(2000),
    },
    NewznabCategory {
        id: 2060,
        name: "Movies/3D",
        parent: Some(2000),
    },
    NewznabCategory {
        id: 2070,
        name: "Movies/DVD",
        parent: Some(2000),
    },
    NewznabCategory {
        id: 2080,
        name: "Movies/WEB-DL",
        parent: Some(2000),
    },
    NewznabCategory {
        id: 3000,
        name: "Audio",
        parent: None,
    },
    NewznabCategory {
        id: 3010,
        name: "Audio/MP3",
        parent: Some(3000),
    },
    NewznabCategory {
        id: 3020,
        name: "Audio/Video",
        parent: Some(3000),
    },
    NewznabCategory {
        id: 3030,
        name: "Audio/Audiobook",
        parent: Some(3000),
    },
    NewznabCategory {
        id: 3040,
        name: "Audio/Lossless",
        parent: Some(3000),
    },
    NewznabCategory {
        id: 3050,
        name: "Audio/Other",
        parent: Some(3000),
    },
    NewznabCategory {
        id: 3060,
        name: "Audio/Foreign",
        parent: Some(3000),
    },
    NewznabCategory {
        id: 4000,
        name: "PC",
        parent: None,
    },
    NewznabCategory {
        id: 4010,
        name: "PC/0day",
        parent: Some(4000),
    },
    NewznabCategory {
        id: 4020,
        name: "PC/ISO",
        parent: Some(4000),
    },
    NewznabCategory {
        id: 4030,
        name: "PC/Mac",
        parent: Some(4000),
    },
    NewznabCategory {
        id: 4040,
        name: "PC/Mobile-Other",
        parent: Some(4000),
    },
    NewznabCategory {
        id: 4050,
        name: "PC/Games",
        parent: Some(4000),
    },
    NewznabCategory {
        id: 4060,
        name: "PC/Mobile-iOS",
        parent: Some(4000),
    },
    NewznabCategory {
        id: 4070,
        name: "PC/Mobile-Android",
        parent: Some(4000),
    },
    NewznabCategory {
        id: 5000,
        name: "TV",
        parent: None,
    },
    NewznabCategory {
        id: 5010,
        name: "TV/WEB-DL",
        parent: Some(5000),
    },
    NewznabCategory {
        id: 5020,
        name: "TV/Foreign",
        parent: Some(5000),
    },
    NewznabCategory {
        id: 5030,
        name: "TV/SD",
        parent: Some(5000),
    },
    NewznabCategory {
        id: 5040,
        name: "TV/HD",
        parent: Some(5000),
    },
    NewznabCategory {
        id: 5045,
        name: "TV/UHD",
        parent: Some(5000),
    },
    NewznabCategory {
        id: 5050,
        name: "TV/Other",
        parent: Some(5000),
    },
    NewznabCategory {
        id: 5060,
        name: "TV/Sport",
        parent: Some(5000),
    },
    NewznabCategory {
        id: 5070,
        name: "TV/Anime",
        parent: Some(5000),
    },
    NewznabCategory {
        id: 5080,
        name: "TV/Documentary",
        parent: Some(5000),
    },
    NewznabCategory {
        id: 6000,
        name: "XXX",
        parent: None,
    },
    NewznabCategory {
        id: 6010,
        name: "XXX/DVD",
        parent: Some(6000),
    },
    NewznabCategory {
        id: 6020,
        name: "XXX/WMV",
        parent: Some(6000),
    },
    NewznabCategory {
        id: 6030,
        name: "XXX/XviD",
        parent: Some(6000),
    },
    NewznabCategory {
        id: 6040,
        name: "XXX/x264",
        parent: Some(6000),
    },
    NewznabCategory {
        id: 6045,
        name: "XXX/UHD",
        parent: Some(6000),
    },
    NewznabCategory {
        id: 6050,
        name: "XXX/Pack",
        parent: Some(6000),
    },
    NewznabCategory {
        id: 6060,
        name: "XXX/ImageSet",
        parent: Some(6000),
    },
    NewznabCategory {
        id: 6070,
        name: "XXX/Other",
        parent: Some(6000),
    },
    NewznabCategory {
        id: 6080,
        name: "XXX/SD",
        parent: Some(6000),
    },
    NewznabCategory {
        id: 6090,
        name: "XXX/WEB-DL",
        parent: Some(6000),
    },
    NewznabCategory {
        id: 7000,
        name: "Books",
        parent: None,
    },
    NewznabCategory {
        id: 7010,
        name: "Books/Mags",
        parent: Some(7000),
    },
    NewznabCategory {
        id: 7020,
        name: "Books/EBook",
        parent: Some(7000),
    },
    NewznabCategory {
        id: 7030,
        name: "Books/Comics",
        parent: Some(7000),
    },
    NewznabCategory {
        id: 7040,
        name: "Books/Technical",
        parent: Some(7000),
    },
    NewznabCategory {
        id: 7050,
        name: "Books/Other",
        parent: Some(7000),
    },
    NewznabCategory {
        id: 7060,
        name: "Books/Foreign",
        parent: Some(7000),
    },
    NewznabCategory {
        id: 8000,
        name: "Other",
        parent: None,
    },
    NewznabCategory {
        id: 8010,
        name: "Other/Misc",
        parent: Some(8000),
    },
    NewznabCategory {
        id: 8020,
        name: "Other/Hashed",
        parent: Some(8000),
    },
];

/// Every standard category and subcategory, ordered by id.
#[must_use]
pub fn all() -> &'static [NewznabCategory] {
    CATEGORIES
}

/// Looks up a category by its Newznab id.
#[must_use]
pub fn by_id(id: u32) -> Option<&'static NewznabCategory> {
    CATEGORIES
        .binary_search_by_key(&id, |category| category.id)
        .ok()
        .map(|index| &CATEGORIES[index])
}

/// A subcategory's top-level parent id (a parent maps to itself).
#[must_use]
pub fn parent_of(id: u32) -> Option<u32> {
    by_id(id).map(|category| category.parent.unwrap_or(category.id))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn by_id_finds_a_top_level_category_with_no_parent() {
        let tv = by_id(5000).unwrap();
        assert_eq!(tv.name, "TV");
        assert_eq!(tv.parent, None);
    }

    #[test]
    fn by_id_finds_a_subcategory_with_its_parent() {
        let anime = by_id(5070).unwrap();
        assert_eq!(anime.parent, Some(5000));
    }

    #[test]
    fn parent_of_a_subcategory_is_its_top_level_id() {
        assert_eq!(parent_of(2040), Some(2000));
    }

    #[test]
    fn parent_of_a_top_level_category_is_itself() {
        assert_eq!(parent_of(2000), Some(2000));
    }

    #[test]
    fn by_id_of_an_unknown_id_is_none() {
        assert_eq!(by_id(9999), None);
    }

    #[test]
    fn parent_of_an_unknown_id_is_none() {
        assert_eq!(parent_of(9999), None);
    }

    #[test]
    fn all_is_sorted_ascending_by_id() {
        let ids: Vec<u32> = all().iter().map(|c| c.id).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted);
    }

    #[test]
    fn all_is_non_empty() {
        assert!(!all().is_empty());
    }

    #[test]
    fn all_has_a_known_member_of_every_top_level_block() {
        let known_ids = [1010, 2040, 3010, 4050, 5070, 6010, 7020, 8010];
        for id in known_ids {
            assert!(by_id(id).is_some(), "expected id {id} to be present");
        }
    }
}
