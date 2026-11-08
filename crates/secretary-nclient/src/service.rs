//! Search, gallery detail, random, related, tags and images, each through the safety filter.
//!
//! The order is always: fetch → parse (nclient-core) → resolve every tag → `safety::vet` →
//! only then cache or return. Anything not `Allowed` is dropped and never stored.

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Duration;

use nclientv3_core::inspector::{
    create_url, detail_url, parse_single_response, SingleParseOutcome,
};
use nclientv3_core::{
    ApiRequestType, Language, SimpleGallery, SortType, Tag, TagStatus, TagType, TitleType,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cache::DiskCache;
use crate::safety::{self, Verdict};
use crate::tags::{KnownTag, TagCache};
use crate::upstream::{Upstream, UpstreamError};

const SEARCH_TTL: Duration = Duration::from_secs(10 * 60);
const DETAIL_TTL: Duration = Duration::from_secs(24 * 3600);
const BLOCKED_TTL: Duration = Duration::from_secs(30 * 24 * 3600);
/// Detail fetches allowed per list page to name unknown tag ids (1 s each).
const RESOLVE_BUDGET: usize = 10;
const RANDOM_TRIES: usize = 5;
const SEEN_MAX: usize = 5000;

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("not found")]
    NotFound,
    #[error("bad request: {0}")]
    BadRequest(&'static str),
    #[error(transparent)]
    Upstream(#[from] UpstreamError),
    #[error("upstream answer not understood")]
    Parse,
}

/// A vetted list entry. `thumb_src` is the upstream URL: server side only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    pub id: i32,
    pub title: String,
    pub pages: i32,
    pub language: String,
    pub tags: Vec<KnownTag>,
    pub thumb_src: String,
    #[serde(default)]
    pub alt_titles: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageSrc {
    pub width: i32,
    pub height: i32,
    pub src: String,
    pub thumb: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Detail {
    pub id: i32,
    pub title: String,
    pub english: String,
    pub japanese: String,
    pub pretty: String,
    pub tags: Vec<KnownTag>,
    pub language: String,
    pub uploaded: i64,
    pub favorites: i32,
    pub pages: Vec<PageSrc>,
    pub cover_src: String,
    pub thumb_src: String,
    pub related: Vec<Summary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum Cached {
    Allowed(Box<Detail>),
    /// Only the fact: no title, tags or URLs of a blocked gallery are kept.
    Dropped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListPage {
    pub items: Vec<Summary>,
    pub page: i32,
    pub num_pages: i32,
}

#[derive(Debug, Clone, Default)]
pub struct SearchParams {
    pub query: String,
    pub page: i32,
    pub sort: SortType,
    pub include: Vec<i32>,
    pub exclude: Vec<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Thumb,
    Cover,
    Page(usize),
    PageThumb(usize),
}

impl ImageKind {
    pub fn name(self) -> String {
        match self {
            ImageKind::Thumb => "thumb".into(),
            ImageKind::Cover => "cover".into(),
            ImageKind::Page(i) => format!("page-{i}"),
            ImageKind::PageThumb(i) => format!("pthumb-{i}"),
        }
    }
}

pub fn parse_sort(s: &str) -> SortType {
    match s {
        "popular" => SortType::PopularAllTime,
        "popular-week" => SortType::PopularWeekly,
        "popular-today" => SortType::PopularDaily,
        "popular-month" => SortType::PopularMonth,
        _ => SortType::RecentAllTime,
    }
}

fn language_name(l: Language) -> &'static str {
    match l {
        Language::English => "english",
        Language::Chinese => "chinese",
        Language::Japanese => "japanese",
        Language::All => "all",
        Language::Unknown => "unknown",
    }
}

fn vet_summary(s: &Summary) -> Verdict {
    let names: Vec<String> = s.tags.iter().map(|t| t.name.clone()).collect();
    safety::vet(
        Some(&names),
        std::iter::once(s.title.as_str()).chain(s.alt_titles.iter().map(String::as_str)),
    )
}

fn vet_detail(d: &Detail) -> Verdict {
    let names: Vec<String> = d.tags.iter().map(|t| t.name.clone()).collect();
    safety::vet(
        Some(&names),
        [
            d.title.as_str(),
            d.english.as_str(),
            d.japanese.as_str(),
            d.pretty.as_str(),
        ],
    )
}

/// Tag ids of a raw v2 list/related item, learning full tag objects on the way.
/// `None` when the item carries no tag data at all.
fn raw_tag_ids(item: &Value, tags: &TagCache) -> Option<Vec<i32>> {
    if let Some(arr) = item.get("tag_ids").and_then(Value::as_array) {
        return arr.iter().map(|v| v.as_i64().map(|i| i as i32)).collect();
    }
    if let Some(arr) = item.get("tags").and_then(Value::as_array) {
        let parsed: Option<Vec<Tag>> = arr.iter().map(Tag::from_json).collect();
        let parsed = parsed?;
        tags.learn(parsed.iter());
        return Some(parsed.iter().map(|t| t.id).collect());
    }
    None
}

fn raw_titles(item: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for k in ["english_title", "japanese_title"] {
        if let Some(s) = item.get(k).and_then(Value::as_str) {
            out.push(s.to_string());
        }
    }
    if let Some(t) = item.get("title").and_then(Value::as_object) {
        for k in ["english", "japanese", "pretty"] {
            if let Some(s) = t.get(k).and_then(Value::as_str) {
                out.push(s.to_string());
            }
        }
    }
    out
}

pub struct Service {
    up: Upstream,
    pub tags: TagCache,
    cache: DiskCache,
    seen: RwLock<HashMap<i32, Summary>>,
}

enum ItemOutcome {
    Allowed(Summary),
    Unresolved(i32),
    Dropped,
}

impl Service {
    pub fn new(up: Upstream, tags: TagCache, cache: DiskCache) -> Service {
        Service {
            up,
            tags,
            cache,
            seen: RwLock::new(HashMap::new()),
        }
    }

    fn host(&self) -> &str {
        self.up.host()
    }

    fn remember(&self, s: &Summary) {
        let mut seen = self.seen.write().unwrap();
        if seen.len() >= SEEN_MAX {
            seen.clear();
        }
        seen.insert(s.id, s.clone());
        drop(seen);
        self.cache.put(&format!("v-{}", s.id), s);
    }

    /// Drop everything stored about a gallery and remember only that it is dropped.
    fn purge(&self, id: i32) {
        self.seen.write().unwrap().remove(&id);
        self.cache.remove(&format!("v-{id}"));
        self.cache.purge_images(id);
        self.cache.put(&format!("g-{id}"), &Cached::Dropped);
    }

    fn list_item(&self, item: &Value) -> ItemOutcome {
        let Some(g) = SimpleGallery::from_v2_list_item(item, self.host(), &self.tags) else {
            return ItemOutcome::Dropped;
        };
        if !g.is_valid() {
            return ItemOutcome::Dropped;
        }
        let Some(ids) = raw_tag_ids(item, &self.tags) else {
            // No tag data: only a detail fetch can tell.
            return ItemOutcome::Unresolved(g.id);
        };
        let Some(known) = self.tags.resolve(&ids) else {
            return ItemOutcome::Unresolved(g.id);
        };
        let summary = Summary {
            id: g.id,
            title: g.title.clone(),
            pages: item.get("num_pages").and_then(Value::as_i64).unwrap_or(0) as i32,
            language: language_name(g.language).into(),
            tags: known,
            thumb_src: g.thumbnail.clone(),
            alt_titles: raw_titles(item),
        };
        if vet_summary(&summary).allowed() {
            ItemOutcome::Allowed(summary)
        } else {
            ItemOutcome::Dropped
        }
    }

    pub async fn search(&self, p: &SearchParams) -> Result<ListPage, ServiceError> {
        let page = p.page.max(1);
        let query = p.query.trim();
        if query.chars().count() > 200 {
            return Err(ServiceError::BadRequest("query too long"));
        }
        let empty = ListPage {
            items: vec![],
            page,
            num_pages: 1,
        };
        // A search for blocked terms is answered empty without contacting the site.
        if safety::tag_is_hard(query) || safety::title_blocked(query) {
            return Ok(empty);
        }
        let mut tags: Vec<Tag> = Vec::new();
        for (ids, status) in [
            (&p.include, TagStatus::Accepted),
            (&p.exclude, TagStatus::Avoided),
        ] {
            for id in ids {
                let t = self
                    .tags
                    .get(*id)
                    .ok_or(ServiceError::BadRequest("unknown tag"))?;
                if status == TagStatus::Accepted && safety::tag_hidden(&t.name) {
                    return Ok(empty);
                }
                tags.push(t.to_core(status));
            }
        }
        for (i, name) in safety::UPSTREAM_EXCLUDE.iter().enumerate() {
            tags.push(Tag::new(
                *name,
                0,
                -1000 - i as i32,
                TagType::Tag,
                TagStatus::Avoided,
            ));
        }
        let url = create_url(
            ApiRequestType::BySearch,
            Some(query),
            page,
            0,
            &tags,
            None,
            Some(p.sort),
            self.host(),
        );
        let key = format!(
            "s-{}",
            hex::encode(<sha2::Sha256 as sha2::Digest>::digest(url.as_bytes()))
        );
        if let Some(cached) = self.cache.get::<ListPage>(&key, SEARCH_TTL) {
            return Ok(self.revet_page(cached));
        }
        let body = self.up.json(&url).await?;
        let json: Value = serde_json::from_str(&body).map_err(|_| ServiceError::Parse)?;
        let results = json
            .get("result")
            .and_then(Value::as_array)
            .ok_or(ServiceError::Parse)?;
        let num_pages = json
            .get("num_pages")
            .and_then(Value::as_i64)
            .map(|v| v as i32)
            .unwrap_or(page);
        let mut items = Vec::new();
        let mut budget = RESOLVE_BUDGET;
        for item in results {
            match self.list_item(item) {
                ItemOutcome::Allowed(s) => items.push(s),
                ItemOutcome::Dropped => {}
                ItemOutcome::Unresolved(id) => {
                    if budget == 0 {
                        continue; // dropped, not shown
                    }
                    budget -= 1;
                    if let Ok(d) = self.gallery(id).await {
                        items.push(summary_of(&d));
                    }
                }
            }
        }
        for s in &items {
            self.remember(s);
        }
        let out = ListPage {
            items,
            page,
            num_pages,
        };
        self.cache.put(&key, &out);
        Ok(out)
    }

    /// Cached pages are vetted again with today's lists.
    fn revet_page(&self, mut p: ListPage) -> ListPage {
        p.items.retain(|s| {
            let ok = vet_summary(s).allowed();
            if !ok {
                self.purge(s.id);
            }
            ok
        });
        p
    }

    /// A vetted gallery, or `NotFound` (missing upstream, or dropped by the filter: the two are
    /// indistinguishable to a client on purpose).
    pub async fn gallery(&self, id: i32) -> Result<Detail, ServiceError> {
        if id <= 0 {
            return Err(ServiceError::NotFound);
        }
        let key = format!("g-{id}");
        match self.cache.get::<Cached>(&key, BLOCKED_TTL) {
            Some(Cached::Dropped) => return Err(ServiceError::NotFound),
            Some(Cached::Allowed(mut d))
                if self.cache.get::<Cached>(&key, DETAIL_TTL).is_some() =>
            {
                if vet_detail(&d).allowed() {
                    d.related.retain(|s| self.still_allowed(s));
                    return Ok(*d);
                }
                self.purge(id);
                return Err(ServiceError::NotFound);
            }
            _ => {}
        }
        let body = match self.up.json(&detail_url(id, self.host())).await {
            Ok(b) => b,
            Err(UpstreamError::Status(404)) => return Err(ServiceError::NotFound),
            Err(e) => return Err(e.into()),
        };
        self.detail_from_body(&body).await
    }

    async fn detail_from_body(&self, body: &str) -> Result<Detail, ServiceError> {
        let json: Value = serde_json::from_str(body).map_err(|_| ServiceError::Parse)?;
        let parsed = match parse_single_response(body, self.host(), &self.tags) {
            Ok(SingleParseOutcome::Gallery(g)) => g,
            Ok(SingleParseOutcome::Error) => return Err(ServiceError::NotFound),
            Ok(SingleParseOutcome::NeedsDetailFetch { .. }) | Err(_) => {
                return Err(ServiceError::Parse)
            }
        };
        let data = &parsed.data;
        let id = data.id;
        if id <= 0 {
            return Err(ServiceError::Parse);
        }
        let all: Vec<&Tag> = data.tags.all_tags_list();
        self.tags.learn(all.iter().copied());
        let tags: Vec<KnownTag> = all
            .iter()
            .map(|t| KnownTag {
                id: t.id,
                name: t.name.clone(),
                kind: t.type_.single().into(),
                count: t.count,
            })
            .collect();
        let mut related = Vec::new();
        if let Some(arr) = json.get("related").and_then(Value::as_array) {
            for item in arr {
                if let ItemOutcome::Allowed(s) = self.list_item(item) {
                    related.push(s);
                }
            }
        }
        let title = nclientv3_core::Gallery {
            data: data.clone(),
            related: vec![],
            language: parsed.language,
            online_favorite: false,
            max_size: parsed.max_size,
            min_size: parsed.min_size,
        }
        .get_title_with_pref(TitleType::Pretty);
        let d = Detail {
            id,
            title,
            english: data.get_title(TitleType::English).into(),
            japanese: data.get_title(TitleType::Japanese).into(),
            pretty: data.get_title(TitleType::Pretty).into(),
            tags,
            language: language_name(parsed.language).into(),
            uploaded: data.upload_date_millis / 1000,
            favorites: data.favorite_count,
            pages: data
                .pages
                .iter()
                .map(|p| PageSrc {
                    width: p.size.width,
                    height: p.size.height,
                    src: p.path.clone(),
                    thumb: p.thumb_path.clone(),
                })
                .collect(),
            cover_src: data.cover.path.clone(),
            thumb_src: data.thumbnail.path.clone(),
            related,
        };
        if !vet_detail(&d).allowed() {
            self.purge(id);
            return Err(ServiceError::NotFound);
        }
        self.cache
            .put(&format!("g-{id}"), &Cached::Allowed(Box::new(d.clone())));
        self.remember(&summary_of(&d));
        for s in &d.related {
            self.remember(s);
        }
        Ok(d)
    }

    pub async fn random(&self) -> Result<Detail, ServiceError> {
        let url = create_url(
            ApiRequestType::Random,
            None,
            1,
            0,
            &[],
            None,
            None,
            self.host(),
        );
        for _ in 0..RANDOM_TRIES {
            let body = self.up.json(&url).await?;
            match parse_single_response(&body, self.host(), &self.tags) {
                Ok(SingleParseOutcome::NeedsDetailFetch { gallery_id }) => {
                    if let Ok(d) = self.gallery(gallery_id).await {
                        return Ok(d);
                    }
                }
                Ok(SingleParseOutcome::Gallery(_)) => {
                    if let Ok(d) = self.detail_from_body(&body).await {
                        return Ok(d);
                    }
                }
                _ => {}
            }
        }
        Err(ServiceError::NotFound)
    }

    /// The vetted summary of a gallery for its grid thumbnail, without a detail fetch when a
    /// list already vetted it.
    async fn vetted_summary(&self, id: i32) -> Result<Summary, ServiceError> {
        let hit = self.seen.read().unwrap().get(&id).cloned();
        let hit = hit.or_else(|| self.cache.get::<Summary>(&format!("v-{id}"), DETAIL_TTL));
        if let Some(s) = hit {
            if vet_summary(&s).allowed() {
                return Ok(s);
            }
            self.purge(id);
            return Err(ServiceError::NotFound);
        }
        self.gallery(id).await.map(|d| summary_of(&d))
    }

    /// An image of a vetted gallery: from the disk cache, else fetched and then cached. The
    /// gallery is vetted on every call, cached or not.
    pub async fn image(&self, id: i32, kind: ImageKind) -> Result<(Vec<u8>, String), ServiceError> {
        let src = match kind {
            ImageKind::Thumb => self.vetted_summary(id).await?.thumb_src,
            other => {
                let d = self.gallery(id).await?;
                match other {
                    ImageKind::Cover => d.cover_src,
                    ImageKind::Page(i) => d.pages.get(i).ok_or(ServiceError::NotFound)?.src.clone(),
                    ImageKind::PageThumb(i) => {
                        let p = d.pages.get(i).ok_or(ServiceError::NotFound)?;
                        p.thumb.clone().unwrap_or_else(|| p.src.clone())
                    }
                    ImageKind::Thumb => unreachable!(),
                }
            }
        };
        let name = kind.name();
        if let Some(hit) = self.cache.image(id, &name) {
            return Ok(hit);
        }
        if src.is_empty() {
            return Err(ServiceError::NotFound);
        }
        let (bytes, ctype) = self.up.image(&src).await?;
        let ctype = ctype
            .filter(|c| c.starts_with("image/"))
            .unwrap_or_else(|| guess_type(&src).into());
        self.cache.put_image(id, &name, &bytes, &ctype);
        Ok((bytes, ctype))
    }

    /// Re-vet a stored summary (favorites) against today's lists.
    pub fn still_allowed(&self, s: &Summary) -> bool {
        let ok = vet_summary(s).allowed();
        if !ok {
            self.purge(s.id);
        }
        ok
    }
}

pub fn summary_of(d: &Detail) -> Summary {
    Summary {
        id: d.id,
        title: d.title.clone(),
        pages: d.pages.len() as i32,
        language: d.language.clone(),
        tags: d.tags.clone(),
        thumb_src: d.thumb_src.clone(),
        alt_titles: vec![d.english.clone(), d.japanese.clone(), d.pretty.clone()],
    }
}

fn guess_type(src: &str) -> &'static str {
    let s = src.to_ascii_lowercase();
    if s.ends_with(".png") {
        "image/png"
    } else if s.ends_with(".gif") {
        "image/gif"
    } else if s.ends_with(".webp") {
        "image/webp"
    } else {
        "image/jpeg"
    }
}
