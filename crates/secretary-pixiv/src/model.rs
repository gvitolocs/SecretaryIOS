//! The service's own shapes. Everything here still carries the upstream (i.pximg.net) URLs; they
//! are turned into signed `/v1/pixiv/s/...` URLs only when a response is built (`http.rs`), so
//! the JSON cache and the bookmark file never hold a signature that could expire.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Summary {
    pub id: String,
    pub title: String,
    pub author: String,
    pub author_id: String,
    pub tags: Vec<String>,
    /// Upstream thumbnail URL.
    pub thumb: String,
    pub pages: u32,
    pub r18: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Tag {
    pub tag: String,
    pub en: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IllustPage {
    pub thumb: String,
    pub regular: String,
    pub orig: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Stats {
    pub views: u64,
    pub likes: u64,
    pub bookmarks: u64,
    pub comments: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Illust {
    pub id: String,
    pub title: String,
    pub author: String,
    pub author_id: String,
    pub created: String,
    /// 0 illustration, 1 manga, 2 ugoira (shown as its cover).
    pub kind: i64,
    pub r18: bool,
    pub description: String,
    pub stats: Stats,
    pub page_count: u32,
    pub cover: String,
    pub tags: Vec<Tag>,
    pub pages: Vec<IllustPage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Listing {
    pub items: Vec<Summary>,
    pub page: u32,
    pub has_more: bool,
    pub total: Option<u64>,
    pub num_pages: Option<u64>,
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn u(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(|x| x.as_u64()).unwrap_or(0)
}

fn summary_of(c: &Value, r18_default: bool) -> Option<Summary> {
    let id = s(c, "id");
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(Summary {
        id,
        title: s(c, "title"),
        author: s(c, "author"),
        author_id: s(c, "author_id"),
        tags: c
            .get("tags")
            .and_then(|t| t.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|t| t.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        thumb: s(c, "thumb"),
        pages: c.get("pages").and_then(|p| p.as_u64()).unwrap_or(1).clamp(1, 10_000) as u32,
        r18: c.get("r18").and_then(|x| x.as_bool()).unwrap_or(r18_default),
    })
}

/// The core's ranking answer (`{mode, page, items, next}`). `r18_mode` marks every item R-18
/// (the core's ranking rows carry no age flag; the R-18 modes are the only source of such rows).
pub fn parse_ranking(v: &Value, page: u32, r18_mode: bool) -> Listing {
    let items = v
        .get("items")
        .and_then(|i| i.as_array())
        .map(|a| a.iter().filter_map(|c| summary_of(c, r18_mode)).collect())
        .unwrap_or_default();
    Listing {
        items,
        page,
        has_more: v.get("next").and_then(|n| n.as_bool()).unwrap_or(false),
        total: None,
        num_pages: None,
    }
}

/// The core's search answer (`{word, page, total, num_pages, items}`).
pub fn parse_search(v: &Value, page: u32) -> Listing {
    let items: Vec<Summary> = v
        .get("items")
        .and_then(|i| i.as_array())
        .map(|a| a.iter().filter_map(|c| summary_of(c, false)).collect())
        .unwrap_or_default();
    let num_pages = v.get("num_pages").and_then(|n| n.as_u64());
    Listing {
        has_more: num_pages.is_some_and(|n| (page as u64) < n) && !items.is_empty(),
        items,
        page,
        total: v.get("total").and_then(|n| n.as_u64()),
        num_pages,
    }
}

pub fn empty_listing(page: u32) -> Listing {
    Listing {
        items: vec![],
        page,
        has_more: false,
        total: Some(0),
        num_pages: Some(0),
    }
}

pub fn parse_illust(v: &Value) -> Option<Illust> {
    let id = s(v, "id");
    if id.is_empty() {
        return None;
    }
    let stats = v.get("stats").cloned().unwrap_or(Value::Null);
    Some(Illust {
        id,
        title: s(v, "title"),
        author: s(v, "author"),
        author_id: s(v, "author_id"),
        created: s(v, "created"),
        kind: v.get("type").and_then(|t| t.as_i64()).unwrap_or(0),
        r18: v.get("r18").and_then(|x| x.as_bool()).unwrap_or(false),
        description: s(v, "description"),
        stats: Stats {
            views: u(&stats, "views"),
            likes: u(&stats, "likes"),
            bookmarks: u(&stats, "bookmarks"),
            comments: u(&stats, "comments"),
        },
        page_count: v.get("page_count").and_then(|p| p.as_u64()).unwrap_or(1).clamp(1, 10_000)
            as u32,
        cover: s(v, "cover"),
        tags: v
            .get("tags")
            .and_then(|t| t.as_array())
            .map(|a| {
                a.iter()
                    .map(|t| Tag {
                        tag: s(t, "tag"),
                        en: s(t, "en"),
                    })
                    .filter(|t| !t.tag.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
        pages: v
            .get("pages")
            .and_then(|p| p.as_array())
            .map(|a| {
                a.iter()
                    .map(|p| IllustPage {
                        thumb: s(p, "thumb"),
                        regular: s(p, "regular"),
                        orig: s(p, "orig"),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    })
}

impl Illust {
    pub fn summary(&self) -> Summary {
        Summary {
            id: self.id.clone(),
            title: self.title.clone(),
            author: self.author.clone(),
            author_id: self.author_id.clone(),
            tags: self.tags.iter().map(|t| t.tag.clone()).collect(),
            thumb: self
                .pages
                .first()
                .map(|p| p.thumb.clone())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| self.cover.clone()),
            pages: self.page_count,
            r18: self.r18,
        }
    }
}
