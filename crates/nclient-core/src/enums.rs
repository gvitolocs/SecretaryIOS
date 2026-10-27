//! Port of `api/enums/*`.

use serde::{Deserialize, Serialize};

/// `TagType` — class with static instances in Java; ids are the `getId()` values
/// used in DB storage and `Tag.toScrapedString`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TagType {
    Unknown,
    Parody,
    Character,
    Tag,
    Artist,
    Group,
    Language,
    Category,
}

impl TagType {
    /// `TagType.values[...]` ordinal/id mapping (UNKNOWN=0 .. CATEGORY=7).
    pub fn id(self) -> u8 {
        match self {
            TagType::Unknown => 0,
            TagType::Parody => 1,
            TagType::Character => 2,
            TagType::Tag => 3,
            TagType::Artist => 4,
            TagType::Group => 5,
            TagType::Language => 6,
            TagType::Category => 7,
        }
    }

    pub fn from_id(id: u8) -> TagType {
        match id {
            1 => TagType::Parody,
            2 => TagType::Character,
            3 => TagType::Tag,
            4 => TagType::Artist,
            5 => TagType::Group,
            6 => TagType::Language,
            7 => TagType::Category,
            _ => TagType::Unknown,
        }
    }

    /// `getSingle()` — `""` for UNKNOWN, `None`-like plural fields are unused
    /// by the portable core.
    pub fn single(self) -> &'static str {
        match self {
            TagType::Unknown => "",
            TagType::Parody => "parody",
            TagType::Character => "character",
            TagType::Tag => "tag",
            TagType::Artist => "artist",
            TagType::Group => "group",
            TagType::Language => "language",
            TagType::Category => "category",
        }
    }

    /// `TagType.typeByName` — falls back to UNKNOWN, matching Java.
    pub fn type_by_name(name: &str) -> TagType {
        for t in [
            TagType::Unknown,
            TagType::Parody,
            TagType::Character,
            TagType::Tag,
            TagType::Artist,
            TagType::Group,
            TagType::Language,
            TagType::Category,
        ] {
            if t.single() == name {
                return t;
            }
        }
        TagType::Unknown
    }
}

/// `TagStatus` — Java ordinal order matters for DB storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum TagStatus {
    #[default]
    Default,
    Avoided,
    Accepted,
}

/// `TitleType` — ordinal used as index into `GalleryData.titles[3]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TitleType {
    Japanese = 0,
    Pretty = 1,
    English = 2,
}

/// `Language`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Language {
    English,
    Chinese,
    Japanese,
    All,
    #[default]
    Unknown,
}

/// `SortType` — declaration order and URL additions are parity-relevant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum SortType {
    #[default]
    RecentAllTime,
    PopularAllTime,
    PopularWeekly,
    PopularDaily,
    PopularMonth,
}

impl SortType {
    /// `getUrlAddition()` — `None` for RECENT_ALL_TIME.
    pub fn url_addition(self) -> Option<&'static str> {
        match self {
            SortType::RecentAllTime => None,
            SortType::PopularAllTime => Some("popular"),
            SortType::PopularWeekly => Some("popular-week"),
            SortType::PopularDaily => Some("popular-today"),
            SortType::PopularMonth => Some("popular-month"),
        }
    }

    /// `SortType.findFromAddition` — substring `contains` in declaration
    /// order, RECENT_ALL_TIME fallback (both for null and no match).
    pub fn find_from_addition(addition: Option<&str>) -> SortType {
        let Some(addition) = addition else {
            return SortType::RecentAllTime;
        };
        for t in [
            SortType::RecentAllTime,
            SortType::PopularAllTime,
            SortType::PopularWeekly,
            SortType::PopularDaily,
            SortType::PopularMonth,
        ] {
            if let Some(url) = t.url_addition() {
                if addition.contains(url) {
                    return t;
                }
            }
        }
        SortType::RecentAllTime
    }
}

/// `ApiRequestType` — `is_single` mirrors the Java constructor flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ApiRequestType {
    ByAll,
    ByTag,
    BySearch,
    BySingle,
    Related,
    Favorite,
    Random,
    RandomFavorite,
}

impl ApiRequestType {
    pub fn is_single(self) -> bool {
        matches!(
            self,
            ApiRequestType::BySingle | ApiRequestType::Random | ApiRequestType::RandomFavorite
        )
    }
}

/// `ImageType`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ImageType {
    #[default]
    Page,
    Cover,
    Thumbnail,
}

/// `SpecialTagIds`
pub struct SpecialTagIds;
impl SpecialTagIds {
    pub const LANGUAGE_JAPANESE: i32 = 6346;
    pub const LANGUAGE_ENGLISH: i32 = 12227;
    pub const LANGUAGE_CHINESE: i32 = 29963;
    pub const INVALID_ID: i32 = -1;
}
