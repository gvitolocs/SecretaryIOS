//! Port of `api/components/Tag.java` and `TagList.java`.

use crate::enums::{TagStatus, TagType};
use crate::util::unescape_unicode_string;
use serde::{Deserialize, Serialize};

/// `Tag` — Java `equals`/`hashCode` use ONLY `id`, mirrored here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tag {
    pub name: String,
    pub count: i32,
    pub id: i32,
    pub type_: TagType,
    pub status: TagStatus,
}

impl PartialEq for Tag {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for Tag {}
impl std::hash::Hash for Tag {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl Tag {
    /// `new Tag(String text)` — CSV `count,id,typeId,name` (scraped format).
    /// Java throws on malformed input; the port returns `None` instead.
    pub fn from_scraped(text: &str) -> Option<Tag> {
        let mut parts = text.splitn(4, ',');
        let count: i32 = parts.next()?.trim().parse().ok()?;
        let id: i32 = parts.next()?.trim().parse().ok()?;
        let type_id: i32 = parts.next()?.trim().parse().ok()?;
        if !(0..=7).contains(&type_id) {
            return None;
        }
        let name = parts.next()?.to_string();
        Some(Tag {
            name,
            count,
            id,
            type_: TagType::from_id(type_id as u8),
            status: TagStatus::Default,
        })
    }

    pub fn new(
        name: impl Into<String>,
        count: i32,
        id: i32,
        type_: TagType,
        status: TagStatus,
    ) -> Tag {
        Tag {
            name: name.into(),
            count,
            id,
            type_,
            status,
        }
    }

    /// `toQueryTag(TagStatus)` — `-` prefix for AVOIDED, then
    /// `<type>:"<name>"` (name NOT escaped, matching Java).
    pub fn to_query_tag_with(&self, status: TagStatus) -> String {
        let mut s = String::new();
        if status == TagStatus::Avoided {
            s.push('-');
        }
        s.push_str(self.type_.single());
        s.push(':');
        s.push('"');
        s.push_str(&self.name);
        s.push('"');
        s
    }

    /// `toQueryTag()` — uses the tag's own status.
    pub fn to_query_tag(&self) -> String {
        self.to_query_tag_with(self.status)
    }

    /// `toScrapedString` — `String.format("%d,%d,%d,%s", count, id, typeId, name)`.
    pub fn to_scraped_string(&self) -> String {
        format!("{},{},{},{}", self.count, self.id, self.type_.id(), self.name)
    }

    /// Parse a `Tag` JSON object (the Android `JsonReader` branch):
    /// recognized keys `count`, `type`, `id`, `name`; `slug` and unknown keys
    /// are skipped. Unrecognized `type` names fall back to UNKNOWN.
    pub fn from_json(v: &serde_json::Value) -> Option<Tag> {
        let obj = v.as_object()?;
        let mut name = String::new();
        let mut count = 0;
        let mut id = 0;
        let mut type_ = TagType::Unknown;
        for (k, val) in obj {
            match k.as_str() {
                "count" => count = val.as_i64()? as i32,
                "type" => type_ = TagType::type_by_name(val.as_str()?),
                "id" => id = val.as_i64()? as i32,
                "name" => name = val.as_str()?.to_string(),
                _ => {} // slug + unknown skipped
            }
        }
        Some(Tag {
            name,
            count,
            id,
            type_,
            status: TagStatus::Default,
        })
    }
}

/// Port of `TagList` — one bucket per `TagType`, indexed by `type.getId()`.
/// Java `sort` uses `(o1, o2) -> o2.getCount() - o1.getCount()` (TimSort,
/// stable); Rust `sort_by` with reversed count compare is equivalent-stable.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TagList {
    buckets: Vec<Vec<Tag>>, // indexed by TagType::id()
}

impl TagList {
    pub fn new() -> TagList {
        TagList {
            buckets: vec![Vec::new(); 8],
        }
    }

    pub fn add_tag(&mut self, tag: Tag) {
        self.buckets[tag.type_.id() as usize].push(tag);
    }

    pub fn add_tags(&mut self, tags: impl IntoIterator<Item = Tag>) {
        for t in tags {
            self.add_tag(t);
        }
    }

    /// `retrieveForType`
    pub fn retrieve_for_type(&self, type_: TagType) -> &[Tag] {
        &self.buckets[type_.id() as usize]
    }

    /// `getAllTagsList` — bucket order (by type id) preserved.
    pub fn all_tags_list(&self) -> Vec<&Tag> {
        self.buckets.iter().flatten().collect()
    }

    /// `getAllTagsSet` — dedup by id (Java HashSet<Tag> semantics).
    pub fn all_tags_set(&self) -> Vec<&Tag> {
        let mut seen = std::collections::HashSet::new();
        self.buckets
            .iter()
            .flatten()
            .filter(|t| seen.insert(t.id))
            .collect()
    }

    pub fn get_count(&self, type_: TagType) -> usize {
        self.buckets[type_.id() as usize].len()
    }

    pub fn get_tag(&self, type_: TagType, index: usize) -> Option<&Tag> {
        self.buckets[type_.id() as usize].get(index)
    }

    pub fn sort_by_count_desc(&mut self) {
        for b in &mut self.buckets {
            b.sort_by(|a, b| b.count.cmp(&a.count));
        }
    }

    pub fn has_tag(&self, tag: &Tag) -> bool {
        self.buckets[tag.type_.id() as usize].iter().any(|t| *t == *tag)
    }

    /// `hasTags` — all of `tags` must be present.
    pub fn has_tags<'a>(&self, tags: impl IntoIterator<Item = &'a Tag>) -> bool {
        tags.into_iter().all(|t| self.has_tag(t))
    }
}

/// Helper used by `GalleryData::parse_json` title handling: Java stores titles
/// through `Utility.unescapeUnicodeString`, with JSON null mapped to `""`.
pub(crate) fn json_title(raw: Option<&str>) -> String {
    unescape_unicode_string(raw.unwrap_or(""))
}
