//! Port of `api/components/GalleryData.java` (JSON constructor path).
//!
//! The Android class mixes DB access, network refresh and background threads
//! into the same object; the port keeps the portable JSON-parsing core and
//! represents the "old favorites entry needs a network refresh" state as a
//! value instead of spawning a thread.

use crate::enums::{ImageType, SpecialTagIds, TitleType};
use crate::page::{Page, Size};
use crate::tag::{json_title, Tag, TagList};
use serde::{Deserialize, Serialize};

/// `GalleryData`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GalleryData {
    /// Java stores `new Date(seconds * 1000)`; the port keeps epoch millis.
    pub upload_date_millis: i64,
    pub favorite_count: i32,
    pub id: i32,
    pub page_count: i32,
    pub media_id: i32,
    /// indexed by `TitleType` ordinal: [Japanese, Pretty, English]
    pub titles: [String; 3],
    pub tags: TagList,
    pub cover: Page,
    pub thumbnail: Page,
    pub pages: Vec<Page>,
    pub valid: bool,
}

impl Default for GalleryData {
    fn default() -> Self {
        GalleryData {
            upload_date_millis: 0,
            favorite_count: 0,
            id: 0,
            page_count: 0,
            media_id: 0,
            titles: Default::default(),
            tags: TagList::new(),
            cover: Page::default(),
            thumbnail: Page::default(),
            pages: Vec::new(),
            valid: true,
        }
    }
}

/// Result of parsing a stored favorites page-path entry
/// (`GalleryData.readPagePath`): the new format decodes locally; the old
/// format required the Java code to hit `api/v2/galleries/<id>` in a thread.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PagePathState {
    /// parsed from the new `readPagePathNew` format
    Loaded,
    /// old entry — caller must refresh via `api/v2/galleries/<id>`; a 404
    /// marks the gallery deleted (Java: `isDeleted = true`)
    NeedsRefresh { id: i32 },
}

impl GalleryData {
    /// `GalleryData.fakeData()`
    pub fn fake_data() -> GalleryData {
        GalleryData {
            id: SpecialTagIds::INVALID_ID,
            favorite_count: -1,
            page_count: -1,
            media_id: SpecialTagIds::INVALID_ID,
            valid: false,
            ..Default::default()
        }
    }

    /// `GalleryData(JsonReader)` — the v2 gallery-detail JSON constructor.
    /// Key handling mirrors the Java switch exactly:
    /// `upload_date` (s→ms), `num_favorites`, `num_pages`, `media_id`
    /// (string OR number), `id`, `cover`, `thumbnail`, `pages` (index
    /// assigned in array order), `title` (japanese/english/pretty; JSON
    /// null → ""), `tags` (parsed, then sorted by count desc), `error` →
    /// `valid = false`, unknown keys skipped.
    pub fn parse_json(v: &serde_json::Value, host: &str) -> Option<GalleryData> {
        let obj = v.as_object()?;
        let mut d = GalleryData::default();
        for (k, val) in obj {
            match k.as_str() {
                "upload_date" => d.upload_date_millis = val.as_i64()? * 1000,
                "num_favorites" => d.favorite_count = val.as_i64()? as i32,
                "num_pages" => d.page_count = val.as_i64()? as i32,
                "media_id" => {
                    // Java: Integer.parseInt when STRING, nextInt when NUMBER
                    let parsed = match val {
                        serde_json::Value::String(s) => s.parse::<i32>().ok()?,
                        other => other.as_i64()? as i32,
                    };
                    d.media_id = parsed;
                }
                "id" => d.id = val.as_i64()? as i32,
                "cover" => d.cover = Page::from_json(val, ImageType::Cover, 0, host),
                "thumbnail" => d.thumbnail = Page::from_json(val, ImageType::Thumbnail, 0, host),
                "pages" => {
                    let arr = val.as_array()?;
                    for (i, p) in arr.iter().enumerate() {
                        d.pages
                            .push(Page::from_json(p, ImageType::Page, i as i32, host));
                    }
                }
                "title" => {
                    let t = val.as_object()?;
                    for (tk, tv) in t {
                        match tk.as_str() {
                            "japanese" => {
                                d.titles[TitleType::Japanese as usize] =
                                    json_title(tv.as_str())
                            }
                            "english" => {
                                d.titles[TitleType::English as usize] = json_title(tv.as_str())
                            }
                            "pretty" => {
                                d.titles[TitleType::Pretty as usize] = json_title(tv.as_str())
                            }
                            _ => {}
                        }
                    }
                }
                "tags" => {
                    let arr = val.as_array()?;
                    for t in arr {
                        if let Some(tag) = Tag::from_json(t) {
                            d.tags.add_tag(tag);
                        }
                    }
                    d.tags.sort_by_count_desc();
                }
                "error" => d.valid = false,
                _ => {}
            }
        }
        Some(d)
    }

    /// `getTitle(TitleType)`
    pub fn get_title(&self, t: TitleType) -> &str {
        &self.titles[t as usize]
    }

    /// `getPage(index)` — Java `ArrayList.get` throws; the port returns Option.
    pub fn get_page(&self, index: usize) -> Option<&Page> {
        self.pages.get(index)
    }

    /// `isValid`
    pub fn is_valid(&self) -> bool {
        self.valid
    }

    /// `createPagePath` — `<pages>;<coverSuffix>;<thumbSuffix>;<pageSuffix>;...`
    /// where each suffix is the substring from the last `/` of the URL.
    /// Java `substring(lastIndexOf('/'))` includes the slash itself.
    pub fn create_page_path(&self) -> String {
        let truncate = |uri: &str| -> String {
            match uri.rfind('/') {
                Some(i) => uri[i..].to_string(),
                None => uri.to_string(), // Java would throw; tolerate instead
            }
        };
        let mut out = String::new();
        out.push_str(&self.pages.len().to_string());
        out.push(';');
        out.push_str(&truncate(self.cover.thumbnail_path()));
        out.push(';');
        out.push_str(&truncate(self.thumbnail.thumbnail_path()));
        out.push(';');
        if self.pages.is_empty() {
            return out;
        }
        for p in &self.pages {
            out.push_str(&truncate(&p.path));
            out.push(';');
        }
        out
    }

    /// `readPagePathNew` — decode a stored favorites page-path string into
    /// `Page`s. Returns `None` for malformed input (Java throws).
    /// The Java method flags `changedInfo = true` when parts[1] starts with
    /// "http"; the port reports that via the boolean in the tuple.
    pub fn parse_page_path(
        path: &str,
        host: &str,
        media_id: i32,
        id: i32,
    ) -> Option<(GalleryData, PagePathState)> {
        if !(path.contains(';') && path.contains('/')) {
            // old format — Java refreshes over the network
            return Some((GalleryData::default(), PagePathState::NeedsRefresh { id }));
        }
        // Java String.split(";") drops ALL trailing empty strings; mirror it.
        let mut parts: Vec<&str> = path.split(';').collect();
        while parts.last() == Some(&"") {
            parts.pop();
        }
        if parts.len() < 3 {
            return None;
        }
        let mut d = GalleryData::default();
        d.media_id = media_id;
        let state = PagePathState::Loaded;
        if parts[1].starts_with("http") {
            d.cover = Page::from_path(ImageType::Cover, parts[1]);
            d.thumbnail = Page::from_path(ImageType::Thumbnail, parts[2]);
            let mut absolute = 0;
            for p in &parts[3..] {
                d.pages
                    .push(Page::from_path(ImageType::Page, *p).with_page(absolute));
                absolute += 1;
            }
        } else {
            d.cover = Page::from_path(
                ImageType::Cover,
                crate::page::gallery_host_url("t1", host, media_id, parts[1]),
            );
            d.thumbnail = Page::from_path(
                ImageType::Thumbnail,
                crate::page::gallery_host_url("t1", host, media_id, parts[2]),
            );
            let mut absolute = 0;
            for p in &parts[3..] {
                d.pages.push(
                    Page::from_path(
                        ImageType::Page,
                        crate::page::gallery_host_url("i1", host, media_id, p),
                    )
                    .with_page(absolute),
                );
                absolute += 1;
            }
        }
        // Java's readPagePathNew does not set pageCount (stays 0 / set via
        // setPageInfo); keep parity by not touching it here.
        let _ = &mut d.page_count;
        Some((d, state))
    }
}

impl Page {
    fn with_page(mut self, page: i32) -> Page {
        self.page = page;
        self
    }
}

/// `Gallery.calculateSizes` — min/max width/height over all pages, seeded
/// exactly like Java (`max = 0,0`, `min = MAX_VALUE,MAX_VALUE`).
pub fn calculate_sizes(pages: &[Page]) -> (Size, Size) {
    let mut max = Size::new(0, 0);
    let mut min = Size::new(i32::MAX, i32::MAX);
    for p in pages {
        let s = p.size;
        if s.width > max.width {
            max.width = s.width;
        }
        if s.height > max.height {
            max.height = s.height;
        }
        if s.width < min.width {
            min.width = s.width;
        }
        if s.height < min.height {
            min.height = s.height;
        }
    }
    (max, min)
}

/// `Gallery.jsonWrite` — serialization used for local gallery metadata.
/// Java iterates a `HashSet<Tag>` (order unspecified); the port uses the
/// deterministic `all_tags_set()` order (first occurrence by id).
/// Title keys are written japanese → pretty → english, only when non-null.
pub fn gallery_json_write(d: &GalleryData, language: crate::enums::Language) -> String {
    use serde_json::json;
    let mut title_obj = serde_json::Map::new();
    // Titles were already unescaped during parse; write them raw like Java.
    title_obj.insert("japanese".into(), json!(d.titles[TitleType::Japanese as usize]));
    title_obj.insert("pretty".into(), json!(d.titles[TitleType::Pretty as usize]));
    title_obj.insert("english".into(), json!(d.titles[TitleType::English as usize]));
    let tags: Vec<_> = d
        .tags
        .all_tags_set()
        .into_iter()
        .map(|t| {
            json!({
                "count": t.count,
                "type": t.type_.single(),
                "id": t.id,
                "name": t.name,
            })
        })
        .collect();
    let _ = language;
    serde_json::to_string(&json!({
        "id": d.id,
        "media_id": d.media_id,
        "upload_date": d.upload_date_millis / 1000,
        "num_favorites": d.favorite_count,
        "title": title_obj,
        "tags": tags,
    }))
    .unwrap()
}
