//! Tag id → name/type, learned from every gallery detail the service parses (the v2 list
//! endpoints carry only `tag_ids`). Persisted as one JSON file so the cache survives restarts.
//! A list item whose ids are not all known here is resolved through its detail, or dropped.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::RwLock;

use nclientv3_core::{Tag, TagList, TagStatus, TagType};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KnownTag {
    pub id: i32,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub count: i32,
}

pub struct TagCache {
    path: Option<PathBuf>,
    tags: RwLock<HashMap<i32, KnownTag>>,
}

impl TagCache {
    pub fn open(path: Option<PathBuf>) -> TagCache {
        let tags = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice::<Vec<KnownTag>>(&b).ok())
            .map(|v| v.into_iter().map(|t| (t.id, t)).collect())
            .unwrap_or_default();
        TagCache {
            path,
            tags: RwLock::new(tags),
        }
    }

    pub fn learn<'a>(&self, tags: impl IntoIterator<Item = &'a Tag>) {
        let mut changed = false;
        {
            let mut map = self.tags.write().unwrap();
            for t in tags {
                if t.id <= 0 || t.name.is_empty() {
                    continue;
                }
                let k = KnownTag {
                    id: t.id,
                    name: t.name.clone(),
                    kind: t.type_.single().to_string(),
                    count: t.count,
                };
                if map.get(&t.id) != Some(&k) {
                    map.insert(t.id, k);
                    changed = true;
                }
            }
        }
        if changed {
            self.save();
        }
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        let list: Vec<KnownTag> = self.tags.read().unwrap().values().cloned().collect();
        if let Ok(bytes) = serde_json::to_vec(&list) {
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, bytes).is_ok() {
                let _ = std::fs::rename(&tmp, path);
            }
        }
    }

    pub fn get(&self, id: i32) -> Option<KnownTag> {
        self.tags.read().unwrap().get(&id).cloned()
    }

    /// All ids resolved, or `None`.
    pub fn resolve(&self, ids: &[i32]) -> Option<Vec<KnownTag>> {
        let map = self.tags.read().unwrap();
        ids.iter().map(|id| map.get(id).cloned()).collect()
    }

    /// Known tags, most used first, optionally of one type and matching a prefix.
    pub fn list(&self, kind: Option<&str>, prefix: &str, limit: usize) -> Vec<KnownTag> {
        let prefix = crate::safety::normalize(prefix);
        let mut out: Vec<KnownTag> = self
            .tags
            .read()
            .unwrap()
            .values()
            .filter(|t| kind.is_none_or(|k| t.kind == k))
            .filter(|t| prefix.is_empty() || crate::safety::normalize(&t.name).contains(&prefix))
            .filter(|t| !crate::safety::tag_hidden(&t.name))
            .cloned()
            .collect();
        out.sort_by(|a, b| b.count.cmp(&a.count).then(a.name.cmp(&b.name)));
        out.truncate(limit);
        out
    }
}

impl KnownTag {
    pub fn to_core(&self, status: TagStatus) -> Tag {
        Tag::new(
            self.name.clone(),
            self.count,
            self.id,
            TagType::type_by_name(&self.kind),
            status,
        )
    }
}

/// `TagProvider` over the cache, for the core's list parser.
impl nclientv3_core::simple_gallery::TagProvider for TagCache {
    fn tags_from_id_list(&self, ids: &str) -> TagList {
        let mut list = TagList::new();
        for id in ids.split(',').filter_map(|s| s.trim().parse::<i32>().ok()) {
            if let Some(t) = self.get(id) {
                list.add_tag(t.to_core(TagStatus::Default));
            }
        }
        list
    }

    fn insert_tag(&self, tag: &Tag) {
        self.learn([tag]);
    }
}
