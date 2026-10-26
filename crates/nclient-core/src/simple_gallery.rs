//! Port of `api/SimpleGallery.java` (`fromV2ListItem`) and
//! `api/components/GenericGallery.java` helpers.

use crate::enums::Language;
use crate::tag::TagList;

/// Stand-in for `Queries.TagTable.getTagsFromListOfInt` — the Java code looks
/// tag ids up in its local SQLite DB. The Rust core takes the provider as a
/// dependency so callers can back it with any store.
pub trait TagProvider {
    /// Resolve a comma-separated list of tag ids into a `TagList`
    /// (Java: `getTagsFromListOfInt`).
    fn tags_from_id_list(&self, ids: &str) -> TagList;
    /// Java also inserts newly seen tags into the DB (`Queries.TagTable.insert`);
    /// providers that persist should do so here.
    fn insert_tag(&self, _tag: &crate::tag::Tag) {}
}

/// No-op provider for galleries without tag data.
pub struct EmptyTagProvider;
impl TagProvider for EmptyTagProvider {
    fn tags_from_id_list(&self, _ids: &str) -> TagList {
        TagList::new()
    }
}

/// `SimpleGallery`
#[derive(Debug, Clone)]
pub struct SimpleGallery {
    pub title: String,
    pub id: i32,
    pub media_id: i32,
    /// Java: `Uri.parse("https://t1.<host>/" + thumbPath)`
    pub thumbnail: String,
    pub language: Language,
    /// Java never parcels tags; kept for `hasTags`/`hasIgnoredTags`.
    pub tags: TagList,
}

impl SimpleGallery {
    /// `SimpleGallery.fromV2ListItem` — v2 list items carry
    /// `id, media_id (string), thumbnail (path), english_title,
    /// japanese_title, tag_ids (int array), num_pages`; detail/related items
    /// may instead carry a `title` object and full `tags` array.
    pub fn from_v2_list_item(
        json: &serde_json::Value,
        host: &str,
        provider: &dyn TagProvider,
    ) -> Option<SimpleGallery> {
        let obj = json.as_object()?;
        let id = obj.get("id")?.as_i64()? as i32;
        let media_id = match obj.get("media_id").and_then(|v| v.as_str()) {
            Some(s) => s.parse::<i32>().unwrap_or(0), // Java: NumberFormatException → 0
            None => obj.get("media_id").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
        };
        // Title: v2 list items use english_title/japanese_title directly;
        // detail/related items may use a title object.
        let title = match obj.get("title").and_then(|v| v.as_object()) {
            Some(t) => {
                let pretty = str_field(t, "pretty");
                let english = str_field(t, "english");
                let japanese = str_field(t, "japanese");
                if !pretty.is_empty() {
                    pretty
                } else if !english.is_empty() {
                    english
                } else {
                    japanese
                }
            }
            None => {
                let english = str_field(obj, "english_title");
                let japanese = str_field(obj, "japanese_title");
                if !english.is_empty() {
                    english
                } else {
                    japanese
                }
            }
        };
        let thumb_path = str_field(obj, "thumbnail");
        let mut language = Language::Unknown;
        let tags = if let Some(arr) = obj.get("tag_ids").and_then(|v| v.as_array()) {
            if arr.is_empty() {
                TagList::new()
            } else {
                let ids: Vec<String> = arr
                    .iter()
                    .filter_map(|v| v.as_i64().map(|i| i.to_string()))
                    .collect();
                let list = provider.tags_from_id_list(&ids.join(","));
                language = crate::gallery::load_language(&list);
                list
            }
        } else if let Some(arr) = obj.get("tags").and_then(|v| v.as_array()) {
            if arr.is_empty() {
                TagList::new()
            } else {
                let mut ids = Vec::with_capacity(arr.len());
                for tag_obj in arr {
                    let tag = crate::tag::Tag::from_json(tag_obj)?;
                    provider.insert_tag(&tag);
                    ids.push(tag.id.to_string());
                }
                let list = provider.tags_from_id_list(&ids.join(","));
                language = crate::gallery::load_language(&list);
                list
            }
        } else {
            TagList::new()
        };
        // Java: if (context != null && id > Global.getMaxId()) updateMaxId —
        // app-state side effect; portable core leaves it to the caller.
        Some(SimpleGallery {
            title,
            id,
            media_id,
            thumbnail: format!("https://t1.{host}/{thumb_path}"),
            language,
            tags,
        })
    }

    /// `hasTags`
    pub fn has_tags<'a>(&self, tags: impl IntoIterator<Item = &'a crate::tag::Tag>) -> bool {
        self.tags.has_tags(tags)
    }

    /// `hasIgnoredTags(String)` — substring check of the AVOIDED query form
    /// of each tag against `s` (Java uses `String.contains`).
    pub fn has_ignored_tags(&self, s: &str) -> bool {
        self.tags
            .all_tags_list()
            .iter()
            .any(|t| s.contains(&t.to_query_tag_with(crate::enums::TagStatus::Avoided)))
    }

    /// `isValid` — Java: `id > 0`.
    pub fn is_valid(&self) -> bool {
        self.id > 0
    }

    /// `GenericGallery.sharePageUrl(i)`
    pub fn share_page_url(&self, index: i32, host: &str) -> String {
        format!("https://{host}/g/{}/{}/", self.id, index + 1)
    }
}

fn str_field(obj: &serde_json::Map<String, serde_json::Value>, key: &str) -> String {
    obj.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}
