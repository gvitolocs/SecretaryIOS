//! Port of `api/components/Gallery.java` (portable parts).

use crate::enums::{Language, SpecialTagIds, TagType, TitleType};
use crate::gallery_data::{calculate_sizes, GalleryData};
use crate::page::Size;
use crate::simple_gallery::SimpleGallery;
use crate::tag::TagList;

/// `Gallery.loadLanguage(TagList)` — first LANGUAGE tag whose id matches a
/// special language id wins (iteration order = insertion order per type
/// bucket); UNKNOWN when none match.
pub fn load_language(tags: &TagList) -> Language {
    for tag in tags.retrieve_for_type(TagType::Language) {
        match tag.id {
            SpecialTagIds::LANGUAGE_JAPANESE => return Language::Japanese,
            SpecialTagIds::LANGUAGE_ENGLISH => return Language::English,
            SpecialTagIds::LANGUAGE_CHINESE => return Language::Chinese,
            _ => {}
        }
    }
    Language::Unknown
}

/// `Gallery.getPathTitle(String, String)` — verbatim:
/// `'/' → ' '`, then regex `[/|\\*"\'?:<>]` each → `' '`, collapse runs of
/// spaces, `trim()`.
pub fn path_title_with_default(title: Option<&str>, default: &str) -> String {
    let Some(title) = title else {
        return default.to_string();
    };
    let mut s = title.replace('/', " ");
    // Java replaceAll("[/|\\\\*\"'?:<>]", " ") — single-char class members.
    // The `/` member is unreachable (already replaced) but harmless.
    s = s
        .chars()
        .map(|c| match c {
            '|' | '\\' | '*' | '"' | '\'' | '?' | ':' | '<' | '>' => ' ',
            other => other,
        })
        .collect();
    while s.contains("  ") {
        s = s.replace("  ", " ");
    }
    s.trim().to_string()
}

/// `Gallery.getPathTitle(String)`
pub fn path_title(title: Option<&str>) -> String {
    path_title_with_default(title, "")
}

/// `Gallery` — the complete gallery object.
#[derive(Debug, Clone)]
pub struct Gallery {
    pub data: GalleryData,
    pub related: Vec<SimpleGallery>,
    pub language: Language,
    pub online_favorite: bool,
    pub max_size: Size,
    pub min_size: Size,
}

impl Gallery {
    /// `new Gallery(context, json, relatedList, isFavorite)` — parse the v2
    /// detail JSON and derive language + sizes (Java `calculateSizes`).
    pub fn parse(
        json: &serde_json::Value,
        related: Vec<SimpleGallery>,
        is_favorite: bool,
        host: &str,
    ) -> Option<Gallery> {
        let data = GalleryData::parse_json(json, host)?;
        let language = load_language(&data.tags);
        let (max_size, min_size) = calculate_sizes(&data.pages);
        Some(Gallery {
            data,
            related,
            language,
            online_favorite: is_favorite,
            max_size,
            min_size,
        })
    }

    /// `Gallery.emptyGallery()`
    pub fn empty() -> Gallery {
        Gallery {
            data: GalleryData::fake_data(),
            related: Vec::new(),
            language: Language::Unknown,
            online_favorite: false,
            max_size: Size::new(0, 0),
            min_size: Size::new(i32::MAX, i32::MAX),
        }
    }

    /// `Gallery.getTitle()` — the fallback chain: current title type
    /// (caller supplies the app preference), then PRETTY, ENGLISH, JAPANESE,
    /// requiring `length() > 2`, else "Unnamed".
    pub fn get_title_with_pref(&self, pref: TitleType) -> String {
        // Java `x.length() > 2` counts UTF-16 code units, not scalar chars.
        let long_enough = |s: &str| s.encode_utf16().count() > 2;
        let mut x = self.data.get_title(pref);
        if long_enough(x) {
            return x.to_string();
        }
        x = self.data.get_title(TitleType::Pretty);
        if long_enough(x) {
            return x.to_string();
        }
        x = self.data.get_title(TitleType::English);
        if long_enough(x) {
            return x.to_string();
        }
        let x = self.data.get_title(TitleType::Japanese);
        if long_enough(x) {
            return x.to_string();
        }
        "Unnamed".to_string()
    }

    /// `getPathTitle()` for this gallery.
    pub fn get_path_title(&self, pref: TitleType) -> String {
        path_title(Some(&self.get_title_with_pref(pref)))
    }

    /// `isOnlineFavorite`
    pub fn is_online_favorite(&self) -> bool {
        self.online_favorite
    }

    /// `isValid` — delegates to `GalleryData.isValid`.
    pub fn is_valid(&self) -> bool {
        self.data.is_valid()
    }

    /// `getId`
    pub fn id(&self) -> i32 {
        self.data.id
    }

    /// `getPageCount`
    pub fn page_count(&self) -> i32 {
        self.data.page_count
    }

    /// `getMediaId`
    pub fn media_id(&self) -> i32 {
        self.data.media_id
    }

    /// `hasIgnoredTags(Set<Tag>)` — true if any of the gallery's tags is in
    /// the set (id equality, like Java `HashSet<Tag>`).
    pub fn has_ignored_tags(&self, ignored: &[crate::tag::Tag]) -> bool {
        self.data
            .tags
            .all_tags_set()
            .iter()
            .any(|t| ignored.iter().any(|i| i.id == t.id))
    }
}
